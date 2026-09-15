// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **THE RIBBON** — the laid rainbow the owner's design keeps unchanged, plus
//! the two refinements v2 adds and the per-row field index every other
//! producer reads.
//!
//! Design of record: `RAINBOW-KITTY-V2.md` §4 (the ribbon, kept), §4.1 (the
//! hot edge), §4.2 (no mark inside the band), §2.3 C2 (the ribbon's `t` IS the
//! laid field), §3.2 (the luminance hierarchy), §3.3 (the light fork), §3.4
//! (requests vs the ledger), §18 (the O(1) field index and the quad budget).
//!
//! ## What is carried verbatim in law
//!
//! The tall body (`TALL_UP_CH 1.10 ch`, spine at the row bottom, `dn`
//! `0.25 → 0.305 ch`), the classic walk (`d/16` for the first
//! [`WALK_FAST_CELLS`] cells, then [`WALK_LAY_RATE`] per cell), one
//! `aterm_render::ribbon_beam` call over `RibbonVertex`s with the C¹
//! `ribbon_profile` (core share 0.5) and its Bayer dither, the flat body
//! (head floor 1.0, crest gain 0.0), the 0.75-cycle wave (at
//! `0.110 ch · disp` since 2026-09-08 — twice v1's `0.055`, the owner's
//! "bigger"; [`WAVE_AMP_CELLS`]), the 18 ms `edge-in` and the exact-zero
//! expiry melt, the four-letter
//! guarantee and its chain law, the staggered backspace retract, the wrap
//! fold, the exit swoosh (`0.75 grace + 3 × 0.05 reach + 0.40 retract +
//! 0.24 fade`), and the source-over twin on light themes.
//!
//! ## What is genuinely new, and why
//!
//! 1. **The hot edge** (§4.1) — a 1-px additive hairline IN THE SPECTRUM
//!    along the ribbon's TOP edge, from the head cell back [`HOT_EDGE_CELLS`]
//!    cells: each vertex carries the stop under it, lifted toward white to
//!    [`HOT_EDGE_LUMA_FLOOR`] ([`hot_edge_ink`]) so it out-shines any bed it
//!    can ride, at [`HOT_EDGE_COV_MAX`] 118 — the transient cap. It was a
//!    `#FFFFFF` hairline at 38 — "a third white idiom" — until 2026-09-08,
//!    when the owner struck the
//!    restraint ("I feel like you are diminishing the specialness and
//!    emphasis of this theme? why?"). Drawn as a `comet_beam` hairline
//!    sampled at the same `spine − up` the body used (the top edge carries
//!    the wave, so three axis-aligned rects cannot follow it). Dark themes
//!    only, into `out`, SPATIAL rather than temporal so it can never lag the
//!    head; its gain is priced at the head cell's birth and scaled by the
//!    head cell's envelope ([`Ribbon::env_of`]), so it goes out WITH the
//!    body — ember, melt and swoosh — and never holds over a spent one. It is
//!    the first item on the owner A/B sheet (§22), so it lives behind one
//!    gate — [`Ribbon::hot_edge_gain`] — that is one line to flip off.
//! 2. **The field index** (§18) — a per-row `col → t` lane built ONCE per tick
//!    by [`Ribbon::plan`] and answering [`Ribbon::field_at`] in O(1). v1's
//!    `rainbow_field_at` was a LINEAR SCAN of every spark, called per station
//!    per frame: a measured ~1.7 M spark visits on one jump frame. The meteor,
//!    the caret and stardust all read this index instead.
//! 3. **No mark inside the band** (§4.2) — the ribbon publishes its [`Band`]
//!    (`top` / `spine` / `bottom`) so stardust can be born in the SKY (§5.4)
//!    instead of on a stroke. v1's in-cell starfield placement is deleted.
//!
//! ## The two measured defects this module is written against
//!
//! * **"The body reads dim and muddy"** — a mid-row peak of 100–130/255 with
//!   olive and navy mids, and only the bottom 2–3 px bright. Three parts, and
//!   this module answers TWO of them; it does not claim the third. The NAVY
//!   mids were v1's dark stops (blue, indigo, violet) composited as smudges
//!   below the ground's own reach; [`bed_ink`] lifts them until they sit on
//!   the same composited luminance as every other stop — through their own
//!   hue at full value first, and toward white only for the light a hue
//!   cannot carry ([`onto_luma`], 2026-09-09; below) — so the arc is EVEN —
//!   indigo weighs what red weighs — and the 33-entry
//!   per-position coverage table v1 priced that against is not carried
//!   (§19.1: *"two ceilings for one bed"*). The bottom-only brightness was
//!   the baseline strip out-shining a body that had been capped below it; the
//!   strip is now priced out of the SAME ceiling the body is
//!   ([`BODY_FRAME_TOP`] − `cov`, ~15 levels on a hot mark) instead of beside
//!   it, so it is a crisp accent on a bright body rather than the only lit
//!   thing in the mark. The OLIVE mids are NOT fixed here. The "one
//!   composited luminance" is L3's 5.25:1 bar against the theme's foreground
//!   — `Y ≈ 0.083` on the default theme — and a warm stop at that luminance
//!   IS an olive: a full-coverage yellow composites at `(80, 80, 3)`, max
//!   channel 80, at or below the ≈ 84 v1's table produced. The warm mids sit
//!   at the bar's ceiling BY CONSTRUCTION — and since 2026-09-08 AT the bar
//!   rather than a guard under it: the owner kept the bar as the one
//!   restraint ("text under the bed stays legible") and struck every other
//!   one ("I feel like you are diminishing the specialness and emphasis of
//!   this theme? why?"), so the 0.15 guard became the 0.05 the composite's
//!   rounding actually needs ([`BODY_CONTRAST_GUARD`]), the solver answers
//!   from under its target ([`solve_for_luma`]), and the luminance clamp
//!   became the bar's own answer at a white foreground ([`BED_LUMA_MAX`]
//!   0.150, from 0.100 — which had bound on every foreground brighter than
//!   ≈ `#D8D8D8`). On the default theme that is one level (79 → 80); on a
//!   white foreground it is sixteen (87 → 103). Relaxing the bar for the bed
//!   over BLANK cells stays REJECTED (below). The number is pinned by
//!   `the_dimmest_stop_of_the_bed_composites_at_a_max_channel_of_about_80_under_the_bar`,
//!   so "dim" has a figure to move when a ruling lands.
//! * **"There are GAPS, BLACK GAPS in the rainbow a few characters back
//!   from the cursor"** (the owner, 2026-09-08, on Nord). Diagnosed twice
//!   on glass — the shipped v0.76.0 and m15's main — with the same
//!   instruments (`blackgaps/`, `blackgaps-main/`): NOT holes. Every "gap"
//!   cell held the SAME relative luminance as the vivid cells beside it
//!   (`Y 0.062–0.083`, ≈ 1.9 × the ground); what read as black was CHROMA.
//!   Two stops lost it two different ways. Indigo `#4B0082` was walked
//!   TOWARD WHITE onto the bar and arrived as `(82, 60, 136)` composited —
//!   `S 0.61`, a grey lavender next to a blue at 210–230 — and the
//!   green→blue crossing carried the arc's authored `S 0.53`
//!   ([`crate::spectrum::SPECTRUM_CROSSING_ROOF`], sized for the cyan
//!   census the owner retired on 2026-09-01) and composited as
//!   `(45, 92, 93)`, the greyest cell on the line. The owner's standing
//!   rulings cover the class — "bright not dim", and "the anti-cyan laws
//!   were what greyed the arc" — so the recipe now spends CHROMA before it
//!   spends white: [`onto_luma`] takes a dark stop up through its own hue to
//!   full value and only then toward white (indigo on Nord: `(127, 0, 219)`,
//!   `S 0.98`, the same `Y 0.097`, the same 5.3:1 under the text), and
//!   [`BED_SAT_FLOOR`] gives the crossing its neighbours' chroma back at the
//!   bed's own read (`(3, 95, 95)`). The luminance budget itself is still
//!   the bar's alone: a floor against the GROUND was tried here and taken
//!   out again, because wherever it exceeds the bar it lifts the bed over
//!   the luminance the text can bear (One Dark's `#ABB2BF` over `#282C34`
//!   went 5.29:1 → 4.69:1), and that trade is the owner's, not this
//!   module's — `the_warm_stops_frontier_table` prints what each theme's
//!   bed reads at over its own ground beside what a lift would cost. What
//!   this does NOT do: brighten yellow, green or the crossing's green half.
//!   At a fixed
//!   `Y` the sRGB transfer caps their peak channel (yellow `(90, 90, 0)`,
//!   green `(0, 102, 0)`, cyan `(0, 98, 97)` at Nord's `0.097`), and they
//!   are already at full chroma — the only lever left is the bar itself,
//!   and that is the owner's ruling, not this module's;
//!   `the_warm_stops_frontier_table` prints what each step up would cost.
//!   Pinned by `the_bed_s_dark_stops_arrive_at_full_chroma_not_walked_grey`,
//!   `the_crossing_composites_no_greyer_than_its_flanks`,
//!   `the_ground_never_lifts_the_bed_over_the_bar` and
//!   `the_visibility_floor_costs_the_bar_nothing_on_nord`.
//!
//! * **"It keeps BRIGHTENING for ~660 ms after the last key"** — light with no
//!   keystroke behind it, which is the anti-stray law's own complaint. The
//!   cause is a body whose coverage re-read the LIVE spine every frame while
//!   the follower was still climbing. v2's law: **a cell's light is priced
//!   ONCE, at birth, from [`super::spine::Spine::birth_disp`]**
//!   ([`Cell::cov0`]). The live spine may still move the body's SHAPE — the
//!   wave amplitude and the wedge follow the follower, which keeps climbing
//!   for ~100 ms after the last key and then settles — but it may never move
//!   its LIGHT: the body's coverage and the hot edge's gain are both priced
//!   from the head cell's own [`Cell::birth_disp`], and both are then closed
//!   by the same envelope ([`Ribbon::env_of`]) — the hairline has no fade of
//!   its own to get out of step with the body's. Pinned by
//!   `the_body_never_brightens_after_the_last_keystroke`, on the body's
//!   request AND on the additive light the hot edge actually emits; the
//!   "goes out with its body" half is pinned on the swoosh by
//!   `the_exit_swoosh_retracts_toward_the_caret_and_reaches_exactly_zero`
//!   and on the ember by `the_hot_edge_embers_out_with_its_body_on_focus_loss`.
//!
//! * **"The ribbon under a line I am still typing is BROKEN INTO PIECES"**
//!   (the owner's screenshot, 2026-09-08 — "rainbow" lit, "theme" dark,
//!   "truly" half lit, "magical" lit, "and" dark, "specai" lit at the caret).
//!   Traced on the shipped binary under real Claude Code and under the paint
//!   probe's fake-claude shape (771 and 624 planned ticks): every echo was
//!   licensed, no cell was re-laid or retired by the TUI's redraw, and the
//!   engine's cell set was contiguous on every tick — the darkness was the
//!   EXPIRY. A cell's life was priced once at birth from the spine
//!   ([`Ribbon::cell_life`]: 1.70 s for the first five keys of a take,
//!   4.55 s once the spine was hot) and ran on the cell's own clock, so the
//!   oldest cells died under a hand that had not lifted (the take's "wha"
//!   went dark 1.9 s into a line still being typed), and a word typed at a
//!   dip — after a thinking pause, or slowly — was born with a SHORTER life
//!   than the hotter word before it (`birth_disp` 0.8·peak after a 700 ms
//!   pause: 3.1 s against 4.4 s at peak 0.75) and died first, a dark word
//!   inside the live span. v2's law now: **cells of a cohort share the
//!   cohort's clock** — a cell's expiry is measured from
//!   `max(cell.born, cohort.alive_at)` ([`live_since`]), and every typing
//!   key refreshes `alive_at` on every un-abandoned cohort, so a mark only
//!   ages once the hand stops, and then the exit swoosh retracts it from the
//!   tail exactly as before (off-glass [`SWOOSH_TOTAL_S`] after the last
//!   key, idle → zero unchanged). This REVERSES the 2026-09-06 ruling that
//!   v1's "while the rhythm lives the whole mark lives" was not a v2 gap
//!   (`RAINBOW-KITTY-V2.md` §23's addendum). Pinned by
//!   `a_line_still_being_typed_keeps_every_cell_from_its_first_key_to_the_caret`
//!   here and `a_line_still_being_typed_has_no_dark_cell_inside_its_live_span`
//!   at the seam.
//!
//! ## The ledger, and the one ceiling this producer applies
//!
//! §3.4: *requests are clipped by the ledger, never by ad-hoc caps.* This
//! module applies exactly ONE ceiling of its own — the bed's published
//! [`UNDER_COV_CAP`], with the strip's accent inside [`BODY_FRAME_TOP`] — and
//! the host's `spend_rainbow_budget` does the rest over probed glyph cells.
//! The per-position table v1 carried beside that cap is not reproduced;
//! [`bed_ink`] is where the arc's per-hue light is equalized, and it is
//! equalized in the coordinate the 5.25:1 bar is actually written in.
//!
//! **Why the bed ink is not v1's `rainbow_bed_ink` byte for byte**, although
//! §3.1 says "kept verbatim": v1's recipe (`rainbow_bed_glass` + the hue
//! give-back) leaves a bright hue such as yellow at `#FFFF00` and reaches
//! L3's 5.25:1 bar through the 33-entry COVERAGE table (yellow's entries sit
//! at 55–65 of 212, so yellow composited at a max channel of ≈ 84 over the
//! default ground — that is the measured "olive mid"). §19.1 deletes that
//! table, and under the bar the composited bed luminance is bounded at
//! `Y ≤ 0.081` for EVERY hue against the default foreground, so a bed with one
//! coverage ceiling has to carry the bar in its ink. [`bed_ink`] composites
//! yellow at the same luminance v1's table did (`Y ≈ 0.080` vs `≈ 0.086`)
//! with more chroma — which is to say it equalizes the arc AT L3's ceiling
//! and leaves the warm mids where v1 left them, ≈ 80 max channel composited.
//! v1's ink at a flat 236 would composite yellow at `(243, 243, 33)` —
//! ≈ 1.25:1 — and a "max channel ≥ 120 at yellow" floor is arithmetically
//! incompatible with 5.25:1 (`(120, 120, ·)` is 3.1:1). Any brighter warm
//! mid is therefore a ruling on the bar, not a recipe — and that ruling was
//! made on 2026-09-06 under the owner's delegation of taste: **THE BAR
//! STANDS, over every cell.** L3's 5.25:1 / luma-72 ceiling (yellow at
//! `(78, 78, 2)`, olive by construction) is kept, and the alternative on the
//! table — the bar relaxed to ≈ 3:1 for the bed over BLANK cells with the
//! ledger holding 5.25:1 over probed glyph cells (§3.4) — is REJECTED: the
//! ribbon lies under text that arrives cell by cell, so a bed whose
//! brightness depended on whether a glyph is there would dim each cell the
//! moment its letter lands and flicker along the line as a word is typed,
//! the opposite of the "light on the frame the caret lands" law. The measured
//! dim-and-muddy complaint was closed on the other axis — the cold key's
//! share of the ceiling (`BODY_COLD_SHARE` 0.88) and the hot edge from the
//! second key (`HOT_EDGE_DISP_MIN` 0.15), both under the same bar (§23) —
//! and on 2026-09-08 the bar was taken to its edge (the guard, the clamp,
//! the solver) and everything ABOVE the text was made louder: the hot edge
//! in the spectrum at the transient cap, the wave at twice its swing. The
//! max-channel pin
//! beside `letters_stay_legible_under_the_ribbon_on_the_default_dark_theme`
//! holds the number so a future change to the bar has something to move,
//! and `the_bed_sits_at_the_bar_not_under_it_on_the_default_dark_theme`
//! holds the other side: light left under the bar is diminished light.
//!
//! ## Contract notes (stage 2)
//!
//! Changes to the stage-1 shapes, each flagged rather than smuggled:
//! [`Cell`] gains [`Cell::cov0`] and [`Cell::birth_disp`] (the birth-priced
//! light and the spine it was priced at; without them the "keeps brightening"
//! defect cannot be fixed) and loses its write-only `seed` (stardust deals its
//! own seeds; a field nothing reads is a determinism story that is not
//! wired); [`Cohort`] gains [`Cohort::anchor_col`], the immutable origin the
//! classic walk is a function of, and [`Cohort::abandoned`], because the
//! exit swoosh is the FINGER-LIFT arc and there is one finger — a live key
//! holds every un-abandoned cohort ON ITS OWN ROW (since 2026-09-12 the
//! finger is read per row, [`Ribbon::place`]: a wrapped paragraph's earlier
//! row keeps the life it had when the caret left it and goes out on that
//! clock while the hand types on the next); and [`Ribbon`]'s pool, plan and index are
//! PRIVATE behind read-only accessors, so nothing can mutate the pool behind
//! the index the other producers read. The resident scratch (`runs`,
//! `sorted`, `verts`, `ink`, `ember_at`, `rearm`) is private too, and the
//! index keeps its dark lanes in a spare pool, so the frame path allocates
//! nothing — including on the Enter / wrap that lights a row that was dark.
//! The vertex density is a per-frame number ([`Ribbon::slabs_per_cell`]),
//! because `ribbon_beam` tiles at least one slab per vertex and §18's budget
//! is otherwise unreachable at retina. [`Ribbon::head_rgb`] keeps v1's
//! contract byte for byte: the AUTHORED stop on dark, the rail's ink on
//! light.
//!
//! ## 2026-09-13 — the owner's gaps (`RAINBOW-KITTY-V2.md` §29)
//!
//! The owner: *"Fix these rainbow cursor trail gaps!"*, *"doing backward also
//! seems create some odd effects"*, *"the rainbow cursor streak doesn't
//! follow the new line down in claude code and codex. it's persistent on the
//! screen"*, *"gaps that happened when typing after cooldown"*. Each was
//! measured on the installed app and reproduced headlessly before a law
//! moved; the laws, in the order the code meets them:
//!
//! * **No two typed cohorts abut on a row** ([`Ribbon::heal_seams`]). Two
//!   keys echoed on one tick replay as `Typed, Typed, Sweep, Move` against
//!   the landing caret; the first `Typed` laid one PAST the live cohort's
//!   end, minted a second cohort, and `plan_run` feathered the run split at
//!   [`RUN_TAIL_EASE`] — the 5/4/5 px dark ramp at the last glyph of every
//!   two-finger roll, 14/14 on glass. Abutting typed cohorts are folded into
//!   one before the plan, every frame.
//! * **The brighter side owns a boundary** (`plan_run`). Coverage at a cell
//!   boundary is the brighter cell's; the dimmer cell reaches its own level
//!   inside itself. The midpoint rule sagged the surviving head's outer
//!   third while its erased neighbour spent and POPPED it back on retire,
//!   and dimmed the previous glyph's cell a third for a frame on every key.
//! * **Re-wetting is monotone** ([`Ribbon::place`]). A live cell laid again
//!   keeps its attack age, with a fresh identity for the content witness;
//!   a leaving or out-of-time cell takes a fresh attack too.
//! * **One finger, one row** (`place`, [`Ribbon::leave_row`]). The hold that
//!   kept "a hot paragraph" lit across wraps is per row now: a typed row
//!   change sends the row the hand left into its retract toward the wrap
//!   point, and no key on the new row holds it. Its retract target is per
//!   row ([`Ribbon::advance_swoosh`]): the caret on its own row, its own end
//!   nearest the caret otherwise; the target is the caret block's LEFT
//!   edge ([`Ribbon::retract_x`]).
//! * **The editing hand holds the row** ([`Ribbon::hold_phrase`] — the
//!   phrase WITHIN the row since Rainbow Path v3 §2.3), and an
//!   erase whose echo lands a tick late still retracts from the retreat's
//!   landing (`pending_erase`).
//! * **A composer's re-anchor relays the moved word** ([`Relocate`]): Ink
//!   and Codex move the row's last word down with the wrap key; the word is
//!   laid where the caret reveals it, on both observed shapes, and the
//!   bottom-pinned box that grows UP on the caret's row drains its stale
//!   cells from the landing ([`Ribbon::re_anchor`]).
//! * **The hand came back** ([`Ribbon::join_cohort`]): a program's caret
//!   round trip (a TUI repainting a row below) abandons the band toward the
//!   HAND's column, never the parked cursor, and the hand's next key takes
//!   the band back — no dark word beside a lit one after a cooldown. A
//!   rebirth after a finished swoosh continues the walk (`last_walk`)
//!   instead of restarting at red.
//!
//! What the doc above still says about the fold's hold ("earlier rows stay
//! lit while the hand is on the last one") is superseded by this section.
//!
//! ## 2026-09-14 — the scrub (`RAINBOW-KITTY-V2.md` §31)
//!
//! The owner, on the shipped v0.85.0: *"the painting system for scrubbing
//! back and forth with back word and forward word doesn't work correctly
//! and leaves gaps."* Measured at the host seam and on glass
//! (`docs/measured/scrub-gaps-2026-09-14.md`, `tests/scrub_gaps.rs`): a
//! word hop left the band's clock alone, so the swoosh ran on the last
//! KEY's clock under the scrubbing hand and the band was mid-retract by the
//! seventh hop; a hop of eight cells or more abandoned it outright.
//!
//! * **A scrub holds the band** ([`Ribbon::scrub`]). A same-row keyed move
//!   whose corridor is lit end to end by a band that is not leaving is the
//!   hand moving over its own text: it abandons nothing, takes nothing
//!   over, lays nothing, and renews the crossed cohorts' clock as an erase
//!   does. The band leaves one swoosh after the hand's last act on it.
//!   Discarded for that move: "a real jump on the same row abandons the
//!   band into its swoosh" and "a hop must not touch the word's clock"
//!   (d23b4f935, 4ca68c467). A jump that LEAVES the run — past the band's
//!   oldest cell, off its end onto dark ground, to another row — keeps
//!   both, and a band already inside its retract keeps the leaving-band
//!   clause. Two guards from the fix-up review: a cell already inside its
//!   expiry melt (a hop's cold stub half a second on) is a cell already
//!   going out, so the move over it is the hop it is and nothing snaps
//!   back to full; and the hold reaches exactly the cohorts that OWN the
//!   corridor's columns, never one shadowed under them (an abandoned
//!   band's drained cells under the fresh ones a jump's wake laid in its
//!   holes).
//! * **The scrub predicate IS the boundary with Rainbow Path v3's overlay
//!   wake** (merged 2026-09-14). v3 §2.7 rules that a jump lays its corridor
//!   on an OVERLAY layer along the meteor's flight and abandons the band
//!   beneath it; the scrub rules that the same keystroke lays nothing and
//!   holds the band. They answer different events, and [`Ribbon::scrub`]
//!   says which: inside the hand's own lit run is a scrub, leaving it — past
//!   its oldest cell, off its end onto dark ground, over light already
//!   melting, or to another row — is a jump and takes the overlay. The two
//!   can never disagree about one move, because a move inside the run has no
//!   dark ground for a corridor to light and no band the hand has left to
//!   abandon; and v3's own reason for the overlay, "a leaving band is never
//!   taken over" (R9), does not reach a band the hand never left.
//!
//! ## 2026-09-13 — the comet, the vivid rail and the from-the-hand attack
//! (`RAINBOW-KITTY-V2.md` §30)
//!
//! The owner, the same day: *"a wider rainbow that seems to be painted from
//! the cursor instead of seems to be painting to the screen and I don't see
//! much yellow? that's confusing"* — *"the rainbow pallet doesn't seems to be
//! the FULL ROYGBIV rainbow"* — *"I think you are painting 'cells' with the
//! rainbow, but this solution seems too inflexible"*. Two causes, measured
//! (§29.6): the body was the SAME SHAPE at every cell, with nothing at the
//! caret marking it as the light's origin and a new cell fading in IN PLACE;
//! and [`bed_ink`] puts every stop on the bar's one luminance, where yellow
//! is an olive and orange a brown by the sRGB weights — no bar that leaves
//! white text legible buys a yellow that reads as yellow. What moved, and
//! what did not:
//!
//! * **The comet body** ([`Ribbon::sample`], [`COMET_DN_HAND_CH`],
//!   [`COMET_UP_TAIL_CH`], [`COMET_TAPER_CELLS`]): fattest at the hand — the
//!   reach below the spine opens to 0.55 ch under the caret and settles to
//!   [`DN_FLOOR_CH`] behind it on the wedge's own bloom — and thinner behind
//!   it: the reach above tapers 1.10 → 0.80 ch over twelve cells. The head
//!   cell keeps the full body; the wave and the wedge's settle are as they
//!   were. Shape, not light.
//! * **The vivid rail** ([`Ribbon::emit`], [`rail_ink`], [`RAIL_LUMA_FLOOR`],
//!   [`RAIL_GAIN`]): a SECOND `ribbon_beam` polyline over the body's slabs,
//!   its top at the row bottom and never above it, its reach the body's
//!   `dn`, in the FULL-VALUE spectrum — yellow `(255, 255, 0)`, orange
//!   `(255, 127, 0)`, the dark stops lifted to `Y 0.20` — Over-composited on
//!   the bed at the body's own coverage. Yellow is yellow where there is no
//!   ink; every device row a letter of the typed row can touch is still the
//!   bed, at the bar. Dark themes only. The strip ([`STRIP_LIFT_GAIN`]) is
//!   kept: its half above the spine is the crisp bed-ink accent at the
//!   baseline, its half below lies under the rail.
//! * **The from-the-hand attack** — the SECOND sanctioned attack beside T3's
//!   18 ms edge-in: a typed cell's far slab reaches full at ~`ATTACK_WIPE_S`
//!   (40 ms), never a wake's ([`Ribbon::wipe_of`], [`ATTACK_WIPE_S`],
//!   [`ATTACK_WIPE_FRONT`]): a new cell's light enters as a 40 ms wipe from
//!   the caret side, per slab, on top of the 18 ms edge-in it always had;
//!   the [`BIRTH_EDGE_FLOOR`] is never gated, so T2 holds on the echo frame.
//!   Off under reduced motion.
//! * **The gate** ([`Ribbon::comet`], [`Config::ribbon_flat`], the `… flat`
//!   spelling): the flat body is the owner's A/B control and is byte for
//!   byte what shipped on 2026-09-13 — every branch above collapses to the
//!   old expression when it is off, and the deletion goldens pin it.
//!
//! What did NOT move: C2 — `t` is the laid field, a function of position —
//! and every law of §29 above; the bar, [`bed_ink`] and the hot edge; the
//! walk, the wave, the swoosh; [`RIBBON_QUAD_BUDGET`] (the rail's rows enter
//! [`Ribbon::slabs_for`]'s estimate, so a long retina line gives density
//! before it sheds).

use std::mem;

use aterm_time::Instant;

use aterm_render::{
    BeamVertex, GlowBlend, RibbonVertex, comet_beam, over_premul, premul_rgb, ribbon_beam,
};

use crate::cursor_glow::{InkRole, band_pos, band_row};
use crate::effect_util::lerp_rgb;
use crate::spectrum::{spectrum, spectrum_with_min_saturation};

use super::meteor::tri;
use super::spine::{DISP_RELEASE_TAU, PHASE_RATE};
use super::timing::{
    EDGE_IN_S, FLIGHT_MIN_MS, IOI_DEFAULT_MS, JUMP_MIN_CELLS, REDUCED_MOTION_FADE_MS, clamp01,
    edge_in, flight_ms, ioi_next, kill_span_s, phrase_rest_ms, smoothstep01, spend, suck_in,
};
use super::{Cadence, Config, Ctx, Event, Frame, Licence, TypedClass, level_step_spend};

// ===========================================================================
// The budget, the walk, and the body's geometry
// ===========================================================================

/// Quads the ribbon may spend on one frame (§18's `RAINBOW_RIBBON_QUAD_BUDGET`
/// — a 76-cell hot line at retina). Shed from the TAIL, never from the head:
/// the head cell is the per-key light and losing it would break the coupling
/// contract (§7.3, "one key, one cell, one light, one note").
pub const RIBBON_QUAD_BUDGET: usize = 10_240;

/// Cells the hot edge reaches back from the head (§4.1).
pub const HOT_EDGE_CELLS: f32 = 3.0;

/// **THE SURGE'S CEILING** — the furthest back the crisp edge may reach, in
/// cells, when a key is born on a CHANGE OF SPEED or under an open theme
/// (§23's addendum "Flow state", 2026-09-09; the design's
/// `edge_cells = 3 + 4·Δ/0.5` capped at 5). Five cells at `cw 15` is 75 px of
/// hairline — two vertices more than the base reach, and the same alpha law
/// over a longer `d`, so the edge reads as a longer SPEED streak and never as
/// a brighter one: [`HOT_EDGE_ALPHA`]'s normalised falloff still lands on
/// exact zero at the reach, and [`HOT_EDGE_COV_MAX`] — the transient cap
/// itself — is untouched.
pub const HOT_EDGE_CELLS_MAX: f32 = 5.0;

/// The hot edge's falloff shape, normalised at the head cell:
/// `alpha(d) = 0.38·(1 − d/3)²`, so the request is
/// `HOT_EDGE_COV_MAX · (1 − d/3)²` with `d` in cells behind the head.
pub const HOT_EDGE_ALPHA: f32 = 0.38;

/// The hot edge's coverage request ceiling — THE TRANSIENT CAP itself (§3.2,
/// `RAINBOW_TRANSIENT_COV_CAP` 118: the meteor's shoulder, the pin's
/// nucleus, every transient at the hand), which is what keeps a 1-px additive
/// hairline legibility-safe: the ledger holds it to the ink lift over the
/// row above's probed glyph cells (L2), and it never touches the row it
/// underlines.
///
/// **118, from 38 (2026-09-08, the owner: "I feel like you are diminishing
/// the specialness and emphasis of this theme? why?" — "everything above
/// and around the text may be as loud as you like").** At 38 the white
/// hairline over the ground composited at max 76 / spread 12 — a grey
/// lightening no scanner could call colour (the paint scanner's rule: max
/// ≥ 60 AND spread ≥ 40) — and over its own bed it was 1.7× (blue) to 2.3×
/// (yellow) the bed's luminance. At the cap the lifted stop is colour on
/// every one of the seven (over the ground: red 144 / 74, orange 144 / 106,
/// yellow 145 / 107, green 145 / 119, blue 156 / 68, indigo 129 / 40,
/// violet 145 / 64) and over its bed the head's request is 2.3× (blue) to
/// 7.0× (yellow) the bed's luminance — before the hairline's 1-px
/// anti-aliasing, which lands 30-70 % of a request on its brightest row.
/// Measured on the default theme in
/// `the_hot_edge_is_the_stop_under_it_lifted_hot_and_brighter_than_the_bed`.
pub const HOT_EDGE_COV_MAX: f32 = 118.0;

/// The relative luminance the hot edge's ink is LIFTED to when its stop is
/// darker — twice [`BED_LUMA_MAX`], so the hairline's ink is at least twice
/// as luminous as any bed it can ride, on every theme and every stop, by
/// construction. A stop already brighter than this (orange, yellow, green)
/// is carried pure; red, blue, indigo and violet are lifted to it
/// ([`hot_edge_ink`] through [`onto_luma`]) — through their own hue at full
/// value first, then toward white: the hue stays, the heat is white.
///
/// It is a floor on the INK, not on the composite: the hairline is additive
/// and priced by [`HOT_EDGE_COV_MAX`], so what lands is `ink · cov / 255` on
/// top of the bed — the blue stop's `(135, 135, 255)` at 118 puts
/// `(62, 62, 118)` on a bed whose blue is already saturated, which is the
/// one way a blue hairline can be brighter than a blue bed.
pub const HOT_EDGE_LUMA_FLOOR: f32 = 2.0 * BED_LUMA_MAX;

/// The spine value below which there is no hot edge at all (§4.1): it is the
/// "still wet at the hand" mark, and a cold hand is not wet.
///
/// **0.15, from 0.35 (2026-09-06, the paint-conformance finding in §23).**
/// At 0.35 the crisp edge arrived at key 4-5; a hand is wet from its second
/// key, and the edge is the one mark that reads as SPEED, so it now rises
/// from `disp` 0.15 (key 2 at 8 cps) over the same 0.30 span.
pub const HOT_EDGE_DISP_MIN: f32 = 0.15;

/// Width of the smoothstep that fades the hot edge in over
/// [`HOT_EDGE_DISP_MIN`] — `smoothstep((disp − 0.15)/0.30)`.
pub const HOT_EDGE_DISP_SPAN: f32 = 0.30;

/// **THE HOT EDGE'S GAIN FLOOR** (2026-09-14, the owner's notch): the share
/// of the hairline a COLD hand gets — the first key of a word, momentum
/// zero. Below it the hairline did not exist at all (`hot_edge_gain` was
/// exactly zero under [`HOT_EDGE_DISP_MIN`]), so the one mark that attaches
/// the band's top edge to the caret was absent on precisely the key the eye
/// is on: a word's first letter had a lowered, edgeless head under a full
/// block. The edge's EXISTENCE no longer depends on momentum; only the rest
/// of its gain still ramps with the hand over [`HOT_EDGE_DISP_SPAN`]. Half:
/// at the transient cap's `118 × 0.70` on the owner's intensity that is 41
/// levels of hot ink on the top row against 83 at full momentum — present,
/// and still visibly a speed mark when the hand is hot. A TYPED key's
/// alone: an arrow's wake is no key, and its head keeps the momentum-only
/// gain it always had (`hot_edge_gain`'s `typed`).
pub const HOT_EDGE_GAIN_FLOOR: f32 = 0.5;

/// How far, in `ch`, the hot edge's hairline may sit from the caret's own cell
/// band before it stops being "the mark at the hand" and becomes a highlighter
/// on somebody else's row.
///
/// It is a CONSEQUENCE, not a knob: the hairline rides `spine − up`, which
/// under the tall spelling is `0.10 ch` above the caret cell's top and under
/// the underline spelling is `0.36 ch` below it. `0.55` bounds both with room
/// for the wave (`0.11 ch` at full momentum), and the pin
/// (`the_hot_edge_lives_only_at_the_head_on_a_dark_ground`) is what stops a
/// later geometry change from quietly moving a white line onto another row.
pub const HOT_EDGE_CARET_REACH_CH: f32 = 0.55;

/// Cells of the classic walk's FIRST phase, which advances `d/16` per cell so
/// a short word already shows several stops (§4, v1 verbatim).
pub const WALK_FAST_CELLS: f32 = 16.0;

/// The classic walk's steady lay rate, `t`-units per cell, after
/// [`WALK_FAST_CELLS`]. §6.4's meteor arc CONTINUES this exact rate backwards
/// from the caret's stop — the two numbers are the same number, and a change
/// here is a change to the meteor.
pub const WALK_LAY_RATE: f32 = 1.0 / 36.0;

/// THE BIRTH FLOOR — v1's `RAINBOW_BIRTH_EDGE_FLOOR`, verbatim: a laid cell is
/// READABLE on the exact frame that echoes its key. The 18 ms `edge-in` (§2.5,
/// T3) is the one sanctioned attack, but it ramps from ZERO, and the host can
/// present a keystroke's echo 0.2–0.4 ms after the key: at that age `edge_in`
/// is 0.0008 and the new cell quantises to nothing, so the glyph presents
/// without its colour and the ribbon arrives one panel period later (the live
/// capture of 2026-09-05 measured 6/255 on the echo frame vs 71 at +17 ms, on
/// 4 of 44 keys). v1 had this floor and lit the cell at 75 on that frame; v2's
/// port dropped it. The floor rides ONLY the cell's birth envelope — the
/// focus-regain rearm keeps its unfloored `edge_in` so regain never snaps.
/// Owner law (§2.1 T2): "the frame that acknowledges the key already shows
/// the colour."
pub const BIRTH_EDGE_FLOOR: f32 = 0.35;

/// The shipped `tall` body's reach ABOVE the spine, in `ch` (v1's
/// `RAINBOW_TALL_UP`). D16's whole point: under `tall` the band covers the
/// entire cell, so star birth zones are defined against [`Band::top`] and land
/// in row − 1.
pub const TALL_UP_CH: f32 = 1.10;

/// The tall body's full-strength plateau above the spine, in `ch` (v1's
/// `RAINBOW_TALL_CORE`). Under `aterm_render::RIBBON_CORE_SHARE` of
/// [`TALL_UP_CH`], so the rasterizer's own plateau clamp is a no-op and the
/// melt keeps the whole of the rest of the cell to fall through.
pub const TALL_CORE_CH: f32 = 0.40;

/// The DOWNWARD melt's floor, in `ch` (v1's `RAINBOW_RIBBON_DN_FLOOR`) — the
/// narrowest the transverse falloff below the spine may ever get, at any
/// momentum and any phase of the wave.
pub const DN_FLOOR_CH: f32 = 0.25;

/// The downward reach at full momentum, in `ch` (v1's `RAINBOW_RIBBON_TOP`) —
/// [`DN_FLOOR_CH`] plus exactly [`WAVE_AMP_CELLS`], so the room the wave needs
/// is inside the tile BY CONSTRUCTION and the clamp is a bound on the wave
/// rather than a tax on the profile.
pub const DN_TOP_CH: f32 = DN_FLOOR_CH + WAVE_AMP_CELLS;

/// The UNDERLINE spelling's reach above the spine, in `ch`: the rest of the
/// cell once [`DN_TOP_CH`] is spent below it, so the mark's top edge lands
/// exactly `0.36 ch` under the cell top (`0.305` until the wave doubled on
/// 2026-09-08). An underline however the shoulder is spelled.
pub const UNDERLINE_UP_CH: f32 = 1.0 - DN_TOP_CH;

/// The leading's own half-thickness in `ch` (v1's `RAINBOW_RIBBON_LEAD`,
/// `RAINBOW_UNDERLINE_H / 2`): the underline spelling's plateau, and the
/// baseline strip's span. It is the same number the resting strip used to be
/// the HEIGHT of; it is now the width of a plateau, so what used to cut melts.
pub const LEAD_CH: f32 = 0.11;

/// **THE ONE FORK** (§4, D14's "as a parameter rather than a fork"): the tall
/// highlighter holds the FULL shoulder, so the letters stand inside the light
/// and the profile melts only at the cell edge.
///
/// `aterm_render::ribbon_profile` short-circuits at exactly `1.0`, so this
/// spelling is also the cheap one.
pub const SHOULDER_TALL: f32 = 1.0;

/// …and the underline spelling's quieter glyph-band wash (v1's
/// `RAINBOW_RIBBON_SHOULDER`). It is A POINT ON THE CURVE — the value the
/// profile passes through once the core ends — and there is no second emitter
/// for it to be the gain of. The tall/underline fork flip-flopped six times in
/// five days in v1 precisely because it was spelled as two bodies; here the
/// only difference between the two spellings is [`BodyProfile`].
pub const SHOULDER_UNDERLINE: f32 = 0.55;

/// The 0.75-cycle wave's amplitude in cells, at full spine.
///
/// **0.110, from v1's `RAINBOW_WAVE_AMP_CELLS` 0.055 (2026-09-08, the owner:
/// "make this rainbow theme truly magical and special and dynamic and
/// beautiful").** At 0.055 the live-momentum wave was ≤ 1 px at `ch 18` and
/// 1.5 px at retina — a settle the eye could not read as motion. Doubled,
/// the mark breathes ±2 px (3 px at retina) with speed, still inside the
/// rows the body owns: the head is pinned ([`WAVE_HEAD_PIN`]), the top edge
/// never rises above [`TALL_UP_CH`] and the bottom never falls past
/// [`DN_TOP_CH`], which grows with this number by construction (the wedge's
/// full travel is `0.36 ch` now, was `0.305`). Pinned by
/// `the_wave_breathes_a_tenth_of_a_cell_at_full_momentum_inside_the_body_s_own_rows`
/// (0.99 px at `ch 18` before, 1.98 after).
pub const WAVE_AMP_CELLS: f32 = 0.110;

/// Cycles of the wave across the WHOLE mark, whatever its length (v1's
/// `RAINBOW_WAVE_CYCLES`): three quarters, so the mark undulates once and does
/// not read as a ripple.
pub const WAVE_CYCLES: f32 = 0.75;

/// The share of the mark over which the wave's amplitude smoothsteps up from
/// the head (v1's `RAINBOW_WAVE_HEAD_PIN`). The head is PINNED so the cell
/// under the hand never moves.
pub const WAVE_HEAD_PIN: f32 = 0.35;

/// Cells over which the body's downward wedge closes behind the head. The mark
/// is fatter under the hand and settles into the leading behind it — about a
/// word's worth of swell.
pub const BLOOM_REACH_CELLS: f32 = 8.0;

/// Vertices per cell along the major axis, AT MOST. ONE VERTEX PER SLAB,
/// because `ribbon_beam` interpolates a vertex's COLOUR to its neighbour's in
/// sRGB — a CHORD across the arc — and the arc's fastest leg is the one a
/// chord walks straight back across. Three thirds keep the emitted mark's
/// cyan dwell under the arc's own; the POSITION is what ramps, and the colour
/// is resolved from the arc at every slab.
///
/// It is a CEILING, not the frame's number: `ribbon_beam` tiles at least one
/// slab per segment whatever stride it is handed, so the vertex density is
/// the only knob the quad budget has. [`Ribbon::slabs_per_cell`] is what a
/// frame actually plans — this, or fewer when the live cells × rows would
/// not fit [`RIBBON_QUAD_BUDGET`] at it (a 76-cell hot line at retina is
/// ~12 k quads at three; at one it is ~4 k, and the tail is not cut).
pub const SLABS_PER_CELL: usize = 3;

/// The baseline strip's own gain, as a share of the cell's ink coverage (v1's
/// `RAINBOW_STRIP_GAIN`): the crisp accent under the letters, riding
/// `aterm_render::ribbon_lift_profile` over [`LEAD_CH`] in the leading.
///
/// It is CLAMPED to `255 − cov` by [`Ribbon::plan`], which is why v2's strip
/// can never reproduce the measured "only the bottom 2–3 px bright" defect:
/// the body now rides its own ceiling, so the strip has ~19 levels of headroom
/// to be crisp in and no more.
pub const STRIP_LIFT_GAIN: f32 = 0.35;

// ===========================================================================
// The comet body, the vivid rail and the from-the-hand attack (2026-09-13,
// `RAINBOW-KITTY-V2.md` §30)
// ===========================================================================
//
// THE OWNER, 2026-09-13: "I also want a wider rainbow that seems to be painted
// FROM the cursor instead of seems to be painting TO the screen and I don't
// see much yellow? that's confusing" — "the rainbow pallet doesn't seems to be
// the FULL ROYGBIV rainbow". Three answers, one gate (`Config::ribbon_flat`,
// the `… flat` spelling restores the body below byte for byte):
//
// * THE COMET BODY — the body used to be the same shape at every cell (up
//   1.10 ch at the head and at the tail, dn 0.25 → 0.36 ch), so nothing
//   marked the hand as the ORIGIN of the light. It is fattest at the hand
//   now: the reach below the spine opens to [`COMET_DN_HAND_CH`] under the
//   caret and settles to [`DN_FLOOR_CH`] over [`BLOOM_REACH_CELLS`] behind
//   it, and the reach above tapers from [`TALL_UP_CH`] at the head to
//   [`COMET_UP_TAIL_CH`] over [`COMET_TAPER_CELLS`] — a head and a tail.
// * THE VIVID RAIL — the bed keeps the owner's one restraint (5.25:1 under
//   every letter, [`bed_ink`]), and under that bar yellow composites as an
//   olive `(77, 77, 2)` and orange as a brown; no bar that leaves white text
//   legible buys a yellow that reads as yellow (§29.6). Yellow can only be
//   yellow WHERE THERE IS NO INK: the reach below the row bottom, into the
//   leading. The rail is a second `ribbon_beam` polyline over the body's own
//   slabs — its top at the row bottom, never above it, its reach the body's
//   `dn`, so it is thickest at the hand (the comet's lower lobe) — in the
//   FULL-VALUE spectrum ([`rail_ink`]: every stop at value 255, the dark
//   stops lifted to [`RAIL_LUMA_FLOOR`] toward white), Over-composited on
//   the bed at the body's own coverage ([`RAIL_GAIN`]). Every device row a
//   letter of the typed row can touch stays at the bar; the rail is where
//   the whole ROYGBIV is visible, yellow and orange included. Dark themes
//   only (the light fork's ink has no vivid recipe, L6).
// * THE FROM-THE-HAND ATTACK — a new cell used to fade in IN PLACE over the
//   uniform 18 ms edge-in. Its light now enters as a WIPE from the caret
//   side ([`ATTACK_WIPE_S`], [`Ribbon::wipe_of`]): the slab beside the hand
//   opens first and the far slab last, so the light visibly leaves the
//   cursor. T2 holds: the whole cell is at [`BIRTH_EDGE_FLOOR`] on the echo
//   frame, and the wipe only shapes the ramp above the floor.
//
// C2 is untouched: `t` is still the laid field, a function of the cell's
// position (the head-anchored "red always at the hand" ramp was the v0.60.0
// look the owner rejected). Shape and attack moved; colour law did not.

/// The comet's reach BELOW the spine at the hand, in `ch`, at full momentum
/// — the lower lobe under the caret, which the rail fills with vivid ink.
/// From [`DN_TOP_CH`]'s 0.36: 0.55 ch is 15–18 device px at the owner's
/// retina cell, a lobe the eye reads as the origin of the light. It closes
/// to [`DN_FLOOR_CH`] over [`BLOOM_REACH_CELLS`] behind the head, and with
/// the spine's release ([`Ribbon::settle_step_s`]) when the hand stops —
/// shape follows the live spine, light does not.
pub const COMET_DN_HAND_CH: f32 = 0.55;

/// The tall body's reach ABOVE the spine at the comet's TAIL, in `ch`: the
/// body is [`TALL_UP_CH`] 1.10 at the head and thins to this
/// [`COMET_TAPER_CELLS`] behind it, so the tail is thinner than the head.
/// 0.80 still covers the glyph box's x-height and cap zone behind the hand
/// (the letters stay inside the light); the taper is applied as a SHARE of
/// the profile's `up` (`0.80 / 1.10`), so the underline spelling thins in
/// proportion.
pub const COMET_UP_TAIL_CH: f32 = 0.80;

/// Cells behind the head over which the comet's `up` tapers from
/// [`TALL_UP_CH`] to [`COMET_UP_TAIL_CH`] (smoothstep). Longer than the
/// lower lobe's [`BLOOM_REACH_CELLS`] so the head reads as a head and not as
/// a bulge: the body narrows over about two words.
pub const COMET_TAPER_CELLS: f32 = 12.0;

/// The rail's ink luminance FLOOR, in relative luminance: a stop whose
/// full-value colour is darker than this (blue `Y 0.072`, indigo, violet) is
/// walked toward white until it reaches it ([`rail_ink`] through
/// [`onto_luma`]), so the cold half of the arc reads as vivid on the rail
/// too. Red at full value is `Y 0.213`, so every warm stop is carried PURE —
/// yellow `(255, 255, 0)`, orange `(255, 127, 0)`, green `(0, 255, 0)`.
/// Under [`HOT_EDGE_LUMA_FLOOR`] 0.30 on purpose: the rail is a BED, a wide
/// band the eye rests on, not a transient, and at 0.30 blue is a pale
/// `(135, 135, 255)`; at 0.20 it is `(100, 100, 255)`-ish and still blue.
pub const RAIL_LUMA_FLOOR: f32 = 0.20;

/// **THE RAIL'S CEILING — ITS OWN, BELOW THE BASELINE** (re-ruled 2026-09-14).
/// Until then the rail sat under L5's one non-caret ceiling — the caret's
/// light floor ([`crate::cursor_glow::RAINBOW_CARET_LIGHT_FLOOR`] 80) times the sparkle field's
/// share (`Y 0.282`): yellow at `(149, 149, 0)`, composited `(140, 140, 3)`,
/// a mustard. The owner asked to SEE yellow and ruled "you can raise the
/// light floor". Raising the FLOOR itself (150) did what its doc warned:
/// the caret block paled to a salmon across most of the arc and the star
/// laws that derive their ceiling from the same number went vacuous —
/// seven pins red. So L5 is amended instead of the floor: **the caret is
/// the brightest persistent thing IN THE GLYPH ROWS**; the rail lives below
/// the row bottom, never under a letter, and takes its own ceiling. At
/// `Y 0.54` yellow's ink is `(200, 200, 0)`, composited `≈ (190, 190, 3)` at
/// the bed's cap — a yellow that reads as yellow — and every stop brighter
/// than the ceiling (yellow, green, cyan-ish crossing, orange) is scaled
/// DOWN through its own hue ([`rail_ink`] via [`onto_luma`], hue exact),
/// so the rail reads as ONE weight, the "black gaps" lesson. Pinned by
/// `the_rail_is_yellow_below_the_row_bottom_and_the_glyph_box_stays_at_the_bar`
/// and `the_rail_ink_table`; the caret's own pin
/// (`the_brightest_pixel_in_the_frame_is_under_the_cursor`) now reads the
/// glyph rows, where the caret still wins.
pub const RAIL_LUMA_CEIL: f32 = 0.54;

const _: () = assert!(
    RAIL_LUMA_FLOOR < RAIL_LUMA_CEIL,
    "the rail's floor must sit under its ceiling"
);

/// The rail's coverage as a share of the body's at the same slab. `1.0`: the
/// rail composites at the bed's own [`UNDER_COV_CAP`] ceiling over the bed
/// (the bed shows through at `19/255`), so it goes out WITH the body —
/// edge-in, wipe, melt, retract and swoosh alike — and never over a spent
/// one. Lower it to let more of the bed's own stop show through.
pub const RAIL_GAIN: f32 = 1.0;

/// The rail's full-strength plateau as a share of its reach: the top half
/// holds full value and the bottom half melts to exactly zero
/// (`aterm_render::ribbon_profile`, shoulder 1.0 — the side with no
/// letterforms). It is `RIBBON_CORE_SHARE`, the most the rasterizer allows,
/// so the rail is a bar with a soft underside rather than a smear.
pub const RAIL_CORE_SHARE: f32 = aterm_render::RIBBON_CORE_SHARE;

/// The from-the-hand attack's duration, seconds: a new cell's light crosses
/// the cell from the caret side to the far side in this long. Longer than
/// [`EDGE_IN_S`] 18 ms (the level's own ramp, kept), so the crossing is
/// visible as a MOTION — five frames at 120 Hz, two or three at 60 — and
/// shorter than a key at any human cadence, so no cell is still filling
/// when the next key lands.
pub const ATTACK_WIPE_S: f32 = 0.040;

/// The wipe front's softness, in cells: the front is a smoothstep this wide,
/// so the light pours rather than sweeps. At 0.35 a three-slab cell shows
/// the front on every one of the wipe's frames.
pub const ATTACK_WIPE_FRONT: f32 = 0.35;

/// **HOW FAR BELOW THE ROW BOTTOM THE VIVID RAIL MAY REACH**, in cells —
/// [`DN_TOP_CH`], the reach the flat body proved ink-free (the row below's
/// caps and ascenders start about `0.2 ch` under its cell top; aterm-render's
/// own §4 note). The comet's lower lobe opens deeper than this at the hand
/// ([`COMET_DN_HAND_CH`]); the part of it below this line is BED ink, at the
/// bar, never full-value colour under a letter (review, 2026-09-13).
pub const RAIL_REACH_MAX_CH: f32 = DN_TOP_CH;

// ===========================================================================
// The bed's ONE ceiling (§3.2, §3.4, L3)
// ===========================================================================

/// The bed's published byte ceiling (L3's `RAINBOW_UNDER_COV_CAP 236`). It is
/// the SOURCE-OVER stream's cap, and the ledger's frame top (251) leaves the
/// companions their remainder above it (§3.4).
pub const UNDER_COV_CAP: f32 = 236.0;

/// The coverage at which a planned boundary counts as ON THE GLASS for
/// `trail status` ([`Ribbon::lit_segments`]): half the cap. Derivation: the
/// bed's dimmest stop composites at a brightest channel of ≈ 80 at full
/// coverage (the pin below), and the paint scanner's colour floor is 60 on
/// that channel over a ground near 20 — so the dimmest stop reads as ink from
/// about half coverage up, and a claim made under that would be a claim the
/// pixels cannot honour.
pub const STATUS_LIT_COV: u8 = (UNDER_COV_CAP / 2.0) as u8;

/// **THE PAINT SCANNER'S COLOUR FLOOR**, the pair the glass census reads a
/// pixel as band ink by: brightest channel at or over [`SCAN_INK_MAX`] AND a
/// channel spread at or over [`SCAN_INK_SPREAD`]. [`STATUS_LIT_COV`] is this
/// floor solved ONCE for the arc's dimmest stop and then applied to every
/// stop; [`reads_as_ink`] applies it to each stop's own composite, which is
/// the difference between a bound and a fact.
pub const SCAN_INK_MAX: u32 = 60;

/// …and the spread half of it — see [`SCAN_INK_MAX`].
pub const SCAN_INK_SPREAD: u32 = 40;

/// Whether a COMPOSITED `0x00RRGGBB` pixel reads as band ink to the glass
/// census ([`SCAN_INK_MAX`] / [`SCAN_INK_SPREAD`]) — the predicate
/// [`Ribbon::ink_segments`] answers "is this boundary on the glass" with,
/// per stop, instead of against the whole arc's worst case.
#[must_use]
pub fn reads_as_ink(rgb: u32) -> bool {
    let (r, g, b) = ((rgb >> 16) & 0xff, (rgb >> 8) & 0xff, rgb & 0xff);
    let mx = r.max(g).max(b);
    let mn = r.min(g).min(b);
    mx >= SCAN_INK_MAX && mx - mn >= SCAN_INK_SPREAD
}

/// The LEDGER'S FRAME TOP (§3.4): the level a `glow_under` pixel may reach
/// once every stream on it has been priced. The bed's own request stops at
/// [`UNDER_COV_CAP`]; the 15 levels between the two are the baseline strip's
/// accent, and there is nothing else in `under` on a ribbon row.
///
/// It is safe to spend them because the bed's ceiling is carried by the INK
/// (see [`bed_luma_budget`]): at full opacity the composite IS the ink, whose
/// relative luminance is the bar's own budget, so a brighter ALPHA cannot
/// break a bar that a brighter COLOUR would have.
pub const BODY_FRAME_TOP: f32 = 251.0;

/// The contrast bar every legibility ceiling in the rainbow family is solved
/// against: unlit ink over lit ground, 5.25:1 (L3).
pub const BODY_CONTRAST_BAR: f32 = 5.25;

/// The margin under [`BODY_CONTRAST_BAR`] the emitter's own rounding may not
/// be able to cross — and NO MORE than that, because light left under the
/// bar is diminished light (the owner, 2026-09-08).
///
/// **0.05, from 0.15.** The 0.15 was sized as "~4 levels of composited luma
/// at the bar, more than all three [rounding costs] can spend together";
/// measured, two of the three spend nothing: [`solve_for_luma`] answers from
/// UNDER its target, so the ink's own byte rounding cannot cross the bar,
/// and the LUT's lerp of two on-bar colours is under the bar (the EOTF is
/// convex). What is left is the partial composite's one level of rounding
/// (`premul_rgb` + `over_premul`), and one Bayer level over the ledger's
/// frame top — worth 0.05 of a ratio point at the dimmest theme above the
/// floor (fg `#A0A0A0`, budget 0.026: 5.2019:1 at a guard of 0), less
/// everywhere brighter. Sized by
/// `letters_stay_legible_under_the_ribbon_on_every_dark_theme_above_the_floor`,
/// which is the first step of a `0 / 0.005 / 0.01 / 0.015 / 0.02 / 0.03 /
/// 0.05` sweep that holds the bar on all 67 of its themes.
pub const BODY_CONTRAST_GUARD: f32 = 0.05;

/// Floor on the bed's composited luminance budget. A theme whose foreground is
/// almost black would solve to a bed nobody can see; below this the ribbon
/// stops obeying the bar and simply takes the dimmest light that still reads.
pub const BED_LUMA_MIN: f32 = 0.020;

/// …and the ceiling: the bar's own answer at a WHITE foreground
/// (`(1.0 + 0.05) / 5.25 − 0.05`), which no foreground can exceed — so under
/// the bar this clamp binds nowhere, and the bed on every theme is exactly
/// as bright as its text allows.
///
/// **0.150, from 0.100 (2026-09-08).** The 0.100 was written against L5
/// (the caret is the brightest PERSISTENT thing on glass), but it bound on
/// every foreground brighter than ≈ `#D8D8D8` — the offline renderer's
/// `#E8E8F0` solved to 0.114 and took 0.100, a white foreground to 0.150 and
/// took 0.100: a third of the bar's light, withheld on the brightest themes
/// for no law the text could name. The owner's ruling is that the bar is the
/// one restraint ("text under the bed stays legible") and the bed is not to
/// sit under it. L5 is untouched: the caret block is an opaque authored stop
/// and out-shines a bed at `Y 0.15` on every stop of the arc.
///
/// L3's "field luminance ceiling 72" is this budget read in v1's unit on the
/// default theme, where [`bed_luma_budget`] solves to `Y ≈ 0.083` (the bar
/// solves it, not this clamp) and a warm stop composited at full coverage
/// carries a Rec.709 gamma-luma of ≈ 74/255 (`(80, 80, 3)` at yellow). The
/// two numbers are one ceiling in two coordinates; this module states it in
/// the one the 5.25:1 bar is written in. Pinned by
/// `a_bright_foreground_s_bed_takes_the_bar_s_whole_budget_not_a_tenth`.
pub const BED_LUMA_MAX: f32 = 0.150;

/// **THE BED'S CHROMA FLOOR** — the least HSV saturation a stop of the arc
/// may carry into [`bed_ink`] and [`hot_edge_ink`], applied before the
/// luminance solve ([`crate::spectrum::spectrum_with_min_saturation`]).
///
/// THE OWNER, 2026-09-08: *"there are gaps black gaps in the rainbow a few
/// characters back from the cursor."* Diagnosed twice on glass (v0.76.0 and
/// m15's main): not holes — every "gap" cell held the SAME relative
/// luminance as the vivid cells beside it; what read as black was chroma.
/// This floor is the crossing's half of that. The green→blue crossing is
/// authored at `S 0.53` ([`crate::spectrum::SPECTRUM_CROSSING_ROOF`],
/// tapering in from `0.65` / `0.72` knots) for the cyan true-peak bound of
/// the census the owner retired on 2026-09-01, and once the bed scales it
/// onto the bar it composites as `(45, 92, 93)` — `S 0.52`, the greyest cell
/// on the line, a teal-grey notch between an `S 0.97` green and an `S 0.81`
/// blue. **`1.0`**: the seven anchors all carry `S 1.0` (each has a zero
/// channel) and every table entry outside the crossing's flanks sits at
/// `S ≥ 0.98`, so the floor asks nothing of any stop but the crossing, and
/// there it asks for exactly what its neighbours have. Hue and value are
/// untouched, so the crossing's hue PACING — the thing the roof still owns
/// — is exactly as authored. Measured: the crossing composites `(3, 95, 95)`
/// at the same `Y`. Pinned by
/// `the_crossing_composites_no_greyer_than_its_flanks`.
pub const BED_SAT_FLOOR: f32 = 1.0;

// There is deliberately NO floor for the bed against the GROUND. One was
// tried (2026-09-09, `BED_GROUND_CONTRAST_MIN` 1.4:1, `bar.max(ground)`): it
// reads as harmless on the shipped defaults, whose bars answer far above it
// (Nord `0.097` against `0.068`), but on any theme whose foreground solves
// the bar under `1.4 ×` its ground it OVERRIDES the bar — One Dark's
// `#ABB2BF` over `#282C34` went from 5.29:1 to 4.69:1 under the text, and 54
// (fg, ground) pairs of the 67-theme sweep were bar-violating by
// construction. A floor that must never exceed the bar collapses to the bar,
// so it cannot exist as a law beside it; how dark a bed a dark-foreground
// theme is handed is a ruling for the owner, and
// `the_warm_stops_frontier_table` prints it (Solarized Dark's bed sits 1.00:1
// over its page at the `BED_LUMA_MIN` clamp, its text at 4.70:1 — the
// documented exception; lifting it to 1.4:1 would cost that text 3.39:1, and
// One Dark's 5.29:1 → 4.69:1). Pinned by
// `the_ground_never_lifts_the_bed_over_the_bar`.

/// Entries in the bed-ink lookup table. The recipe below is a bisection over
/// the sRGB EOTF — far too expensive per slab — but it is a pure function of
/// the arc POSITION, so it is solved once per theme into this table and read
/// with one lerp. 129 entries put the interpolation error under one level
/// everywhere on the arc.
pub const BED_INK_LUT_LEN: usize = 129;

/// Share of the ceiling a COLD key's cell takes (the rest is bought by
/// momentum, [`Cell::cov0`]). A single keystroke on a cold hand must still lay
/// a band a person can read — the ribbon is the per-key light, and a per-key
/// light that needs a rhythm before it appears is a lag tell. The last eighth
/// is what a sustained run earns.
///
/// **0.88, from 0.72 (2026-09-06, measured by the paint-conformance gate —
/// §23).** At 0.72 of a `Y ≈ 0.08` ceiling the cold bed's brightest channel
/// sat at 40-110/255 on the default theme for the first second of a 7 cps
/// take (frames 40-97 of a kept take: colourful pixels 415-687 on the typed
/// row, none over 110), under the house pixel floor for "effect ink" that v1
/// cleared from its first key — the exact lag tell this constant exists to
/// forbid. The bed's LIGHT now starts near its ceiling; momentum still shows
/// as length, life, the hot edge and the wave, which is where the eye reads
/// speed anyway. The legibility bar (`BED_LUMA_MAX`) is untouched: the share
/// only moves inside it.
pub const BODY_COLD_SHARE: f32 = 0.88;

// ===========================================================================
// Life, the chain law, and the exit swoosh (§4, kept)
// ===========================================================================

/// The host's duration knob, as a multiplier on the base cell life.
pub const LIFE_DURATION_GAIN: f32 = 1.2;

/// Floor of the base cell life, seconds.
pub const LIFE_BASE_MIN: f32 = 0.26;

/// Ceiling of the base cell life, seconds.
pub const LIFE_BASE_MAX: f32 = 0.55;

/// Linear momentum term of the cell life: `base · (1 + 2.6·d + 11·d²)`. A
/// visible wake from the very first keys.
pub const LIFE_SPINE_LINEAR: f32 = 2.6;

/// …and the SUPERLINEAR term, so an earned hot run drags a rainbow across many
/// cells and wrapped lines instead of merely a longer wake.
pub const LIFE_SPINE_SQUARE: f32 = 11.0;

/// FOUR-LETTER GUARANTEE: within a rhythm a cell lives at least this many
/// observed inter-key gaps, so the ribbon spans about the last four typed
/// cells at ANY human cadence (4 cells lit needs ~3 gaps; the extra half-gap
/// buys the tail's fade room).
pub const CHAIN_KEYS: f32 = 4.5;

/// The ribbon chains across ANY plausible rhythm — one key every five seconds
/// still counts. Only a truly isolated keystroke (a gap beyond this) takes no
/// floor and fades crisply.
pub const CHAIN_GAP_MAX: f32 = 5.0;

/// Fade margin added to the chained life, seconds.
pub const CHAIN_MARGIN: f32 = 0.10;

/// …capped just above `CHAIN_KEYS × CHAIN_GAP_MAX` so the slowest chained
/// rhythm still earns its full four-letter span.
pub const CHAIN_LIFE_MAX: f32 = 23.0;

// ===========================================================================
// The wake — what a jump leaves behind (R5, 2026-09-08)
// ===========================================================================

/// **A JUMP LAYS ITS CORRIDOR** (the owner, 2026-09-08: "I want more of a
/// trailing cursor rainbow effect", "I like the streak effect, but leave more
/// a rainbow after effect"). Measured on 379ab6159, before this law, on
/// glass against a trail-OFF control: behind a Ctrl-E into unlit ground the
/// corridor held colour for under 100 ms — the train crosses 25 cells in
/// ~60 ms and its root then sucks INTO the landing (T5), so nothing of it
/// lingers where it flew — and a band live under the hand was gone 707 ms
/// after a Ctrl-A (the abandon's 0.40 + 0.24 s retract, sliding toward the
/// new caret). Nothing trailed. The wake is the ribbon the jump itself lays
/// behind the caret, newest at the landing, on the walk the band already
/// had (C2: `t` is a function of position, so the corridor continues the
/// live cohort's spectrum instead of re-anchoring at red).
///
/// A phrase, not a line: a Ctrl-A across 200 columns must not paint the
/// whole row. Counted back from the LANDING, so the rainbow lies beside the
/// caret, where the eye is. Cells beyond the cap that the live band already
/// lights are TAKEN OVER, never cut — light that was on the glass stays on
/// it and goes out on the wake's clock. A band ALREADY LEAVING (its cohort
/// retracting, or abandoned by an earlier jump) is not taken over and has
/// nothing laid under it while it is lit: it finishes on its own clock, and
/// the wake lights only the cells its drain has already emptied — a half-
/// drained cell handed to a fresh cohort would freeze at the level it had,
/// and that frozen ramp between two bright runs is the owner's "black gaps a
/// few characters back from the cursor" (R6), reproduced on a Ctrl-A at
/// 1.0–1.4 s idle before this clause.
pub const WAKE_MAX_CELLS: u16 = 32;

/// Long enough to read as an after-effect and short enough that a Ctrl-A /
/// Ctrl-E ping-pong cannot accumulate. FIXED, not [`Ribbon::cell_life`] — a
/// jump must not inherit the four-letter chain law, nor its 1.70 s swoosh
/// floor ([`SWOOSH_LIFE_S`]). A later JUMP never holds the wake's cohort
/// (each jump abandons what came before it), so the pool holds at most
/// `WAKE_LIFE_S / jump period` wakes. A later keyed move that is a SCRUB
/// over the wake ([`Ribbon::scrub`], 2026-09-14) — its corridor lit end to
/// end by the wake's own live cells, none yet inside its expiry melt — does
/// hold it, as it holds any cohort that owns the ground the hand crosses:
/// one cohort, no new cells, renewed for as long as the hand scrubs it.
/// A stub past its melt opening is never held, so the pinned ping-pong over
/// dark ground still empties within one wake life of its last key.
pub const WAKE_LIFE_S: f32 = 1.10;

// **THE WAKE IS BORN ALONG THE FLIGHT** (2026-09-13, Rainbow Path v3 §2.7,
// law K3). Corridor cell `j` of `n`, counted from the ORIGIN, is born at
// `t0 + T · j / n` with `T = flight_ms(cells)` — the arrival edge that
// already times the meteor head, the bell, the kitty's `land_at` and the
// pin — so the corridor is dark ahead of the meteor and lit behind it, and
// its far end is lit by `T ≤ 120 ms` (R10, < 200 ms). Until it is born a
// cell is DARK ([`Ribbon::env_of`]'s first line) — the attack starts at
// `born`, never before it. A hop (under [`JUMP_MIN_CELLS`]) takes
// [`FLIGHT_MIN_MS`]. This replaces `WAKE_BORN_LAG_S` 0.10, one lag for
// every cell of the corridor whatever its length: on glass the cells beside
// the landing came up a whole frame after the landing's own starburst (the
// refresh's "birth gap", cols 38–41 dark at +103 ms). There is no constant:
// the clock is [`flight_ms`]'s.

/// A same-row hop under [`JUMP_MIN_CELLS`] — an arrow key — lays its wake at
/// this share of [`WAKE_LIFE_S`]: holding an arrow paints rainbow behind the
/// caret, a single tap leaves a short one, and a cell a live cell already
/// owns is left exactly as it is (the hop does not abandon). The hop's new
/// cells go into a cohort of their own ([`Cohort::wake`]) on the row's own
/// walk, never into a typed word's: an arrow beside the word you just typed
/// does not restart the word's grace, move its bounds, or change when it
/// swooshes — a hop lifts no other mark's finger.
pub const WAKE_HOP_LIFE_SHARE: f32 = 0.5;

/// Cells the four-letter guarantee reaches for.
pub const FOUR_LETTER_CELLS: u16 = 4;

/// **THE PHRASE REST'S FLOOR**, seconds — the least grace a cohort keeps
/// after the last key of its phrase before the exit swoosh begins (finish
/// reaching four letters, then retract into the caret). Derived from the
/// melody's own rest ([`super::timing::PHRASE_PAUSE_MS`], 900 ms) since
/// 2026-09-13 (Rainbow Path v3 §2.6, S5): the song and the light let go of
/// a phrase on the same instant. A cohort's ACTUAL grace is
/// [`Cohort::grace_s`] — [`super::timing::phrase_rest_ms`] of the hand's
/// tempo at its last key, `max(0.90, 2.5 · ioi)` — so a slow hand's line
/// does not start leaving between two of its own keys; this floor is what
/// the derived totals below are stated at. (Was `LIFT_GRACE_S` 0.75, a
/// literal of its own: the audit measured the after-glow 931–1201 ms after
/// the last key against a law that promised more.)
pub const PHRASE_REST_MIN_S: f32 = super::timing::PHRASE_PAUSE_MS as f32 / 1000.0;

/// One extension cell lights per this step while the swoosh finishes REACHING
/// four letters.
pub const REACH_STEP_S: f32 = 0.05;

/// Beats of [`REACH_STEP_S`] the reach spends.
pub const REACH_BEATS: u16 = 3;

/// Cells the reach may stage in one tick, across every live cohort. Sized so
/// the staging array is a fixed local and the frame path allocates nothing
/// (§18); a mark whose reach is starved by it simply reaches on the next beat.
pub const REACH_STAGE: usize = 8;

/// The retract then drains the ribbon tail→head over this long, whatever its
/// length — a short hop and a hot multi-line comet both slurp cleanly back
/// into the caret.
pub const RETRACT_DUR_S: f32 = 0.40;

/// How long a RETRACTED cell takes to go out once the drain has selected it,
/// so the vanish is continuous instead of a staircase of whole-cell deletions
/// ending in a cliff.
pub const RETRACT_FADE_S: f32 = 0.24;

// MERGE, 0.86: the KILL's retract span used to be a literal `0.012·n + 0.240`
// here. Upstream gave the schedule ONE home in `timing.rs` — where the base
// and the per-cell term are named constants the sky reads too — and this
// module now imports [`kill_span_s`] from there. Upstream's home wins: its
// doc states everything this copy did (§8.2, the insert's rewrite reusing it
// verbatim, stardust's field sharing it) and it keeps the same operation
// order, `per_cell · cells + base`, so the retract is bit-identical.

/// The exit swoosh past the grace: `3 × 0.05 reach + 0.40 retract + 0.24
/// fade` = 0.79 s. The whole swoosh is `grace + 0.79 s` — [`swoosh_total_s`]
/// of the cohort's own grace.
pub const SWOOSH_AFTER_GRACE_S: f32 =
    REACH_STEP_S * REACH_BEATS as f32 + RETRACT_DUR_S + RETRACT_FADE_S;

/// The whole exit swoosh AT THE FLOOR GRACE: `0.90 grace + 0.79` = 1.69 s.
/// The least after-glow a phrase leaves; a cohort's own is
/// [`Cohort::swoosh_total`]. (Was two literals, 1.54 and a 1.70 life floor;
/// both are DERIVED now, §2.6.)
///
/// MERGE, 0.86: this DERIVATION supersedes main's `LIFT_GRACE_S + …` = 1.54 s
/// literal, and the merge does not let the literal back. The grace is the
/// melody's phrase rest (Rainbow Path v3 §2.6 step 5): `PHRASE_REST_MIN_S`
/// 0.90 s at the floor and tempo-scaled above it, so a slower hand gets a
/// longer after-glow instead of one 0.75 s number for every tempo. At the
/// floor that is +150 ms of after-glow against main's form — the difference
/// of the two constants and nothing softer, `0.90 + 0.79` against
/// `0.75 + 0.79`. `LIFT_GRACE_S` no longer exists to be added, so main's
/// form would not even compile here.
pub const SWOOSH_TOTAL_S: f32 = PHRASE_REST_MIN_S + SWOOSH_AFTER_GRACE_S;

/// The margin a cell's life keeps past its swoosh, so the swoosh always
/// terminates the ribbon before the natural melt can — **the swoosh IS the
/// ending, never a passive dim-out.** A cell's life floor is
/// [`swoosh_life_s`] of the rest its phrase was priced at when it was laid,
/// and every key that holds the phrase re-floors the phrase's cells
/// ([`Ribbon::place`]), so a rest that lengthens under a slowing hand can
/// never outlive the cells it is holding.
pub const SWOOSH_LIFE_MARGIN_S: f32 = 0.01;

/// The life floor at the floor grace, 1.70 s — the number the chain law and
/// the frame-cost gate were measured against.
pub const SWOOSH_LIFE_S: f32 = SWOOSH_TOTAL_S + SWOOSH_LIFE_MARGIN_S;

/// The swoosh's own offset of its RETRACT at the floor grace: grace plus the
/// three reach beats, the idle at which a cohort starts moving into the
/// caret; a cohort's own is [`Cohort::retract_start`]. A real jump ABANDONS
/// the band by rewinding its cohorts' `alive_at` to exactly this far back,
/// so the mark goes out through the retract + fade it would have taken
/// anyway (`0.40 + 0.24 s`), starting from the light it has NOW — never a
/// step (v1's own note: clamping `life` steps too, because the melt rides
/// `age / life`).
pub const RETRACT_START_S: f32 = retract_start_s(PHRASE_REST_MIN_S);

/// The idle at which a cohort of grace `grace_s` starts its retract.
#[inline]
#[must_use]
pub const fn retract_start_s(grace_s: f32) -> f32 {
    grace_s + REACH_STEP_S * REACH_BEATS as f32
}

/// The whole swoosh of a cohort of grace `grace_s`.
#[inline]
#[must_use]
pub const fn swoosh_total_s(grace_s: f32) -> f32 {
    grace_s + SWOOSH_AFTER_GRACE_S
}

/// The life floor of a cell whose phrase rests `grace_s`.
#[inline]
#[must_use]
pub const fn swoosh_life_s(grace_s: f32) -> f32 {
    swoosh_total_s(grace_s) + SWOOSH_LIFE_MARGIN_S
}

/// Focus loss embers the ribbon out over this long on `spend` (§8.2). Nothing
/// sounds, and nothing is retracted — the mark simply stops being lit.
pub const FOCUS_EMBER_S: f32 = 0.30;

/// **A STOP IS STILL ON THE GLASS** while its composited pixel is still a
/// COLOUR the eye can pick out of the ground: [`SEEN_CONTRAST`] of contrast
/// against the theme's own background in the channel that carries it, and at
/// least this much channel SPREAD, so what is left is a hue and not a grey
/// wash.
///
/// The SPREAD is a property of the PIXEL alone — a chroma, in bytes — so it
/// already says the same thing over any ground, and it is the paint census's
/// own `max - min >= 40` unchanged. The LEVEL is the half that has to be
/// said against the ground, and it is said in [`SEEN_CONTRAST`].
pub const SEEN_SPREAD: f32 = 40.0;

/// **THE LEVEL HALF OF [`SEEN_SPREAD`], AS A RATIO AND NOT A BYTE COUNT** —
/// the WCAG contrast the composite must stand from the ground in its
/// furthest channel (`channel_contrast`) to be a mark at all.
///
/// **It replaces an absolute 24-byte lift, refuted on glass the same day it
/// shipped (2026-09-14).** A byte lift is not one statement: 24 bytes off
/// Nord's `#2E3440` is a composite peaking at 71..90, comfortably over the
/// paint census's own colour floor (`ribscan` calls a pixel painted at
/// `max(r, g, b) >= 60 && max - min >= 40`), while 24 bytes off the SHIPPED
/// default `#111318` is a composite peaking at 50..59 — UNDER it, for all
/// 24 stops of the arc. The same constant that took Nord to zero dark runs
/// therefore took the default theme's ember to 8 frames of 30 carrying a
/// run of up to 33 dark cells: the band exited at a level that STRADDLES
/// the instrument's line, and which cells fell on which side of it was left
/// to the blur. It is the same defect on five of the eight dark themes that
/// ship (default, Solarized Dark, Tokyo Night, Catppuccin Mocha, One Dark);
/// Nord is one of the three it happens to miss.
///
/// **A RATIO is one statement.** On `#111318` the census's own 60 is 1.71:1
/// in the red channel (1.68 green, 1.61 blue); `1.90` is that line plus the
/// margin a byte of rounding, the ink table's 1/128th sampling and the
/// emitter's own quantisation need. It puts every stop of every shipped
/// theme out at a composite the census still calls paint — peaking at 69..75
/// on the default ground and 86..109 on Nord — with no theme in between
/// where the exit can straddle the line. On a light theme it is the same
/// sentence read in the paper's polarity: the mark stands that far BELOW
/// the ground instead of above it.
///
/// It cannot lift a stop the bed's own bar already leaves under the census
/// (Solarized Dark's foreground puts 13 of the 24 stops under it at FULL
/// coverage, before any ember): there the floor lands over the cell's whole
/// coverage and the stop simply HOLDS until the pool clears, which is the
/// most a fade can do for a stop that has no room to fade.
pub const SEEN_CONTRAST: f32 = 1.90;

/// **THE EMBER'S PER-STOP FLOOR, in that stop's own extinction units** — the
/// coverage the focus ember leaves a stop at, [`BedInkLut::seen_floor_at`]
/// times this, just far enough over the colour floor that no rounding of the
/// alpha byte and no error in the table's own sampling can take one stop
/// under it while its neighbours are still lit.
///
/// It is deliberately SMALL. The band's disappearance is the ember's end —
/// `Ribbon::retire`'s one clear of the pool, the single global decision in
/// the whole seam — and the band should reach that instant as dim as the
/// census's colour floor allows, so what the eye sees is a band that has
/// dimmed to almost nothing and then stops, rather than a band cut off while
/// it still has light. A LOWER floor cannot buy more: below it a stop is not
/// on the glass at all, and taking the stops under it one at a time is the
/// dark run this whole mechanism exists to forbid.
pub const EMBER_FLOOR_MARGIN: f32 = 1.05;

/// **THE CURTAIN** (2026-09-13, Rainbow Path v3 §2.8, law A2; D-2, D-3):
/// at a coordinate-space seam the host cannot carry the cells across — the
/// alternate screen, a tab or terminal switch, a resize with no reflow map,
/// a content invalidation — every cohort is drawn into the hand it last
/// stood under over this long on `suck-in` and spent on `spend`,
/// FARTHEST-FIRST ([`CURTAIN_STAGGER_S`]), and nothing is laid until it is
/// over. The retract's own fade
/// ([`RETRACT_FADE_S`]), so the eye reads the one exit it knows. Where the
/// old law was `reset()` — 15–18 lit cells → 0 in one 17 ms frame (audit
/// s8, the refresh's s19, s7) — this is at least three captured frames at
/// 30 fps and never a frame where the count drops from > 4 to 0.
pub const CURTAIN_S: f32 = RETRACT_FADE_S;

/// **THE CURTAIN IS STAGGERED, FARTHEST-FIRST** (2026-09-14): the window of
/// `CURTAIN_S` over which the cells START spending, in [`Cohort::drain_rank`]
/// order — the end farthest from the hand at `0`, the cell under the hand at
/// `CURTAIN_STAGGER_S`. The retract this curtain imitates is staggered
/// (`RETRACT_DUR_S` of starts, then [`RETRACT_FADE_S`] of spend each), and a
/// curtain that put every cell on ONE clock did not gather the band in — it
/// dropped the whole band at once: measured on glass at every band age swept
/// (last key +40 ms through +1040 ms), `ribbon_segments` fell 19 → 0 in a
/// SINGLE frame, which is the cliff `CURTAIN_S`'s own doc comment forbids.
/// The natural retract never steps more than 6.
///
/// **THE RETRACT'S OWN RATIO**, on the curtain's own budget: the retract
/// spends `RETRACT_DUR_S` of starts and then `RETRACT_FADE_S` per cell, so
/// `RETRACT_DUR_S / (RETRACT_DUR_S + RETRACT_FADE_S)` of its span is
/// stagger. The curtain takes that fraction of `CURTAIN_S`, which keeps the
/// whole fall inside 0.24 s — the number the design, the doc comments and
/// the CHANGELOG all state — while giving the count the retract's shape.
///
/// A band of `n` boundaries then loses about `n · frame / CURTAIN_STAGGER_S`
/// per frame — 22 % of it at 30 fps, 11 % at 60 — instead of all of it at
/// once. See [`CURTAIN_MAX_STEP_FRAC`].
pub const CURTAIN_STAGGER_S: f32 = CURTAIN_S * (RETRACT_DUR_S / (RETRACT_DUR_S + RETRACT_FADE_S));

/// What each cell of a falling curtain spends over, once its
/// [`CURTAIN_STAGGER_S`] turn has come — the remainder, so the LAST cell to
/// start (the one under the hand) reaches exactly zero at `CURTAIN_S`.
pub const CURTAIN_SPEND_S: f32 = CURTAIN_S - CURTAIN_STAGGER_S;

/// **THE CLIFF BOUND** the curtain's law is stated against: the most of the
/// band ONE 33 ms frame may take while a curtain runs, as a fraction of what
/// was lit when the curtain fell — measured in the quantity `trail status`
/// and the glass census actually read (`ribbon_segments`, and
/// [`Ribbon::ink_segments`]'s per-stop count beside it), never in a
/// geometric predicate that scores alpha 1 and alpha 236 alike.
///
/// Stated as a FRACTION because the quantity is scale-dependent:
/// `slabs_per_cell` is 3, 2 or 1 as the budget and the momentum move, so the
/// same band is 58 boundaries in the twin's fixture and 19 on the owner's
/// glass. The arithmetic above gives 22 % at 30 fps; the headroom is for the
/// bunching a discrete count shows. A curtain on ONE clock takes 100 % in a
/// single frame, which is what this refutes.
pub const CURTAIN_MAX_STEP_FRAC: f32 = 0.34;

/// …and the other half of the same law: a curtain must be seen to FALL.
/// At 30 fps at least this many frames carry band ink between the seam and
/// darkness — `reset()`'s one frame, and a one-clock curtain's two, are
/// both under it.
pub const CURTAIN_MIN_FRAMES: usize = 5;

/// **THE CONTENT RETIREMENT'S MELT** (2026-09-12, the abandoned band): a
/// cell the host has SEEN its glyph REPLACED under — overwritten by a
/// re-laid input box, or blanked while the same text stands on another
/// row, the box re-laid elsewhere — goes out over this long on the
/// retract's own `spend` law ([`Ribbon::retire_cells`]). Short, because
/// the light is sitting under the wrong letters, or beside the text it was
/// laid under, and the module's yardstick is "no light with no keystroke
/// behind it": the eye must read it as the mark following the text away,
/// not as a mark lingering where text used to be. Well under the 150 ms
/// the owner's "stray rainbows" ruling allows, and half the retract's own
/// fade, so a witness that fires on the frame a box relocates has the old
/// band dark before the hand's next key echoes.
///
/// NOT the clock for a glyph that merely WENT (2026-09-14, the new line's
/// fade): a band whose text was erased and not replaced — a submit, a
/// Ctrl-U — has nothing to be wrong over, and this cut read as "the
/// contrail disappeared". It is released to the swoosh's retract instead
/// ([`Ribbon::release_cells`]).
pub const RETIRE_MELT_S: f32 = 0.12;

/// The share of a cell's life over which the expiry melt runs (v1's
/// `RAINBOW_EDGE_OUT`). Identity over the first 70 %.
pub const EXPIRY_MELT_SHARE: f32 = 0.30;

/// The expiry melt's exponent (v1's `RAINBOW_EXPIRY_MELT_GAMMA`). SUPERLINEAR,
/// because a cell that is out of time must be at zero BEFORE it is removed or
/// its removal is a visible step: at γ = 1.6 the melt reaches 6e-4 of body on
/// the last frame, where a sub-linear taper still holds ~6 %.
pub const EXPIRY_MELT_GAMMA: f32 = 1.6;

/// The coverage share the run's outer TAIL boundary keeps, ramping linearly to
/// full across the tail cell (v1's `RAINBOW_RUN_TAIL_EASE`): the band's oldest
/// edge reads as a full slab easing out through a feather, never as a chopped
/// stub.
///
/// **WHERE IT APPLIES (2026-09-14, the owner's slivers).** At EVERY boundary
/// the younger side's light enters at this share and the boundary takes the
/// brighter of the two sides — `max(older side, younger side × ease)`, the
/// older side being the tail side of the run's stream ([`Run::stream_dir`]:
/// the left for a typed run). Where the older side is a live cell at full
/// light the boundary is that light and the ease never shows; where it is
/// absent the boundary is the feather; where it is a cell that is LEAVING
/// the feather grows in exactly as fast as that cell's light goes. Before
/// this the ease was a flat tenth at the first boundary of every RUN — and a
/// run is one cohort, so a wake laid beside a word, or a word whose earlier
/// cells a composer's re-anchor had drained, printed a dark column and a
/// step INSIDE a continuous band on the very frame its neighbour retired.
pub const RUN_TAIL_EASE: f32 = 0.10;

// ===========================================================================
// The atoms
// ===========================================================================

/// **THE COHORT'S CLOCK** (2026-09-08): the instant a cell's expiry is
/// measured from — its own birth or its cohort's last live typing key,
/// whichever is later. Every key in a live run refreshes the whole run, so a
/// cell born with 1.70 s of life keeps 1.70 s of life AFTER THE LAST KEY of
/// its run rather than after its own; a mark only ages once the hand stops.
/// A cell whose cohort is gone (never on the frame path — `retire` drops the
/// cells with the cohort) falls back to its birth.
///
/// Zero-alloc and O(cohorts) per call; a row has one cohort per typing burst,
/// so this is a handful of compares.
#[must_use]
pub fn live_since(cohorts: &[Cohort], cell: &Cell) -> Instant {
    cohorts
        .iter()
        .find(|c| c.id == cell.cohort)
        .map_or(cell.born, |c| cell.born.max(c.alive_at))
}

/// **WHICH LAYER A CELL IS ON** (Rainbow Path v3 §2.3, law K1). Typed and
/// insert lays write `Base`; a wake — a jump's corridor, a hop's short trail
/// — writes ONLY `Over` and never touches a `Base` cohort's cells or clocks:
/// "a leaving band is never taken over" is literal, the wake does not
/// address the band at all. Composition is per position: a born `Over`
/// cell is what the geometry draws where one exists ([`Ribbon::build`]),
/// the `Base` cell beneath it otherwise — and because the wake shares the
/// band's walk origin (R9, [`Ribbon::wake_origin`]) the two carry the same
/// colour at the same column, so an overlay over a draining band reads as
/// one continuous corridor and never a hole.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Layer {
    /// A typed key's, an insert's, or the swoosh's reach's cell.
    #[default]
    Base,
    /// A wake's cell — a jump's corridor or a hop's trail.
    Over,
}

/// One LAID RIBBON CELL — the atom of the field (v1's `spark`, ported).
///
/// `Copy` and flat: the cell pool is a resident `Vec` reused every frame
/// (§18's zero-allocation rule), and a style-crossfade ghost snapshots it.
#[derive(Clone, Copy, Debug)]
pub struct Cell {
    /// Grid row.
    pub row: u16,
    /// Grid column.
    pub col: u16,
    /// Which [`Cohort`] laid it — the hue walk's identity, so a re-typed cell
    /// re-joins the run it belongs to instead of restarting at red.
    pub cohort: u32,
    /// THE FIELD at this cell: the classic walk's spectrum position, 0..1+.
    /// C2 — the caret, the ribbon head and a star's halo on this cell are the
    /// same colour on the same frame because they all read THIS number.
    pub t: f32,
    /// When this cell was laid (the echo frame). Its identity in the content
    /// witness and the newest-writer ordering; a fresh lay never inherits an
    /// earlier glyph's record. The expiry melt runs from [`live_since`] —
    /// this, or the cohort's last live key, whichever is later.
    pub born: Instant,
    /// Start of the visual 18 ms `edge-in`. A live same-kind re-wet keeps
    /// the earlier attack so an already lit glyph does not dip, while `born`
    /// still names the fresh cell. A leaving cell starts a fresh attack.
    /// Future wake cells remain dark until `born`, even with an older attack.
    pub attack_at: Instant,
    /// Total life in seconds, priced at birth from `Spine::birth_disp` — and
    /// measured from the COHORT'S clock, not this cell's (2026-09-08): the
    /// life a cell was born with is the life it has left after the LAST key
    /// of its run, so no cell can run out under a hand that is still typing.
    pub life_s: f32,
    /// **THE CELL'S LIGHT, PRICED ONCE.** The share of [`UNDER_COV_CAP`] this
    /// cell's body may take at its peak, `0..1`, resolved at BIRTH from
    /// `Spine::birth_disp` and never re-read from the live spine again.
    ///
    /// This field is the whole of the fix for the measured *"it keeps
    /// brightening for ~660 ms after the last key"* defect: the follower is
    /// still climbing when the hand stops, so a body that re-prices itself
    /// every frame gains light with no keystroke behind it — which is exactly
    /// what the anti-stray law forbids. Shape may follow the live spine (the
    /// wave, the wedge; both only SETTLE); light may not.
    pub cov0: f32,
    /// True for a cell laid by a real typing advance (as opposed to the exit
    /// swoosh's reach) — the "earned by real typing only" gate v1 spells in
    /// `spawn`, and the gate stardust's field-star deal reads.
    pub typing: bool,
    /// Set when the cell is retracting toward the caret (Backspace or kill):
    /// the `spend` alpha law runs from this stamp. `None` while the cell is
    /// simply living. The exit swoosh drives the whole COHORT instead, because
    /// it retracts the mark as one object.
    pub retract_at: Option<Instant>,
    /// Set when the cell has been RETIRED by content ([`Ribbon::retire_cells`]):
    /// the glyph it was laid under has been REPLACED by another, so the light
    /// sits under the wrong letters. The cell spends to exactly zero over
    /// [`RETIRE_MELT_S`] from this stamp and leaves the pool; its attack is
    /// frozen at the stamp so it can never brighten after it. `None` while
    /// the cell is simply living — and `None` for a cell whose glyph merely
    /// WENT ([`Ribbon::release_cells`]): that cell leaves with its cohort
    /// through the swoosh's retract, unstamped. Distinct from
    /// [`Cell::retract_at`] so the Backspace goldens stay byte-identical.
    pub retire_at: Option<Instant>,
    /// The `Spine::birth_disp` this cell was priced at — the number behind
    /// [`Cell::cov0`], kept so the HOT EDGE can be priced from the head cell's
    /// birth as well: a hairline that re-read the live follower would brighten
    /// for ~100 ms after the hand stopped, which is the "light with no
    /// keystroke behind it" class this module is written against.
    pub birth_disp: f32,
    /// **THE CRISP EDGE'S REACH**, in cells, priced at BIRTH by
    /// [`edge_cells`] from the surge and flow's heat — one `f32`, read only
    /// by [`Ribbon::emit_hot_edge`] and only from the HEAD cell, and never
    /// re-read from the live spine (the same law as [`Cell::cov0`]: shape may
    /// follow the spine, light and reach may not).
    pub edge_cells: f32,
    /// Which layer this cell is on — see [`Layer`].
    pub layer: Layer,
    /// **RE-ARMED FROM A LEVEL** (Rainbow Path v3 §2.6, law M1 — monotone
    /// re-wetting): `(when, level)`. Set when a cell's light continues
    /// another's — a wake cell born over a live band cell at the band's own
    /// level, or a corridor cell RE-OWNED by a later jump from the level it
    /// had — and read by [`Ribbon::env_of`] in place of the birth attack:
    /// the envelope starts at exactly `level` at `when` and rises to full
    /// through the one sanctioned attack, `edge-in`. It rises, never steps,
    /// and is never held at a partial level for longer than `EDGE_IN_S` —
    /// which is what the old takeover broke when it baked a half-melted
    /// cell's level into `cov0` (R6's frozen ramp). `None` for a cell born
    /// from dark on the ordinary attack.
    pub rearm: Option<(Instant, f32)>,
}

impl Cell {
    /// True once the cell is on its way OUT — retracting after a Backspace or
    /// kill, or retired by content — and so no longer an OWNER of its column:
    /// a sweep may lay over it, a wake may not take it over, the head is not
    /// it, and a witness does not read it. The one predicate every "is this
    /// cell live" site reads, so a new way of leaving cannot be forgotten at
    /// one of them.
    #[must_use]
    pub fn leaving(&self) -> bool {
        self.retract_at.is_some() || self.retire_at.is_some()
    }
}

/// A COHORT — one contiguous run of cells laid by one typing burst on one row,
/// sharing a hue walk (v1's `classic_run`). The unit the exit swoosh, the wrap
/// fold and the kill drain all act on.
#[derive(Clone, Copy, Debug)]
pub struct Cohort {
    /// Identity, referenced by [`Cell::cohort`].
    pub id: u32,
    /// The row this cohort lives on. A cohort never spans rows; a wrap FOLDS
    /// into a new cohort (§4's "wrap fold", kept).
    pub row: u16,
    /// Leftmost column the cohort covers.
    pub col0: u16,
    /// One past the rightmost column the cohort covers.
    pub col1: u16,
    /// **THE WALK'S ORIGIN** — the column of the cohort's FIRST typed cell,
    /// and immutable: the swoosh's reach moves [`Cohort::col0`] under the
    /// mark, an edit inside the run moves the head, and neither may repaint a
    /// colour the eye has already read. C2 / §4: `t` is a function of
    /// POSITION, `t0 + walk_t(col − anchor_col)` (see [`Cohort::t_at`]), so a
    /// cell backspaced and retyped takes back exactly the stop it had.
    pub anchor_col: u16,
    /// The walk's anchor value — the `t` the cell at [`Cohort::anchor_col`]
    /// took. A same-row rebirth or a wrap fold within v1's freshness window
    /// CONTINUES the previous cohort's walk here; past it, the walk re-anchors
    /// at red.
    pub t0: f32,
    /// When the cohort's first cell was laid.
    pub born: Instant,
    /// The exit swoosh's grace anchor (0.75 s of grace before the drain
    /// begins): the last live typing key ON THIS COHORT'S ROW — the swoosh is
    /// the FINGER-LIFT arc (§4: "no typing key for this long"), and since
    /// 2026-09-12 the finger is read PER ROW ([`Ribbon::place`]): a key holds
    /// every un-abandoned cohort on the row it lays on and none elsewhere.
    /// A wrapped paragraph's earlier row keeps the life it had when the hand
    /// left it and leaves through its own swoosh while the hand types on
    /// the next — and so does a band an input box was re-laid away from,
    /// which under the one-finger-anywhere law was renewed by every later
    /// key on the new row and outlived the hand for as long as it typed. An
    /// [`Cohort::abandoned`] cohort keeps a clock of its own.
    ///
    /// It is also the clock every cell's EXPIRY runs from ([`live_since`],
    /// 2026-09-08): a key refreshes the whole run's life, not only the cell
    /// it lays. Monotone — a sweep laid at an older key's stamp never
    /// rewinds it.
    pub alive_at: Instant,
    /// Set by a real jump ([`Ribbon::abandon`]): this cohort is leaving on its
    /// own rewound clock, and a typing key elsewhere no longer refreshes it —
    /// the band under the hand's OLD position must go out while the new one
    /// is being laid, not wait for the next finger-lift.
    pub abandoned: bool,
    /// Minted by a wake ([`Ribbon::wake`]) and never typed into. A HOP's
    /// new cells join only such a cohort, or mint one — so an arrow beside
    /// a live word over blank ground touches neither the word's clock nor
    /// its bounds (its cells it leaves exactly as they are) — and a typed
    /// key laid into a wake cohort clears the flag: the hand is on it now.
    /// An arrow OVER the word — every crossed cell lit — is a scrub
    /// ([`Ribbon::scrub`], 2026-09-14): it lays no wake at all and holds
    /// the word's clock, the hand being on it.
    pub wake: bool,
    /// The caret column the retract pulls this cohort toward, captured the
    /// frame it entered its retract ([`Ribbon::advance_swoosh`]) and held
    /// until a key brings the cohort back to laying. T5 says a leaving mark
    /// moves TOWARD THE CARET, and it does — toward the hand that let go of
    /// it: a mark read against the LIVE caret lurched across the row the
    /// frame a Ctrl-A landed while it was half-way into its old spot (13
    /// cells at 1.25 s idle), which is the step "smooth transitions" forbids.
    pub retract_col: Option<u16>,
    /// **WHICH END DRAINS FIRST** once the retract owns the mark, frozen
    /// beside [`Cohort::retract_col`] (§2.6, per-row drain order): `false`
    /// — the left end first, farthest from a head at the right — for the
    /// caret's own row and a row ABOVE it; `true` — the right end first —
    /// for a row BELOW the caret, whose logical-farthest cell is its right
    /// end. The band always drains away from the hand.
    pub drain_right_first: bool,
    /// Where the cohort is in the exit choreography.
    pub phase: Phase,
    /// An abandon the HAND did not make — a program's caret round trip, or
    /// the hand leaving the row — which a typed key landing in or beside
    /// this cohort takes back ([`Ribbon::join_cohort`], 2026-09-13). A jump
    /// the hand made (Nav, Return) is not rejoinable: it meant to leave.
    pub rejoinable: bool,
    /// **THE PHRASE THIS COHORT BELONGS TO** (Rainbow Path v3 §2.3, S6):
    /// the Score's unit — opened by the first typed key after a rest or
    /// after a Return / a line kill, closed by the next. Every cohort minted
    /// or joined by a typed lay while phrase `P` is open belongs to `P`; a
    /// wrap fold's new-row cohort belongs to `P`; a typing lay into a cohort
    /// of a CLOSED phrase (the hand paused past the rest and resumed on the
    /// same line) re-assigns it to the open phrase. `0` for a cohort no
    /// phrase holds — a wake's. ONE FINGER holds ONE PHRASE
    /// ([`Ribbon::place`]): a key refreshes the cohorts of the open phrase
    /// that are ON THE HAND'S ROW, and no other (the merged row clause,
    /// 2026-09-14 — see [`Ribbon::place`]).
    pub phrase: u32,
    /// **THE GRACE THIS COHORT'S SWOOSH KEEPS**, seconds — the phrase rest
    /// ([`super::timing::phrase_rest_ms`]) at the hand's tempo when the
    /// cohort was last held; [`PHRASE_REST_MIN_S`] at the floor. The idle
    /// after `alive_at` at which the reach begins; the retract begins at
    /// [`Cohort::retract_start`] and the swoosh is over at
    /// [`Cohort::swoosh_total`].
    pub grace_s: f32,
    /// A content-only release may be undone when the same complete cell
    /// identities and glyphs return. A later abandon/retirement revokes it.
    release_clock: Option<ReleaseClock>,
}

#[derive(Clone, Copy, Debug)]
struct ReleaseClock {
    alive_at: Instant,
    retract_col: Option<u16>,
    phase: Phase,
    rejoinable: bool,
    cells: usize,
}

impl Cohort {
    /// The idle at which this cohort's retract begins: its grace plus the
    /// three reach beats.
    #[inline]
    #[must_use]
    pub fn retract_start(&self) -> f32 {
        retract_start_s(self.grace_s)
    }

    /// The idle at which this cohort's swoosh is over.
    #[inline]
    #[must_use]
    pub fn swoosh_total(&self) -> f32 {
        swoosh_total_s(self.grace_s)
    }

    /// **THE DRAIN RANK OF A COLUMN**, `0..=1` — how early in the retract
    /// the cell at `col` starts spending: `1` first (the end farthest from
    /// the hand in logical order), `0` last (the end nearest it). Read by
    /// the envelope and the cadence from the frozen
    /// [`Cohort::drain_right_first`], so the order cannot flip mid-drain.
    #[inline]
    #[must_use]
    pub fn drain_rank(&self, col: u16) -> f32 {
        let span = f32::from(self.col1.saturating_sub(self.col0)).max(1.0);
        if self.drain_right_first {
            f32::from(col.saturating_sub(self.col0)) / span
        } else {
            f32::from(self.col1.saturating_sub(1).saturating_sub(col)) / span
        }
    }

    /// **THE FIELD AT A COLUMN OF THIS COHORT** (C2): the classic walk read
    /// from the immutable origin, so it is the same number however the cell
    /// came to be laid — typed, retyped after a backspace, or reached by the
    /// swoosh (a reach cell sits BEFORE the origin and continues the walk
    /// backwards at the same rate).
    #[must_use]
    pub fn t_at(&self, col: u16) -> f32 {
        self.t0 + walk_t(f32::from(col) - f32::from(self.anchor_col))
    }
}

/// A cohort's place in the exit swoosh (§4: `0.75 grace + 3 × 0.05 reach +
/// 0.40 retract + 0.24 fade`), kept verbatim from v1.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Phase {
    /// Taking cells — the hand is on it.
    #[default]
    Laying,
    /// The 0.75 s grace after the last key: nothing moves yet.
    Grace,
    /// The three 0.05 s reach beats.
    Reaching,
    /// The 0.40 s retract toward the caret on `suck-in`.
    Retracting,
    /// The 0.24 s fade to exactly zero.
    Fading,
}

impl Phase {
    /// The phase a cohort of grace `grace_s` ([`Cohort::grace_s`]) is in
    /// `idle_s` seconds after its last live key. `None` once the swoosh has
    /// finished and the cohort is over.
    #[must_use]
    pub fn at(idle_s: f32, grace_s: f32) -> Option<Self> {
        let grace = if grace_s.is_finite() {
            grace_s.max(PHRASE_REST_MIN_S)
        } else {
            PHRASE_REST_MIN_S
        };
        let reach_end = retract_start_s(grace);
        let retract_end = reach_end + RETRACT_DUR_S;
        if idle_s.is_nan() || idle_s <= 0.0 {
            // NaN-refusing spelling: a non-finite idle takes the laying arm,
            // which draws the mark rather than dropping it.
            return Some(Self::Laying);
        }
        if idle_s < grace {
            Some(Self::Grace)
        } else if idle_s < reach_end {
            Some(Self::Reaching)
        } else if idle_s < retract_end {
            Some(Self::Retracting)
        } else if idle_s < swoosh_total_s(grace) {
            Some(Self::Fading)
        } else {
            None
        }
    }

    /// True once the swoosh owns the mark and the retract is moving it.
    #[must_use]
    pub fn is_retracting(self) -> bool {
        matches!(self, Self::Retracting | Self::Fading)
    }
}

/// One planned sample of a cohort's spine at a CELL BOUNDARY — what
/// `aterm_render::RibbonVertex` is built from.
///
/// Boundary-anchored, not centre-anchored, for the reason `RibbonVertex`'s own
/// doc gives: a segment must run boundary-to-boundary so every major-axis slab
/// `ribbon_beam` tiles lands inside ONE cell however the step divides the cell
/// width.
#[derive(Clone, Copy, Debug)]
pub struct Segment {
    /// Window-absolute X of the boundary, sub-pixel.
    pub x: f32,
    /// The body's full-strength centreline here, sub-pixel — the 0.75-cycle
    /// wave at `0.055 ch · disp` rides ON this value.
    pub spine: f32,
    /// Reach above the spine before the profile is exactly zero.
    pub up: f32,
    /// …and below it.
    pub dn: f32,
    /// The field at this boundary (C2) — resolved to colour through
    /// `spectrum::spectrum(t)`.
    pub t: f32,
    /// Coverage at the spine, already folding time-fade, intensity, the
    /// `edge-in` and the expiry melt. Capped at [`UNDER_COV_CAP`].
    pub cov: u8,
}

/// The vertical extent of the ribbon's body at one cell — `[spine − up,
/// spine + dn]`.
///
/// Published so stardust can obey §4.2 ("no mark is drawn inside the ribbon
/// band") and §5.4 (the sky band is `[top − 0.30 ch, top − 0.04 ch]`, relative
/// to THIS `top`, which is D16's entire resolution: under the shipped `tall`
/// spelling the band covers the whole cell, so a zone stated in cell
/// coordinates would put every strike star inside the body).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Band {
    /// Window-absolute Y of the body's TOP edge (`spine − up`).
    pub top: f32,
    /// Window-absolute Y of the body's centreline, wave included.
    pub spine: f32,
    /// Window-absolute Y of the body's BOTTOM edge (`spine + dn`).
    pub bottom: f32,
}

impl Band {
    /// True when `y` lies inside the body — §4.2's whole claim, in one call, so
    /// stardust never has to re-derive the inequality.
    #[must_use]
    pub fn contains(&self, y: f32) -> bool {
        (self.top..=self.bottom).contains(&y)
    }
}

/// **THE TWO PROFILES, ONE EMITTER** (§4, D14).
///
/// The tall/underline fork is these four numbers and nothing else: both
/// spellings walk the same [`Ribbon::plan`], are drawn by the same single
/// `aterm_render::ribbon_beam` call, and price their light through the same
/// [`Cell::cov0`] law. In v1 the fork was two bodies, and it flip-flopped six
/// times in five days; a body one `if` can produce cannot flip-flop.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BodyProfile {
    /// Reach above the spine, in `ch`.
    pub up_ch: f32,
    /// Reach below the spine at full momentum, in `ch`.
    pub dn_ch: f32,
    /// Full-strength plateau above the spine, in `ch`.
    pub core_up_ch: f32,
    /// The shoulder `aterm_render::ribbon_profile` takes on the UP side — the
    /// one parameter the two spellings really disagree about.
    pub shoulder: f32,
}

/// One row's dense `col → t` lane of the field index.
#[derive(Clone, Debug, Default)]
struct FieldLane {
    /// The grid row this lane indexes.
    row: u16,
    /// `t` by column, `f32::NAN` where the row carries no field. Sized to the
    /// grid's column count once and REUSED — the lane is cleared over
    /// [`FieldLane::touched`] only, never zero-filled wholesale (§18 deletes
    /// v1's 8 KB `owner_starts` zero-init per frame for exactly this reason).
    t: Vec<f32>,
    /// The columns written this frame, so the next frame's clear is O(marks)
    /// rather than O(columns).
    touched: Vec<u16>,
}

impl FieldLane {
    /// Clear only what the last frame wrote — the O(marks) reset §18 asks for.
    fn clear(&mut self) {
        for &col in &self.touched {
            if let Some(slot) = self.t.get_mut(usize::from(col)) {
                *slot = f32::NAN;
            }
        }
        self.touched.clear();
    }

    /// Size the lane to `cols`, filling with the "no field here" sentinel.
    fn resize(&mut self, cols: u16) {
        self.t.clear();
        self.t.resize(usize::from(cols), f32::NAN);
        self.touched.clear();
    }
}

/// **THE FIELD INDEX** (§18) — `col → t` for every row carrying live ribbon
/// light, rebuilt once per frame by [`Ribbon::plan`].
///
/// Lookups are O(1) in COLUMNS and linear in LIVE ROWS (a handful: the ribbon
/// is a per-row mark and the cohort cap bounds it). That is what replaces
/// v1's three per-station reverse scans of `sparks`, which is the single
/// largest CPU line item §18 deletes.
#[derive(Clone, Debug, Default)]
pub struct FieldIndex {
    /// One lane per live row. Resident and reused; see [`FieldLane::touched`].
    lanes: Vec<FieldLane>,
    /// Lanes whose row went dark, kept at full width for the next row that
    /// lights: an Enter or a wrap re-lights a row every few seconds while
    /// typing, and minting a fresh `cols`-wide `Vec` for each one is exactly
    /// the per-frame allocation §18 forbids. Bounded by the most rows that
    /// were ever live at once.
    spare: Vec<FieldLane>,
    /// The grid width the lanes were sized to — a resize rebuilds them.
    cols: u16,
}

impl FieldIndex {
    /// The field at a cell, or `None` where no ribbon light is laid.
    #[must_use]
    pub fn at(&self, row: u16, col: u16) -> Option<f32> {
        let lane = self.lanes.iter().find(|l| l.row == row)?;
        let t = *lane.t.get(usize::from(col))?;
        if t.is_nan() { None } else { Some(t) }
    }

    /// True when no row carries field light — one of the three pools
    /// `needs_frame_cadence()` ORs (§18's idle → zero).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.lanes.iter().all(|l| l.touched.is_empty())
    }

    /// Rows carrying field light this frame — the "linear in LIVE ROWS" term
    /// of every lookup.
    #[must_use]
    pub fn live_rows(&self) -> usize {
        self.lanes.len()
    }

    /// Begin a frame: RETIRE the lanes for rows that carried no light last
    /// frame into the spare pool, and clear the rest over their own `touched`
    /// list.
    ///
    /// The prune is what keeps "linear in LIVE ROWS" true: without it a
    /// session that has typed on a thousand rows carries a thousand lanes and
    /// every `field_at` walks them all — v1's linear scan wearing a different
    /// hat. A grid resize drops every lane, live and spare, since none of them
    /// is the right width any more.
    fn begin(&mut self, cols: u16) {
        if self.cols != cols {
            self.cols = cols;
            self.lanes.clear();
            self.spare.clear();
            return;
        }
        let mut i = 0;
        while i < self.lanes.len() {
            if self.lanes[i].touched.is_empty() {
                let dark = self.lanes.swap_remove(i);
                self.spare.push(dark);
            } else {
                i += 1;
            }
        }
        for lane in &mut self.lanes {
            lane.clear();
        }
    }

    /// The lane for `row`, taken from the spare pool if this row is newly lit
    /// and minted only when the pool is empty — the one allocation, paid once
    /// per "most rows ever live at once", never per frame.
    fn lane_mut(&mut self, row: u16) -> &mut FieldLane {
        if let Some(i) = self.lanes.iter().position(|l| l.row == row) {
            return &mut self.lanes[i];
        }
        let mut lane = self.spare.pop().unwrap_or_default();
        lane.row = row;
        if lane.t.len() != usize::from(self.cols) {
            lane.resize(self.cols);
        }
        self.lanes.push(lane);
        self.lanes.last_mut().expect("just pushed")
    }

    /// Drop every lane, live and spare.
    fn reset(&mut self) {
        self.lanes.clear();
        self.spare.clear();
    }

    /// Move every lane with a scroll of `rows` (seam point 12): a lane whose
    /// row leaves the grid retires into the spare pool, cleared over its own
    /// `touched` list — the O(marks) reset, and no lane is minted again on
    /// the next plan; the rest keep their columns and take the new row, so
    /// [`FieldIndex::at`] answers for the MOVED cell between the scroll and
    /// the next plan, exactly as a newest-first scan of the translated pool
    /// would.
    fn translate(&mut self, rows: u16) {
        let mut i = 0;
        while i < self.lanes.len() {
            if let Some(row) = self.lanes[i].row.checked_sub(rows) {
                self.lanes[i].row = row;
                i += 1;
            } else {
                let mut dark = self.lanes.swap_remove(i);
                dark.clear();
                self.spare.push(dark);
            }
        }
    }

    /// [`FieldIndex::translate`]'s ROW-BAND twin (seam point 12, the band
    /// path): a lane on a row outside `top..=bottom` keeps its row, a lane
    /// inside takes its row moved by `delta`, and a lane whose row leaves the
    /// band retires into the spare pool cleared over its own `touched` list
    /// — the same O(marks) retirement, so no lane is minted again on the next
    /// plan and [`FieldIndex::at`] answers for the MOVED cell between the
    /// band move and the next plan (the law [`crate::cursor_glow::band_row`]
    /// states; the edge row is a real cell that does not own this light).
    fn translate_band(&mut self, top: u16, bottom: u16, delta: i16) {
        let mut i = 0;
        while i < self.lanes.len() {
            if let Some(row) = band_row(self.lanes[i].row, top, bottom, delta) {
                self.lanes[i].row = row;
                i += 1;
            } else {
                let mut dark = self.lanes.swap_remove(i);
                dark.clear();
                self.spare.push(dark);
            }
        }
    }

    /// Write one cell's field. **FIRST WRITER WINS**, and the plan feeds this
    /// NEWEST-FIRST — which is the index's whole contract, stated in exactly
    /// the terms v1's reverse scan of `sparks` answered it in: the newest cell
    /// at a column owns that column, and an older cell it shadows can never be
    /// seen and is never asked about.
    fn set(&mut self, row: u16, col: u16, t: f32) {
        let lane = self.lane_mut(row);
        let Some(slot) = lane.t.get_mut(usize::from(col)) else {
            return;
        };
        if !slot.is_nan() {
            return;
        }
        lane.touched.push(col);
        *slot = t;
    }
}

// ===========================================================================
// The bed's ink (§3.1, §3.2, §3.3)
// ===========================================================================

/// One sRGB channel, 0..1, through the EOTF to LINEAR light.
fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.040_45 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// The WCAG relative luminance of an `0x00RRGGBB` colour — the coordinate the
/// 5.25:1 bar is stated in, and therefore the only honest coordinate for a
/// ceiling that claims to keep it.
#[must_use]
pub fn relative_luminance(rgb: u32) -> f32 {
    let chan = |sh: u32| srgb_to_linear(((rgb >> sh) & 0xff) as f32 / 255.0);
    0.2126 * chan(16) + 0.7152 * chan(8) + 0.0722 * chan(0)
}

/// **THE BED'S ONE CEILING**, in relative luminance: the brightest a
/// source-over ribbon may composite and still leave `theme_fg` legible over it
/// at [`BODY_CONTRAST_BAR`] (plus [`BODY_CONTRAST_GUARD`]).
///
/// This is §19.1's "one ceiling + the ledger" made arithmetic. v1 carried a
/// 33-entry per-position coverage table BESIDE a byte cap — two ceilings for
/// one bed, solved by two different emitters, and they disagreed. There is one
/// bar here, it is stated in the coordinate the bar is actually written in,
/// and [`bed_ink`] is what spends it.
///
/// **A FUNCTION OF THE FOREGROUND ALONE.** [`BED_LUMA_MIN`] is the one
/// documented exception under it ("below this the ribbon stops obeying the
/// bar and simply takes the dimmest light that still reads") and
/// [`BED_LUMA_MAX`] the bar's own answer at a white foreground. The theme's
/// GROUND is not an input: a floor against it was tried and removed (see the
/// note above `BED_LUMA_MIN`'s neighbours) because wherever it exceeded the
/// bar it put the bed over the luminance the text can bear.
#[must_use]
pub fn bed_luma_budget(theme_fg: u32) -> f32 {
    let bar =
        (relative_luminance(theme_fg) + 0.05) / (BODY_CONTRAST_BAR + BODY_CONTRAST_GUARD) - 0.05;
    // `clamp` is total here: both bounds are finite constants with
    // `BED_LUMA_MIN < BED_LUMA_MAX`, and `relative_luminance` of a byte
    // triple is finite.
    bar.clamp(BED_LUMA_MIN, BED_LUMA_MAX)
}

/// Every channel scaled by `k` — a uniform gamma-space scale, which is the one
/// operation that leaves a colour's channel RATIOS (its hue and saturation)
/// exactly where they were.
fn scale_rgb(rgb: u32, k: f32) -> u32 {
    let ch = |sh: u32| ((((rgb >> sh) & 0xff) as f32 * k) + 0.5).clamp(0.0, 255.0) as u32;
    (ch(16) << 16) | (ch(8) << 8) | ch(0)
}

/// A walk toward WHITE by `w`: `c + w·(255 − c)`. Every channel DIFFERENCE
/// survives unscaled, so the hue direction is untouched and only the
/// saturation falls — the same move v1's bed glass floor made, for the same
/// reason: a dark arc colour over a dark ground is a navy smudge, not a stop.
/// Since 2026-09-09 it is the LAST resort of [`onto_luma`], taken only from a
/// stop already at full value: white is the one currency that buys light at
/// the price of chroma, and the owner's "black gaps" were that price paid
/// where it did not have to be.
fn toward_white(rgb: u32, w: f32) -> u32 {
    let ch = |sh: u32| {
        let c = ((rgb >> sh) & 0xff) as f32;
        ((c + w * (255.0 - c)) + 0.5).clamp(0.0, 255.0) as u32
    };
    (ch(16) << 16) | (ch(8) << 8) | ch(0)
}

/// Bisect a monotone one-parameter colour family for the brightest member
/// UNDER `target` relative luminance. 24 halvings resolve the parameter to
/// under 1e-7, which is finer than the byte quantization it feeds — and the
/// answer is `family(lo)`, the last parameter whose bytes measured under the
/// target, never the midpoint: the family is a step function of bytes, so
/// the midpoint's bytes can land a level OVER the target, which is exactly
/// the rounding the old 0.15 [`BODY_CONTRAST_GUARD`] was paying for. The
/// invariant `Y(family(lo)) < target` holds from `lo = 0` (black for a scale,
/// the colour itself for a walk toward white from under the target) and is
/// kept by every halving.
fn solve_for_luma(family: impl Fn(f32) -> u32, target: f32) -> u32 {
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    for _ in 0..24 {
        let mid = 0.5 * (lo + hi);
        if relative_luminance(family(mid)) < target {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    family(lo)
}

/// **A STOP PUT ON ONE RELATIVE LUMINANCE AT THE MOST CHROMA THAT LUMINANCE
/// ALLOWS** — the one family the bed and the hot edge both solve over.
///
/// One monotone family, bisected by [`solve_for_luma`]: from black, the stop
/// scaled UP through itself to its own hue at FULL VALUE (`k = 255 / max`,
/// the brightest colour with exactly this hue and this saturation), and only
/// past that point — only for a target the hue cannot reach at full value —
/// a walk toward white. The recipe this replaces (2026-09-09) walked toward
/// white from the stop ITSELF, so a dark stop paid in chroma for light it
/// could have had in value: indigo `#4B0082` at the bar's `Y 0.097` came out
/// `(118, 60, 160)`, `S 0.61`, a grey lavender — the owner's "black gap".
/// Through this family it is `(127, 0, 219)`, `S 0.98`: the same hue, the
/// same luminance, the same contrast under any text, and every bit of the
/// chroma the hue owns. The fg-over-ribbon bar is a function of `Y` alone,
/// so this costs it nothing, on any theme.
///
/// Blue `#0000FF` is the one anchor whose full value is still under the bar
/// (`Y 0.072`), so it alone still takes some white — `(45, 45, 255)`,
/// `S 0.82`, exactly what it took before, because it was already at full
/// value. A stop BRIGHTER than the target scales DOWN, byte for byte as
/// before (its `k` lands under `1`): red, orange, yellow, green and every
/// warm mid are untouched by this change, and the bar-side pins beside them
/// (`the_dimmest_stop_…`, `a_bright_foreground_s_bed_…`) read the same
/// numbers. A black (no hue to spend) walks toward white as it always did;
/// the arc has none.
fn onto_luma(rgb: u32, target: f32) -> u32 {
    let mx = ((rgb >> 16) & 0xff).max((rgb >> 8) & 0xff).max(rgb & 0xff);
    if mx == 0 {
        return solve_for_luma(|w| toward_white(rgb, w), target);
    }
    let k_full = 255.0 / mx as f32;
    let full = scale_rgb(rgb, k_full);
    if relative_luminance(full) >= target {
        solve_for_luma(|k| scale_rgb(rgb, k * k_full), target)
    } else {
        solve_for_luma(|w| toward_white(full, w), target)
    }
}

/// **THE BED'S ON-GLASS INK** — an arc colour put on ONE composited luminance,
/// `budget` (see [`bed_luma_budget`]), at the most chroma that luminance
/// allows.
///
/// Two moves, one bar:
///
/// * [`BED_SAT_FLOOR`] first — the crossing's authored chroma dip is given
///   back at the bed's own read, hue and value untouched;
/// * then [`onto_luma`] — a colour BRIGHTER than the budget (red, yellow,
///   green) is scaled down uniformly, so its hue is bit-exact and only its
///   value moves; a colour DARKER than it (blue, indigo, violet) is lifted
///   through its own hue to full value and only then toward white, so it
///   arrives as a vivid stop instead of the navy smudge (v1) or the grey
///   lavender (v2 before 2026-09-09) the two measured defects report.
///
/// The consequence is that **every stop of the arc composites at the same
/// weight**, and one coverage ceiling is legal for the whole arc — which is
/// why the 33-entry table §19.1 deletes is not needed and is not carried. That
/// is the NAVY half of the measured "dim and muddy" defect (a dark stop as a
/// smudge) and not the OLIVE half: the weight is the bar's own, and on the
/// default theme a warm stop at that weight is `(80, 80, 3)` composited — the
/// luminance v1's table produced, with more chroma. A brighter warm mid is a
/// ruling on the bar (module doc: the bar STANDS, and the bed sits AT it
/// since 2026-09-08), not on this recipe.
///
/// It also makes the per-hue "equal-ledge" core law v1 needed unnecessary: the
/// per-row melt step is proportional to the position's peak premultiplied
/// level, and after this recipe every position has the same one.
#[must_use]
pub fn bed_ink(rgb: u32, budget: f32) -> u32 {
    onto_luma(spectrum_with_min_saturation(rgb, BED_SAT_FLOOR), budget)
}

/// **THE WCAG CONTRAST OF ONE CHANNEL**, both bytes read as a monochrome
/// light — the per-channel form of the ratio [`relative_luminance`] states
/// the body's own [`BODY_CONTRAST_BAR`] in, and the coordinate
/// [`SEEN_CONTRAST`] is stated in.
///
/// Per CHANNEL and not over the whole colour, because the bed equalises the
/// arc's LUMINANCE ([`bed_ink`]) and a whole-colour luminance test therefore
/// cannot tell two stops of the bed apart at all: at one luminance a violet
/// stop is `#0019FD` and a warm one `#585000`, and a luminance floor that
/// leaves the warm one readable puts the violet one out at a blue channel of
/// 186 — the band cut off while it is still bright. The channel that carries
/// the hue is what the eye picks out of the ground, and what the census
/// measures, so it is what is measured here.
fn channel_contrast(a: f32, b: f32) -> f32 {
    let (a, b) = (srgb_to_linear(a / 255.0), srgb_to_linear(b / 255.0));
    let (hi, lo) = if a >= b { (a, b) } else { (b, a) };
    (hi + 0.05) / (lo + 0.05)
}

/// **IS THIS COMPOSITED PIXEL STILL A STOP?** — [`SEEN_CONTRAST`] of
/// contrast against the ground in the channel that stands furthest from it
/// ([`channel_contrast`]) and [`SEEN_SPREAD`] bytes of channel spread. Both
/// halves are needed: the contrast alone passes a grey smudge, the spread
/// alone passes the ground itself on a strongly tinted theme.
///
/// GROUND-INDEPENDENT by construction: the level half is a ratio against
/// this theme's own ground and the chroma half is a property of the pixel,
/// so neither half carries a byte count that is only true of one background.
#[must_use]
fn seen(rgb: u32, bg: u32) -> bool {
    let ch = |c: u32, sh: u32| f32::from(((c >> sh) & 0xff) as u8);
    let (r, g, b) = (ch(rgb, 16), ch(rgb, 8), ch(rgb, 0));
    let stand = channel_contrast(r, ch(bg, 16))
        .max(channel_contrast(g, ch(bg, 8)))
        .max(channel_contrast(b, ch(bg, 0)));
    let spread = r.max(g).max(b) - r.min(g).min(b);
    stand >= SEEN_CONTRAST && spread >= SEEN_SPREAD
}

/// **THE COVERAGE AT WHICH A STOP GOES OUT** — the smallest alpha BYTE at
/// which `ink` composited over `bg` still passes [`seen`]; `255` for an ink
/// no coverage can make read.
///
/// This is the whole arithmetic behind the ember's per-stop floor. The bed
/// puts every stop on ONE composited LUMINANCE ([`bed_ink`], the legibility
/// bar) — it does not put them on one DISTANCE FROM THE GROUND, and it
/// cannot: at the bar the warm third's ink is a dark olive whose brightest
/// channel is around 91, where blue's is 255. Fading every stop's alpha by
/// the same factor therefore does NOT take them out together — each loses
/// the same FRACTION of a very different distance — and the warm stops
/// cross this floor while their neighbours are still bright, which is a
/// dark run opening inside a band that is still lit. Measured on the
/// default theme by `the_warm_stops_go_out_at_several_times_the_coverage_…`:
/// chartreuse goes out at 188 of the bed's 236 and orange at 137, where blue
/// and violet go out at 52 — a spread of 3.6x.
///
/// Solved by a scan over the 256 alphas the renderer can actually ask for,
/// with the renderer's own compositor, once per theme into
/// [`BedInkLut::seen`]. The scan runs DOWN from full and answers "one over
/// the highest alpha that fails", not "the first alpha that passes": the
/// floor has to be a level the stop reads at and at every coverage ABOVE it,
/// and only the downward form says that without an argument about the shape
/// of [`seen`] in the alpha. `256` — over any coverage the bed can spend —
/// for an ink no coverage can make read, which [`Ribbon::ember_floor_of`]
/// turns into "hold at full until the pool clears".
#[must_use]
fn seen_floor(ink: u32, bg: u32) -> f32 {
    (1..=255u16)
        .rev()
        .map(|a| u8::try_from(a).unwrap_or(255))
        .find(|&a| !seen(over_premul(bg, premul_rgb(ink, a), a), bg))
        .map_or(1.0, |a| f32::from(a) + 1.0)
}

/// **THE CRISP EDGE'S REACH, PRICED AT BIRTH** — how many cells back the
/// hairline of a cell laid on this tick may run ([`Cell::edge_cells`]).
///
/// Two things stretch it, and it takes the longer of them:
///
/// * **THE SURGE** ([`super::spine::Spine::surge`]) — a key whose birth price
///   exceeded the previous key's by [`super::spine::SURGE_MIN_RISE`], or a key
///   the follower is slamming for: a change of speed wears a longer streak,
///   in full at [`super::spine::SURGE_FULL_RISE`]. `3 + 4·Δ/0.5` capped at 5,
///   which is exactly `HOT_EDGE_CELLS + (MAX − HOT_EDGE_CELLS)·surge`.
/// * **THE OPEN THEME** ([`super::Flow::heat`]) — flow raises the FLOOR from
///   3 to 5 as it opens, so an unbroken run's every key wears the long edge
///   and the hand's own speed still shows above it.
///
/// At `surge = 0` and `heat = 0` this is [`HOT_EDGE_CELLS`] exactly — the
/// value the constant had before flow, and the reason a cold frame is
/// byte-identical.
#[inline]
#[must_use]
pub fn edge_cells(surge: f32, heat: f32) -> f32 {
    let stretch = clamp01(surge).max(clamp01(heat));
    HOT_EDGE_CELLS + (HOT_EDGE_CELLS_MAX - HOT_EDGE_CELLS) * stretch
}

/// **THE HOT EDGE'S INK** — the stop, lifted until it is at least
/// [`HOT_EDGE_LUMA_FLOOR`] luminous ([`onto_luma`]: through its own hue at
/// full value, then toward white); a stop already brighter is carried pure.
/// The bed's recipe equalizes the arc DOWN onto one legibility budget; this
/// one equalizes it UP onto one heat, and for the same reason in the other
/// direction — a blue hairline at blue's own `Y 0.072` over a blue bed whose
/// blue channel is already saturated adds nothing the eye can see, and
/// "brighter than the bed" has to be true of every stop or the edge reads as
/// speed only on the warm half of the arc. It takes [`BED_SAT_FLOOR`] too, so
/// the hairline over the seam is a teal line over a teal bed, not a grey one.
///
/// Additive light, so it is not under the bed's bar: the hairline is 1 px on
/// the ribbon's top edge, priced by [`HOT_EDGE_COV_MAX`] inside the
/// transient cap, and ledger-held over the row above's descenders (§4.1).
#[must_use]
pub fn hot_edge_ink(rgb: u32) -> u32 {
    let rgb = spectrum_with_min_saturation(rgb, BED_SAT_FLOOR);
    if relative_luminance(rgb) >= HOT_EDGE_LUMA_FLOOR {
        rgb
    } else {
        onto_luma(rgb, HOT_EDGE_LUMA_FLOOR)
    }
}

/// **THE VIVID RAIL'S INK** (2026-09-13) — the stop at FULL VALUE, put
/// inside the band `[RAIL_LUMA_FLOOR, RAIL_LUMA_CEIL]`: lifted toward white
/// to the floor where its hue cannot carry that much light (blue, indigo,
/// violet), scaled DOWN through its own hue to the ceiling where it carries
/// more than L5 allows beside the caret (yellow, green, orange), carried
/// pure between (red). The bed's recipe equalizes the arc DOWN onto the
/// legibility bar (yellow to an olive `(77, 77, 2)` on the owner's glass);
/// this one has no letter to protect — the rail lies where no letter can be
/// — and answers to the caret's floor instead ([`RAIL_LUMA_CEIL`]): yellow
/// `(149, 149, 0)`, green `(0, 168, 0)`, orange `(227, 113, 0)`, red
/// `(255, 0, 0)`, blue `(103, 103, 255)`. The green→blue crossing is
/// authored under full value (a mix of two anchors, `(0, 128, 128)`-ish), so
/// it is scaled up through its own hue first (`scale_rgb`, hue and
/// saturation exact) and takes [`BED_SAT_FLOOR`] like the bed does, so the
/// seam is a teal and not a grey.
///
/// A pure function of the arc position, solved once per theme into
/// [`BedInkLut`]'s third table and read with one lerp.
#[must_use]
pub fn rail_ink(rgb: u32) -> u32 {
    let rgb = spectrum_with_min_saturation(rgb, BED_SAT_FLOOR);
    let mx = ((rgb >> 16) & 0xff).max((rgb >> 8) & 0xff).max(rgb & 0xff);
    if mx == 0 {
        return onto_luma(rgb, RAIL_LUMA_FLOOR);
    }
    let full = scale_rgb(rgb, 255.0 / mx as f32);
    let y = relative_luminance(full);
    if y < RAIL_LUMA_FLOOR {
        onto_luma(full, RAIL_LUMA_FLOOR)
    } else if y > RAIL_LUMA_CEIL {
        onto_luma(full, RAIL_LUMA_CEIL)
    } else {
        full
    }
}

/// The light theme's compositing ROLE for the body (§3.3, L6). The tall
/// spelling stands over letterforms, so it takes the conservative ink and the
/// shared legibility ceiling; the underline spelling is provably confined to
/// the leading, so it may be vivid and go to the rail's own ceiling.
///
/// `pub(crate)` because `InkRole` is: the light-ink recipe is the family's,
/// not this module's, and a producer outside the crate has no business naming
/// a compositing role it cannot spell.
#[must_use]
pub(crate) fn light_role(cfg: &Config) -> InkRole {
    if cfg.ribbon_tall {
        InkRole::OverText
    } else {
        InkRole::Leading
    }
}

/// The body's published alpha ceiling for this theme — [`UNDER_COV_CAP`] on
/// a dark one, the compositing role's on a light one (§3.4). One spelling,
/// because [`Ribbon::cov_of`] and [`Ribbon::ember_floor`] must price the
/// same cell against the same ceiling.
#[must_use]
fn cov_cap(cfg: &Config) -> f32 {
    if cfg.dark_theme {
        UNDER_COV_CAP
    } else {
        light_role(cfg).alpha_cap()
    }
}

/// The bed's ink table for ONE theme — resident, rebuilt only when the theme
/// or the spelling changes.
///
/// [`bed_ink`] is a bisection over `powf`; there are up to `SLABS_PER_CELL`
/// vertices per cell per frame and it is a pure function of the arc position,
/// so it is solved once into this table and read with one lerp (§18: no dead
/// work, and nothing expensive on the steady frame path).
#[derive(Clone, Debug, Default)]
pub struct BedInkLut {
    /// `(theme_fg, theme_bg, dark_theme, ribbon_tall)` the table was solved
    /// for. A change in any of the four invalidates it.
    key: Option<(u32, u32, bool, bool)>,
    /// [`BED_INK_LUT_LEN`] entries over the folded arc `[0, 1]`.
    lut: Vec<u32>,
    /// The hot edge's twin table ([`hot_edge_ink`]), same positions — solved
    /// beside the bed's so the hairline costs one lerp per vertex too.
    hot: Vec<u32>,
    /// The vivid rail's table ([`rail_ink`]), same positions (2026-09-13).
    rail: Vec<u32>,
    /// **THE COLOUR FLOOR'S TABLE** ([`seen_floor`] of the BED's ink over
    /// this theme's ground, in alpha BYTES), same positions (2026-09-14).
    /// The focus ember reads it so that every stop leaves on its own
    /// extinction rather than on one shared fraction of very different
    /// distances from the ground.
    seen: Vec<f32>,
}

impl BedInkLut {
    /// Rebuild if the theme moved. Deterministic and clockless.
    fn sync(&mut self, cfg: &Config) {
        let key = (cfg.theme_fg, cfg.theme_bg, cfg.dark_theme, cfg.ribbon_tall);
        if self.key == Some(key) && self.lut.len() == BED_INK_LUT_LEN {
            return;
        }
        let budget = bed_luma_budget(cfg.theme_fg);
        let role = light_role(cfg);
        self.lut.clear();
        self.lut.reserve(BED_INK_LUT_LEN);
        self.hot.clear();
        self.hot.reserve(BED_INK_LUT_LEN);
        self.rail.clear();
        self.rail.reserve(BED_INK_LUT_LEN);
        self.seen.clear();
        self.seen.reserve(BED_INK_LUT_LEN);
        for i in 0..BED_INK_LUT_LEN {
            let x = i as f32 / (BED_INK_LUT_LEN - 1) as f32;
            let arc = spectrum(x);
            let bed = if cfg.dark_theme {
                bed_ink(arc, budget)
            } else {
                role.ink(arc)
            };
            self.lut.push(bed);
            self.hot.push(hot_edge_ink(arc));
            self.rail.push(rail_ink(arc));
            self.seen.push(seen_floor(bed, cfg.theme_bg));
        }
        self.key = Some(key);
    }

    /// The ink at walk position `t`. The walk is unbounded, so it is folded
    /// through `meteor::tri` — the SAME reflection §6.4's arc uses, which is
    /// what makes "the station under the caret equals the caret's stop" true
    /// by construction rather than by coincidence.
    #[must_use]
    pub fn at(&self, t: f32) -> u32 {
        Self::sample(&self.lut, t)
    }

    /// The hot edge's ink at walk position `t` — the same fold, the hot
    /// table ([`hot_edge_ink`]).
    #[must_use]
    pub fn hot_at(&self, t: f32) -> u32 {
        Self::sample(&self.hot, t)
    }

    /// The vivid rail's ink at walk position `t` — the same fold, the rail
    /// table ([`rail_ink`]).
    #[must_use]
    pub fn rail_at(&self, t: f32) -> u32 {
        Self::sample(&self.rail, t)
    }

    /// **THE COLOUR FLOOR AT WALK POSITION `t`** — the alpha byte under
    /// which this stop's bed stops reading as a stop over the live theme's
    /// ground ([`seen_floor`]), the same fold as the ink tables. `0` on an
    /// unsynced table, which is "no floor" — exactly the curve this module
    /// had before the table existed.
    ///
    /// The BRACKETING entries' MAX, not a lerp: the ink at `t` is itself a
    /// lerp of the two entries, and `seen_floor` is not linear in the ink, so
    /// a lerped floor can land a byte or two UNDER the true extinction of the
    /// colour actually drawn — and a floor under the true one is the one
    /// error that matters here, because it is a stop going dark under its
    /// neighbours. The table's step is 1/128th of the arc and the floor moves
    /// about two bytes across it, so the max costs at most that and is never
    /// short.
    #[must_use]
    pub fn seen_floor_at(&self, t: f32) -> f32 {
        if self.seen.is_empty() {
            return 0.0;
        }
        let x = clamp01(tri(t)) * (BED_INK_LUT_LEN - 1) as f32;
        let i = (x as usize).min(BED_INK_LUT_LEN - 1);
        let j = (i + 1).min(BED_INK_LUT_LEN - 1);
        self.seen[i].max(self.seen[j])
    }

    /// One lerp on `table` at the folded position of `t`; `0` on an unsynced
    /// table.
    fn sample(table: &[u32], t: f32) -> u32 {
        if table.is_empty() {
            return 0;
        }
        let x = clamp01(tri(t)) * (BED_INK_LUT_LEN - 1) as f32;
        let i = (x as usize).min(BED_INK_LUT_LEN - 1);
        let j = (i + 1).min(BED_INK_LUT_LEN - 1);
        lerp_rgb(table[i], table[j], x - i as f32)
    }
}

// ===========================================================================
// The walk (C2)
// ===========================================================================

/// **THE CLASSIC WALK**, `t` at `d` cells from the mark's first cell: `d/16`
/// over the first [`WALK_FAST_CELLS`] cells so a short word already shows
/// several stops, then [`WALK_LAY_RATE`] per cell so a long line does not
/// spin through the arc.
#[must_use]
pub fn walk_t(d: f32) -> f32 {
    if !d.is_finite() {
        return 0.0;
    }
    if d <= WALK_FAST_CELLS {
        d / WALK_FAST_CELLS
    } else {
        1.0 + (d - WALK_FAST_CELLS) * WALK_LAY_RATE
    }
}

/// A cell's terminal approach to zero, read on its own clock (v1's
/// `rainbow_expiry_melt`): smoothstep over the last [`EXPIRY_MELT_SHARE`] of
/// life, raised to [`EXPIRY_MELT_GAMMA`]. Exactly `1.0` over the first 70 % of
/// the domain and exactly `0.0` at `u = 1`.
#[must_use]
pub fn expiry_melt(u: f32) -> f32 {
    let taper = smoothstep01((1.0 - clamp01(u)) / EXPIRY_MELT_SHARE);
    if taper >= 1.0 {
        1.0
    } else {
        taper.powf(EXPIRY_MELT_GAMMA)
    }
}

/// **THE FOCUS EMBER'S LEVEL, PER STOP** (2026-09-14) — the band's common
/// fade `f` (the `spend` curve, or the theme's one linear fade under
/// reduced motion) read INTO the stop's own range: it runs from exactly
/// `1.0` at the blur to exactly `floor` when the fade is spent, where
/// `floor` is the coverage at which THIS stop stops being seen
/// ([`seen_floor`], as a share of the cell's own full coverage).
///
/// **THE LAW IT KEEPS: THE BAND LEAVES AS ONE OBJECT.** A common multiplier
/// does not: the bed puts every stop on one composited luminance, not on one
/// distance from the ground, so at half coverage the warm third is already
/// under the colour floor while blue is four times above it — the band
/// splits into two lit pieces with a dark run between them, which is the
/// owner's "black gaps" arriving by a new route. Here every stop keeps the
/// whole of the fade it HAS: no stop can cross its own floor while the ember
/// still has level, because the fade's bottom IS that floor, and the ember's
/// end ([`Ribbon::retire`]) then takes every cell in the same frame, each
/// from its own just-visible level. One interval on every frame, and out
/// together.
///
/// Exactly `1.0` at the blur, so that frame is byte for byte what it was;
/// exactly `floor` when the fade is spent; non-increasing in `f`, so the
/// ember still never brightens. A cell whose floor is ABOVE its own coverage
/// (a stop the bar leaves barely readable even at the cap) simply holds what
/// it has and leaves with the rest — the clamp is on the LEVEL, never on the
/// floor, because a clamped floor would put that cell back on a schedule of
/// its own.
#[must_use]
pub fn ember_level(floor: f32, f: f32) -> f32 {
    let floor = floor.max(0.0);
    clamp01(floor + (1.0 - floor) * clamp01(f))
}

/// The ONE linear fade of the theme (§2.5, §6.11): under reduced motion a
/// mark's alpha runs to zero linearly over the last
/// [`REDUCED_MOTION_FADE_MS`] of its life. `remaining_s` is how long the mark
/// has left; `1.0` until the window opens, exactly `0.0` at the end.
#[must_use]
pub fn reduced_fade(remaining_s: f32) -> f32 {
    clamp01(remaining_s / (REDUCED_MOTION_FADE_MS / 1000.0))
}

/// Seconds until [`expiry_melt`] next takes a mark of `peak` levels down
/// one u8 level, `u` of the way through its `life_s` — the cadence law's
/// "next visible step" for the melt, in closed form through the
/// smoothstep's own inverse (`x = ½ − sin(asin(1 − 2y)/3)`). Before the
/// melt opens this is the instant of its first level drop, so a static
/// cell asks for exactly the frame its light first moves. `None` once
/// nothing is left to see.
#[must_use]
pub(crate) fn level_step_melt(peak: f32, u: f32, life_s: f32) -> Option<f32> {
    if !(peak.is_finite() && life_s.is_finite() && u.is_finite()) || peak <= 0.0 || life_s <= 0.0 {
        return None;
    }
    let u = clamp01(u);
    let level = (peak * expiry_melt(u)).round();
    if level < 1.0 {
        return None;
    }
    let taper = ((level - 0.5) / peak)
        .clamp(0.0, 1.0)
        .powf(1.0 / EXPIRY_MELT_GAMMA);
    let x = 0.5 - ((1.0 - 2.0 * taper).clamp(-1.0, 1.0).asin() / 3.0).sin();
    let u_next = 1.0 - EXPIRY_MELT_SHARE * x;
    Some(((u_next - u) * life_s).max(0.0))
}

// ===========================================================================
// The producer
// ===========================================================================

/// One boundary's planned sample before it becomes a [`Segment`]:
/// `(spine, up, dn, t, cov)`, in window px / walk units / coverage levels.
type Sample = (f32, f32, f32, f32, f32);

/// One contiguous planned run — a range of [`Ribbon::plan`] plus the columns
/// it covers, so [`Ribbon::band`] can answer a cell without a search and
/// [`Ribbon::emit`] can shed the run's TAIL when the budget runs out.
///
/// A run belongs to ONE cohort: [`Ribbon::build_runs`] breaks a run where the
/// cohort changes, so the exit swoosh of one cohort can never move the
/// boundaries of a neighbour that is still being typed.
#[derive(Clone, Copy, Debug)]
struct Run {
    row: u16,
    col0: u16,
    col1: u16,
    lo: usize,
    hi: usize,
    /// The head cell's column (see [`Ribbon::head_col`]).
    head_col: u16,
    /// Index into [`Ribbon::plan`] of the HEAD boundary — the head cell's
    /// RIGHT edge, where the hot edge ends.
    ///
    /// The head is always the run's right-hand side and the tail its left:
    /// a run is one cohort, and a cohort's walk is a monotone function of the
    /// column ([`Cohort::t_at`]), so the oldest light — the reach, the first
    /// typed cells — is always the left end. There is no "which end is the
    /// head" heuristic: one resolved from the head's POSITION flipped the tail
    /// onto the erased cells whenever a backspace carried the caret past the
    /// run's midpoint, and drew the hot edge over them.
    head: usize,
    /// True when the head cell is real, un-retracting light — the hot edge's
    /// "still wet at the hand" condition (§4.1). A run whose every cell is
    /// retracting (the whole word backspaced) has nothing wet in it.
    wet: bool,
    /// Whether the caret stands in or beside this run — the run the hand is
    /// on is emitted FIRST, so a saturated budget sheds the oldest row and
    /// never the head (§7.3: one key, one cell, one light).
    at_caret: bool,
    /// The newest typing cell's birth, the emit order's second key.
    born: Instant,
    /// **WHICH WAY THE STREAM RUNS** from the head: `−1` toward lower
    /// columns — every typed run and every rightward wake, whose tail is
    /// their left end — or `+1` toward higher columns, which is a LEFTWARD
    /// wake's (the caret landed on the run's FIRST cell and the corridor it
    /// crossed lies to its right; see [`Ribbon::head_col`]'s wake clause).
    /// The hot edge reaches from the head this way only — behind the hand,
    /// never over the cells it has erased — and [`Run::head`] is the run's
    /// `lo` boundary when it is `+1`.
    stream_dir: i8,
}

/// **THE ATTACH'S LEVEL** this frame — set by [`Ribbon::plan_run`] at the
/// head boundary of the run under the caret and read wherever the caret
/// cell's light is planned: the head boundary's own coverage (`base`), the
/// head cell's uniform attack (`uniform`, [`BIRTH_EDGE_FLOOR`] to 1 over
/// the 18 ms edge-in) and the probe the from-the-hand wipe runs on
/// ([`Ribbon::wipe_of`] — the head cell with its attack on its BIRTH
/// clock). [`AttachFloor::level`] at `dist` across the caret cell (`0` at
/// the head's edge, `1` at the block's far edge) is the light the attach
/// puts there: the caret cell's own segments when nothing is laid under the
/// caret, and the FLOOR under a LEAVING cell's light when one is — its own
/// run's erased cell draining after a Backspace, or a neighbour cohort's
/// cell retiring by content — so the block stands on the band while the
/// cell spends, and the frame that retires it changes no byte (review of
/// the attach, 2026-09-14: the erased cell's spend took the caret cell to
/// zero under the block, then the retirement switched the attach on at
/// full in one frame).
#[derive(Clone, Copy, Debug)]
struct AttachFloor {
    row: u16,
    /// The caret cell.
    col: u16,
    base: f32,
    uniform: f32,
    probe: Cell,
}

impl AttachFloor {
    /// The attach's light at `dist` across the caret cell.
    fn level(&self, rib: &Ribbon, ctx: &Ctx<'_>, dist: f32) -> f32 {
        self.base * self.uniform * rib.wipe_of(ctx, &self.probe, dist)
    }
}

/// THE RIBBON producer: the laid cells, their cohorts, this frame's plan, and
/// the field index every other producer reads.
///
/// The pool, the plan and the index are PRIVATE: the index answers "what a
/// newest-first scan of the pool would" only while nothing edits the pool
/// behind it, and the read-only accessors ([`Ribbon::cells`],
/// [`Ribbon::cohorts`], [`Ribbon::plan_segments`], [`Ribbon::field`]) are
/// what tests and the other producers read.
#[derive(Clone, Debug, Default)]
pub struct Ribbon {
    /// The live cells, resident and reused (§18: no allocation per frame).
    cells: Vec<Cell>,
    /// The live cohorts.
    cohorts: Vec<Cohort>,
    /// This frame's boundary samples, rebuilt by [`Ribbon::plan`] and consumed
    /// by [`Ribbon::emit`]. Resident scratch.
    plan: Vec<Segment>,
    /// This frame's `col → t` index.
    index: FieldIndex,
    /// Vertices per cell THIS frame planned at — [`SLABS_PER_CELL`] or fewer
    /// under budget pressure (see [`Ribbon::slabs_per_cell`]). Every index
    /// into [`Ribbon::plan`] is stated in it.
    slabs: usize,
    /// The next cohort id to mint.
    next_cohort: u32,
    /// The caret cell as last observed — the anchor `field()` resolves
    /// against (seam point 4).
    caret: Option<(u16, u16)>,
    /// This frame's runs into [`Ribbon::plan`], in EMIT order (the run at the
    /// caret first, then newest first). Resident scratch.
    runs: Vec<Run>,
    /// `(row, col, cell index)` sort scratch — the ONE sort per frame the
    /// index and the geometry share (§18).
    sorted: Vec<(u16, u16, u32)>,
    /// `RibbonVertex` scratch, taken and returned by `mem::take` so the emit
    /// path allocates nothing.
    verts: Vec<RibbonVertex>,
    /// The bed's ink table for the live theme.
    ink: BedInkLut,
    /// The theme GROUND the last planned frame composited over, stamped
    /// beside [`Ribbon::ink`]'s sync so [`Ribbon::ink_segments`] can answer
    /// what a boundary actually reads as without a `Ctx`.
    ground: u32,
    /// **WHAT THE LAST FRAME PUT ON THE GLASS**: how many of the quads that
    /// emit appended to `under` composite to something the glass census
    /// counts as band ink ([`reads_as_ink`]). Measured on the frame's own
    /// pixels rather than modelled from the plan — the bed is not the only
    /// stream on a ribbon row (the baseline strip's accent and the vivid
    /// rail ride with it), so a number derived from [`Segment::cov`] alone
    /// under-reports the end of a fall. `trail status` reports it as
    /// `ribbon_drawn=`.
    ink_quads: usize,
    /// Set while the window is unfocused: the ribbon embers out over
    /// [`FOCUS_EMBER_S`] on `spend` and nothing sounds (§8.2). Cleared by
    /// [`Ribbon::retire`] with the pool once the ember has burnt out, and by
    /// [`Ribbon::regain_focus`] (into a rearm if still burning).
    ember_at: Option<Instant>,
    /// Focus REGAINED while the ember was still burning: `(when, fade)`, the
    /// ember's own common fade at that instant, which [`ember_level`] reads
    /// into each stop's own range. The light comes back up from the level it
    /// had through the one sanctioned attack, `edge-in`, rather than snapping
    /// to full — v1's `rearm`. The FADE is stamped rather than one level,
    /// because the level is per stop (2026-09-14).
    rearm: Option<(Instant, f32)>,
    /// The reduced-motion posture of the last frame planned, cached for the
    /// cadence law (seam point 9 is asked with a clock and nothing else).
    reduced: bool,
    /// The focused pane's `(first column, width)` — the edges [`Ribbon::lay`]
    /// wraps a fold at — or `None` for the whole grid. Handed down by the
    /// engine every tick ([`super::Engine::set_pane`]).
    pane: Option<(u16, u16)>,
    /// **THE ATTACH** of the last frame planned — the `(row, col)` of the
    /// caret cell the band was continued under ([`Ribbon::plan_run`]), or
    /// `None`. Read by [`Ribbon::lay_cell`]: a typed cell laid where the
    /// attach stood INHERITS its light — its attack is already spent — so
    /// the key that turns the caret cell into the head cell moves no byte
    /// down; the from-the-hand wipe then belongs to the NEXT attach, one
    /// cell on under the block. Read again by [`Ribbon::build`]: a LEAVING
    /// cell the attach floored last frame that the caret has since left —
    /// a held Backspace moving the block off the cell it erased — restarts
    /// its retract at that frame, so what the block leaves behind drains
    /// from the level it showed under the block instead of dropping to
    /// wherever its own clock had got to.
    attach: Option<(u16, u16)>,
    /// The attach's level this frame ([`AttachFloor`]), or `None` while no
    /// run stands under the caret. Set at the head boundary as the runs are
    /// planned, read by the caret cell's slabs in the same run or the
    /// neighbour run to its right — which is planned after it (`sorted` is
    /// row-major) — and by the attach's own segments.
    attach_floor: Option<AttachFloor>,
    /// The cell height of the last frame planned, px.
    cell_h: f32,
    /// The WEDGE's full travel in px — `(dn_ch − DN_FLOOR_CH)·ch` of the
    /// last frame's profile — the distance the mark's lower edge closes as
    /// the spine releases ([`Ribbon::settle_step_s`]).
    wedge_px: f32,
    /// Erases the hand has made whose RETREAT the ribbon has not yet seen.
    /// An `Erase` is replayed against the tick's caret; when its echo lands
    /// on a LATER tick that caret still stands past the erased glyph and
    /// `retract_suffix` finds nothing to retract, so the erased cell would
    /// stay lit under the blank (measured: a held Backspace retracting in
    /// pairs on alternate keys). The count is spent by the next
    /// typed-licensed same-row retreat, which retracts from ITS landing; a
    /// typed lay clears it.
    pending_erase: u16,
    /// The cell of the last typed Space, `(row, col)` — what tells a
    /// re-anchor how long the word an app moved to the next row was.
    last_space: Option<(u16, u16)>,
    /// A re-anchor's moved word, waiting for the landing that reveals where
    /// it went (see [`Relocate`]).
    relocate: Option<Relocate>,
    /// The walk a finished swoosh left behind on its row, so a same-row
    /// rebirth inside [`CHAIN_GAP_MAX`] continues the arc instead of
    /// re-anchoring at red — `Cohort::t0`'s documented continuation, which
    /// had nothing to read once `retire` dropped the cohort with its cells.
    last_walk: Option<LastWalk>,
    /// **THE HAND'S TEMPO**, ms — the inter-onset EMA the melody runs
    /// ([`super::timing::ioi_next`]), run here on the same typed keys so
    /// the ribbon's grace is [`super::timing::phrase_rest_ms`] of the same
    /// number the song rests on (S5). `0.0` before the first key reads as
    /// the default tempo ([`Ribbon::rest_s`]).
    ioi_ms: f32,
    /// The clock of the last key that HELD a phrase — a typed key, an erase
    /// or a kill: the rest is measured from it, exactly as the melody's
    /// `last_key_ms` is any touch.
    last_key_at: Option<Instant>,
    /// **THE OPEN PHRASE** (§2.3), or `None` between phrases — after a rest
    /// has passed since [`Ribbon::last_key_at`], a Return or a line kill.
    /// The next typed lay opens the next one.
    phrase_open: Option<u32>,
    /// The next phrase id to mint; `0` is never minted (it is "no phrase",
    /// a wake cohort's).
    next_phrase: u32,
    /// **THE CURTAIN IN PROGRESS**: when it fell. `None` when none is
    /// falling.
    ///
    /// The column each cohort is drawn toward is NOT here: it is frozen on
    /// the cohort itself ([`Cohort::retract_col`], with
    /// [`Cohort::drain_right_first`] beside it), by exactly the rule a
    /// natural retract freezes them with, and from the BAND'S OWN HAND
    /// rather than from [`Ribbon::caret`] — see [`Ribbon::curtain`].
    curtain: Option<Instant>,
}

/// **A RE-ANCHOR'S MOVED WORD** (2026-09-13, the owner: "the rainbow cursor
/// streak doesn't follow the new line down in claude code and codex").
///
/// A TUI composer (Claude Code's Ink box, Codex) re-wraps its whole box per
/// keystroke: the last word of the row moves down to the next visual row
/// and the caret lands after it. The seam licenses that as a TYPED
/// re-anchor and the ribbon sees one `Move` — no key ever laid the moved
/// word's cells on the new row, and its old cells stayed lit on the row the
/// hand left. The ribbon knows the word's length from the last typed Space
/// (`w` cells before the origin column), so it can relay the word where the
/// caret reveals it: `[landing − 1 − w, landing − 1)`, the wrap key's own
/// glyph at `landing − 1`. Ink's two-move rewrite lands the caret at the
/// box's inset FIRST (`landing − 1 − w` off the pane) and only then at the
/// word's end, in a move the seam refuses — so the relocation waits, and
/// the next same-row forward typed move on the row pays it from its origin.
#[derive(Clone, Copy, Debug)]
struct Relocate {
    /// The row the word moved to.
    row: u16,
    /// Cells the moved word has (without the wrap key's own glyph).
    w: u16,
    /// When the re-anchor was seen; a relocation older than
    /// [`RELOCATE_PATIENCE_S`] buys nothing.
    at: Instant,
}

/// How long a [`Relocate`] waits for the caret to reveal the moved word.
pub const RELOCATE_PATIENCE_S: f32 = 2.0;

/// The walk a retired cohort left on its row — see `Ribbon::last_walk`.
#[derive(Clone, Copy, Debug)]
struct LastWalk {
    col1: u16,
    anchor_col: u16,
    t0: f32,
    alive_at: Instant,
}

impl Ribbon {
    /// A ribbon with nothing laid.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The live cells, in pool (append) order. Read-only.
    #[must_use]
    pub fn cells(&self) -> &[Cell] {
        &self.cells
    }

    /// The live cohorts. Read-only.
    #[must_use]
    pub fn cohorts(&self) -> &[Cohort] {
        &self.cohorts
    }

    /// The instant `cell`'s expiry is measured from — see [`live_since`].
    #[must_use]
    pub fn live_since(&self, cell: &Cell) -> Instant {
        live_since(&self.cohorts, cell)
    }

    /// This frame's planned boundary samples, valid after [`Ribbon::plan`].
    /// Read-only.
    #[must_use]
    pub fn plan_segments(&self) -> &[Segment] {
        &self.plan
    }

    /// **THE FRAME'S VERTEX DENSITY** — how many [`Ribbon::plan_segments`]
    /// each cell of a run spans this frame, `1..=`[`SLABS_PER_CELL`]. Boundary
    /// `b` of a run whose first column is `col0` sits at
    /// `run.lo + (b − col0) · slabs_per_cell()`.
    ///
    /// The law (§18): the frame's `live cells × device rows × slabs` must fit
    /// [`RIBBON_QUAD_BUDGET`], and since `ribbon_beam` tiles at least one
    /// slab per vertex pair the density is the only thing that can give. It
    /// falls from three toward one as the live mark grows — a 76-cell hot line
    /// at retina plans at one — and the budget's truncation is reached only
    /// past that, where it sheds the OLDEST run's tail (see
    /// [`Ribbon::emit`]). Valid after [`Ribbon::plan`].
    #[must_use]
    pub fn slabs_per_cell(&self) -> usize {
        self.slabs.clamp(1, SLABS_PER_CELL)
    }

    /// The caret cell as last observed, or `None` before any.
    #[must_use]
    pub fn caret(&self) -> Option<(u16, u16)> {
        self.caret
    }

    /// **THE TWO PROFILES** (§4). The whole tall/underline fork, as data.
    ///
    /// `dn_ch` is the reach below the spine AT FULL MOMENTUM UNDER THE HAND:
    /// the comet's [`COMET_DN_HAND_CH`] by default, the flat body's
    /// [`DN_TOP_CH`] under the `… flat` spelling (2026-09-13). Both
    /// spellings still differ above the spine and nowhere else.
    #[must_use]
    pub fn body_profile(cfg: &Config) -> BodyProfile {
        let dn_ch = if Self::comet(cfg) {
            COMET_DN_HAND_CH
        } else {
            DN_TOP_CH
        };
        if cfg.ribbon_tall {
            BodyProfile {
                up_ch: TALL_UP_CH,
                dn_ch,
                core_up_ch: TALL_CORE_CH,
                shoulder: SHOULDER_TALL,
            }
        } else {
            BodyProfile {
                up_ch: UNDERLINE_UP_CH,
                dn_ch,
                core_up_ch: LEAD_CH,
                shoulder: SHOULDER_UNDERLINE,
            }
        }
    }

    /// **THE GATE** (2026-09-13, `RAINBOW-KITTY-V2.md` §30): the comet body
    /// and the from-the-hand attack are the default; the `… flat` spelling
    /// ([`Config::ribbon_flat`]) restores the flat body byte for byte. One
    /// predicate, so the A/B is one line to flip.
    #[inline]
    #[must_use]
    pub fn comet(cfg: &Config) -> bool {
        !cfg.ribbon_flat
    }

    /// Whether the VIVID RAIL is drawn: the comet body on a DARK ground. The
    /// light fork composites source-over ink under the shared legibility
    /// ceiling (L6) and has no vivid recipe, so it keeps the bed alone
    /// below the baseline.
    #[inline]
    #[must_use]
    pub fn rail_lit(cfg: &Config) -> bool {
        Self::comet(cfg) && cfg.dark_theme
    }

    // -- ingest ------------------------------------------------------------

    /// Ingest one engine event at its own edge, BEFORE this frame's plan.
    ///
    /// [`Event::Typed`] extends the head cohort (or folds a wrap into a new
    /// one); [`Event::Erase`] retracts the SUFFIX of the row from the caret on
    /// — v1's law, kept: the lit run stays contiguous from the caret backward
    /// and nothing is ever removed from the middle, so no cell holds light
    /// over a glyph that has shifted; [`Event::Kill`] drains the same suffix,
    /// at least `cells` wide, farthest-first at `12·n + 240` ms (the Word /
    /// Line scope is the synth's, §8.2 — the drain is one law);
    /// [`Event::Move`] moves [`Ribbon::caret`] and then asks ONE question of
    /// a keyed same-row move — [`Ribbon::scrub`], the boundary between the
    /// two laws that answer it. A move that stays INSIDE the hand's own lit
    /// run is a SCRUB: it lays nothing, takes nothing over, abandons
    /// nothing, and HOLDS the cohorts it crossed (the owner's back-word /
    /// forward-word gesture on v0.85.0). A move that LEAVES the run — past
    /// its oldest cell, off its end onto dark ground, over light already
    /// melting, or to another row — is a REAL jump under a non-typed licence
    /// (a row change, or [`JUMP_MIN_CELLS`] or more on the row): it lays the
    /// WAKE of a same-row navigation or synthetic jump ([`Ribbon::wake`],
    /// R5 — the corridor it crossed, on the [`Layer::Over`] overlay, each
    /// cell born on the meteor's own flight clock, over a [`Layer::Base`]
    /// band whose cells it never touches) and then ABANDONS every other
    /// cohort straight into its retract beneath it (see
    /// [`Cohort::retract_start`]), draining from the origin end so the drain
    /// follows the meteor; a same-row hop under the jump floor with a
    /// navigation witness (an arrow) lays a shorter wake in a cohort of its
    /// own and abandons nothing — a click inside the word is an edit, and
    /// the swoosh's own grace handles it; a band already leaving is left on
    /// its own clock by either arm; a PTY-driven or return-licensed jump still only abandons
    /// (no credit, no wake); [`Event::Focus`] takes the 300 ms ember and, on
    /// regain inside it, the `edge-in` rearm; [`Event::ReducedMotion`] and
    /// [`Event::Return`] lay nothing. T1 holds: the wake is born only on an
    /// observed, licensed move whose witness was a key.
    ///
    /// Every event is applied against `ctx.caret`, the caret as the host
    /// observed it for THIS tick; a host that buffers two typed echoes into
    /// one tick must coalesce them into one `Typed { cells: 2 }`, because the
    /// caret each of them landed at is not carried on the event.
    pub fn on_event(&mut self, ev: &super::Event, at: Instant, ctx: &Ctx<'_>) {
        // UNDER A CURTAIN NOTHING IS LAID (§2.8): the cells on glass belong
        // to a coordinate space that has ended and are being drawn out; a
        // key, an echo, a jump in the new space waits for the curtain to
        // finish (0.24 s) — the caret mirror alone is kept.
        if self.curtain.is_some() {
            if let Event::Move { to, .. } = *ev {
                self.caret = Some(to);
            }
            return;
        }
        // Once the hand explicitly edits or leaves a released mark, a later
        // matching redraw cannot undo that decision. This changes no cell or
        // lifetime; it only revokes the content-release restoration token.
        let revoke = match *ev {
            Event::Erase | Event::Kill { .. } | Event::Return => Some(ctx.caret.0),
            Event::Rewrite { row, .. } => Some(row),
            Event::Move {
                from,
                licence: Licence::Nav | Licence::Return | Licence::Synthetic,
                ..
            } => Some(from.0),
            _ => None,
        };
        if let Some(row) = revoke {
            for cohort in &mut self.cohorts {
                if cohort.row == row {
                    cohort.release_clock = None;
                }
            }
        }
        match *ev {
            Event::Typed { cells, class, .. } => {
                // THE HAND IS ON IT (2026-09-13, the focus seam): a key
                // typed while the focus ember burns — a control-socket
                // agent typing into an unfocused window, the host's own
                // "live typed wake" — clears the ember and comes back up
                // from the level the light had reached, through `edge-in`,
                // exactly as a focus regain does. Without this arm every
                // cell laid under a burning ember was born at `spend(1) =
                // 0` and the unfocused window went dark for good.
                if self.ember_at.is_some() {
                    self.regain_focus(at);
                }
                // THE SCORE'S KEY (Rainbow Path v3 §2.1/§2.3): the tempo and
                // the phrase move before anything is laid, so the lay reads
                // the grace this key priced.
                self.note_key(at);
                self.pending_erase = 0;
                if class == TypedClass::Space {
                    let (row, col_end) = ctx.caret;
                    self.last_space = col_end.checked_sub(1).map(|c| (row, c));
                }
                self.lay(cells, at, ctx);
            }
            // AN ERASE OR A KILL HOLDS THE PHRASE (§2.3, S6): the hand is
            // on it. Before this a Backspace run started the swoosh from
            // the band's left end while the erase was still going (audit
            // s10) because only a lay refreshed a cohort. The hold is the
            // open phrase's cohorts ON THE CARET'S ROW — the row clause is
            // the shipped `hold_row`'s (`OnlyOwnerRenews`, the derived
            // `ribbon_row_hold_model`), the phrase clause is v3's.
            //
            // MERGE, 0.86: upstream gave both arms ONE call shape,
            // [`Ribbon::erase`], and this takes it — the retract, the owed
            // retreat and the hold are one law with two spans. What the
            // candidate keeps is WHAT IT HOLDS WITH: `erase` holds the
            // phrase, not the bare row (`hold_phrase` supersedes `hold_row`
            // — see its doc, and the v0.85.0 snap it was measured against).
            // The two bodies are otherwise the same law: upstream's
            // `cells.max(1)` reaches `retract_suffix` as `min_cells`, where
            // 0 and 1 both give `last = far.unwrap_or(col).max(col)`, so a
            // Backspace-shaped kill retracts exactly what it did here; and
            // `pending_erase` took `cells.max(1)` on both sides already.
            Event::Erase => self.erase(ctx.caret, 1, RETRACT_FADE_S, at),
            Event::Kill { cells, scope } => {
                self.erase(ctx.caret, cells.max(1), kill_span_s(cells), at);
                // A line kill ends the phrase, as a Return does: what is
                // typed next is a new line.
                if scope == super::KillScope::Line {
                    self.phrase_open = None;
                }
            }
            Event::Move {
                from, to, licence, ..
            } => {
                self.caret = Some(to);
                let same_row = to.0 == from.0;
                if licence.is_echo() {
                    // A typed echo's own FORWARD motion is inert here: the
                    // key laid its cell, and the caret mirror is all that
                    // moves. A delivered insert's is inert the same way: its
                    // sweep laid the span. What is NOT inert (2026-09-13):
                    // the hand LEAVING THE ROW, the retreat an erase's echo
                    // makes, and a composer's re-anchor — each was a Move the
                    // ribbon used to ignore, and each left light where the
                    // hand no longer was.
                    if !same_row {
                        self.leave_row(from, to, at, licence);
                    } else if to.1 < from.1 {
                        if self.pending_erase > 0 {
                            // THE ERASE'S RETREAT: the erased cells are the
                            // ones at and past the landing, whatever tick
                            // the `Erase` itself was replayed on.
                            self.retract_suffix(to.0, to.1, from.1 - to.1, at, RETRACT_FADE_S);
                            self.pending_erase = 0;
                        } else if licence == Licence::Typed {
                            self.re_anchor(from, to, at);
                        }
                    } else if to.1 > from.1 {
                        self.settle_relocate(to.0, from.1, at, ctx);
                    }
                } else {
                    // THE SCRUB (2026-09-14, the owner: "the painting
                    // system for scrubbing back and forth with back word
                    // and forward word doesn't work correctly and leaves
                    // gaps"): a keyed same-row move whose every crossed
                    // cell is lit by a band that is not leaving is the hand
                    // moving over its own text — at any distance, either
                    // way. It abandons nothing, takes nothing over, lays
                    // nothing, and HOLDS the band it crossed
                    // ([`Ribbon::scrub`]). Everything below is for a move
                    // that leaves the run.
                    let scrub = same_row
                        && licence.is_keyed_nav()
                        && self.scrub(to.0, from.1, to.1, at, ctx);
                    let jump = !same_row || to.1.abs_diff(from.1) >= JUMP_MIN_CELLS;
                    if scrub {
                        // Held; nothing else to do.
                    } else if jump {
                        // THE WAKE, then the abandon that spares it. A row
                        // change has no corridor on one row (an Up-arrow
                        // recall must not paint the line it lands on), and a
                        // PTY cascade or a Return earns no credit — they keep
                        // the old law: abandon, lay nothing.
                        let wakes = same_row && licence.is_keyed_nav();
                        let keep = if wakes {
                            self.wake(to.0, from.1, to.1, at, ctx, WAKE_LIFE_S, true)
                        } else {
                            None
                        };
                        // A PROGRAM's jump strands the band under a hand
                        // that never left it (a TUI repainting a row below
                        // and parking the cursor back): the band goes back
                        // into the HAND's last column, never toward wherever
                        // the program parked the cursor, and the hand's next
                        // key may take the band back (`join_cohort`). A
                        // SAME-ROW jump the hand made takes the landing as
                        // its frozen target with a drain that FOLLOWS THE
                        // METEOR — origin end first — so no band cell starts
                        // spending before the corridor cell above it is born
                        // (Rainbow Path v3 §2.6/§2.7).
                        let program = licence == Licence::Pty;
                        let toward = if program {
                            Some((from.1, false))
                        } else {
                            same_row.then_some((to.1, to.1 < from.1))
                        };
                        self.abandon(at, keep, toward, program);
                    } else if same_row && licence == Licence::Nav && from.1 != to.1 {
                        // AN ARROW'S HOP: a short wake behind the caret, no
                        // abandon — the band under the hand keeps its light.
                        self.wake(
                            to.0,
                            from.1,
                            to.1,
                            at,
                            ctx,
                            WAKE_LIFE_S * WAKE_HOP_LIFE_SHARE,
                            false,
                        );
                    }
                }
            }
            Event::Focus(on) => {
                if on {
                    self.regain_focus(at);
                } else {
                    self.ember_at = Some(at);
                    self.rearm = None;
                }
            }
            Event::Sweep { row, col0, col1 } => self.sweep(row, col0, col1, at, ctx),
            // THE INSERT'S REWRITE: the kill's drain from the GIVEN column
            // (the caret mirror has not moved yet on this frame's `ctx`),
            // and the ribbon's caret follows it.
            Event::Rewrite { row, col, cells } => {
                let span_s = kill_span_s(cells);
                self.retract_suffix(row, col, cells.max(1), at, span_s);
                self.caret = Some((row, col));
            }
            // A Return closes the phrase: the next typed key opens a new
            // one, and the line the hand left goes out at its own rest.
            Event::Return => self.phrase_open = None,
            Event::ReducedMotion(_) => {}
        }
    }

    /// The focused pane's `(first column, width)`; `None` is the whole grid.
    pub fn set_pane(&mut self, pane: Option<(u16, u16)>) {
        self.pane = pane;
    }

    /// **DROP THE CURTAIN** ([`CURTAIN_S`]): every cohort on glass is drawn
    /// into the hand it last stood under — its own frozen
    /// [`Cohort::retract_col`], by the retract's per-row rule against the
    /// BAND'S OWN HEAD ([`Ribbon::curtain_hand`]) — and spent to exactly
    /// zero over 0.24 s, FARTHEST-FIRST on [`CURTAIN_STAGGER_S`], with
    /// nothing laid until it is over. A ribbon with nothing laid has nothing
    /// to curtain and stays ready; a curtain already falling keeps its
    /// clock. The phrase ends with the space.
    pub fn curtain(&mut self, at: Instant) {
        self.phrase_open = None;
        if self.cells.is_empty() || self.curtain.is_some() {
            return;
        }
        // **THE HAND IS THE BAND'S OWN HEAD** (2026-09-14), not
        // [`Ribbon::caret`]. On the commonest path to this seam — type a
        // command, press Return, let `less`/`vim`/`top` take the screen —
        // the Return that launched the program has ALREADY moved the caret
        // to column 0 of the next line by the time the host notices the
        // space changed, so the caret at seam time is tens of columns from
        // the cells. Reading its bare COLUMN dragged the whole band leftward
        // while it faded — measured on glass at the alt seam, span
        // `[37,56] → [33,45] → [28,31] → [25,25]`, ending 31 columns and
        // 465 px from its own head: a slide across the row, not a gather
        // into the hand. (The resize seam looked right only because the
        // caret had not moved there.) The band's own head IS where the hand
        // let go of it — the caret sits one past the last laid glyph — so
        // the target is the head cohort's `col1`, which is that cell's right
        // edge.
        let hand = self.curtain_hand();
        for i in 0..self.cohorts.len() {
            // A cohort already retracting froze its target when the hand let
            // go of it; that is the same fact, measured earlier, and it stays.
            if self.cohorts[i].retract_col.is_some() {
                continue;
            }
            let coh = self.cohorts[i];
            let (target, right_first) = Self::target_toward(&coh, hand);
            self.cohorts[i].retract_col = target;
            self.cohorts[i].drain_right_first = right_first;
        }
        self.curtain = Some(at);
    }

    /// **WHERE THE HAND WAS** when a curtain falls — `(row, column one past
    /// the band's head)`, read from the cohorts themselves so it cannot be
    /// moved by whatever the host did to the caret at the seam.
    ///
    /// The newest cohort the hand actually typed into (`!wake`, newest
    /// [`Cohort::alive_at`]); a ribbon made only of wake corridors falls back
    /// to its newest cohort. `col1` is exclusive, so it is exactly the column
    /// the caret stood in when that cohort's last cell was laid.
    fn curtain_hand(&self) -> Option<(u16, u16)> {
        let newest = |wake_ok: bool| {
            self.cohorts
                .iter()
                .filter(|c| wake_ok || !c.wake)
                .max_by_key(|c| c.alive_at)
                .map(|c| (c.row, c.col1))
        };
        newest(false).or_else(|| newest(true))
    }

    /// Whether a curtain is falling — the host's "nothing is laid" gate
    /// and the twins' witness.
    #[must_use]
    pub fn curtained(&self) -> bool {
        self.curtain.is_some()
    }

    /// **THE PHRASE REST IN FORCE**, seconds — [`phrase_rest_ms`] of the
    /// hand's tempo; the floor before any key.
    #[must_use]
    pub fn rest_s(&self) -> f32 {
        let ioi = if self.ioi_ms > 0.0 {
            self.ioi_ms
        } else {
            IOI_DEFAULT_MS
        };
        phrase_rest_ms(ioi) / 1000.0
    }

    /// The open phrase's id — read-only, for the twins.
    #[must_use]
    pub fn phrase_open(&self) -> Option<u32> {
        self.phrase_open
    }

    /// **A TYPED KEY ON THE SCORE** (§2.1, §2.3): measure the gap since the
    /// last key, resolve a REST (a gap of the rest in force or more — the
    /// melody's own test, [`super::timing::phrase_rest_reached`], on the
    /// tempo as of the previous key), advance the tempo by the melody's own
    /// law, and open a new phrase if the last one is closed. Run once per
    /// `Typed` event, never per cell.
    fn note_key(&mut self, at: Instant) {
        let gap_ms = self
            .last_key_at
            .map(|t| (at.saturating_duration_since(t).as_secs_f32() * 1000.0).round());
        let first = gap_ms.is_none();
        let rest = gap_ms.is_none_or(|g| g >= self.rest_s() * 1000.0);
        if rest || self.phrase_open.is_none() {
            self.open_phrase();
        }
        let gap = gap_ms.map_or(0, |g| g.clamp(0.0, f32::from(u16::MAX) * 64.0) as u32);
        let ioi = if self.ioi_ms > 0.0 {
            self.ioi_ms
        } else {
            IOI_DEFAULT_MS
        };
        self.ioi_ms = ioi_next(ioi, gap, first, rest);
        self.last_key_at = Some(at);
    }

    /// Mint the next phrase and open it.
    fn open_phrase(&mut self) {
        let id = self.next_phrase.max(1);
        self.next_phrase = id.wrapping_add(1).max(1);
        self.phrase_open = Some(id);
    }

    /// The phrase a typed lay belongs to — the open one, opened here if a
    /// typed echo arrives with none open (a ledger-bridged sweep after a
    /// Return, say): a typed cell always has a phrase.
    fn phrase_for_lay(&mut self) -> u32 {
        if self.phrase_open.is_none() {
            self.open_phrase();
        }
        self.phrase_open.unwrap_or(1)
    }

    /// **THE HAND IS ON THE PHRASE UNDER IT** — an erase or a kill: every
    /// un-abandoned cohort of the open phrase on the caret's ROW keeps its
    /// laying clock, its grace is re-priced at the rest in force, and the
    /// rest is measured from this key.
    ///
    /// This REPLACES `hold_row` (2026-09-13): an erase or a kill used to
    /// refresh every un-abandoned cohort on the caret row unconditionally,
    /// which is what stopped the exit swoosh running on the last TYPED key's
    /// clock straight through a Backspace run (the surviving letters drained
    /// away ~1 s into the edit and snapped back to full, with no attack, on
    /// the fix key). The row scope is kept verbatim — `row` here IS that
    /// row, `ribbon_row_hold_model`'s `OnlyOwnerRenews` — and the phrase is
    /// added on top: an edit inside the phrase holds it.
    ///
    /// **AN ERASE AFTER THE REST OPENS A NEW ONE AND TAKES THE ROW BACK**
    /// (2026-09-14). The first spelling of this law closed the phrase instead
    /// and held nothing, on the reading that "an edit after the rest is an
    /// edit to a line the hand had let go of" — and it deleted the shipped
    /// gesture that `hold_row` was added for: on v0.85.0 a Backspace run
    /// begun after the after-glow had started SNAPPED the band back (13 lit
    /// cells to 19, at full) and the band then lived 1175.8 ms past the last
    /// erase; on the closed phrase there was no snap and the tail was 0.0 ms,
    /// because closing it made every LATER erase of the run a no-op too. A
    /// hand that comes back to a line is a hand on that line. So the erase
    /// MINTS a phrase and ADOPTS the caret row's un-abandoned cohorts into
    /// it, which renews exactly the row the gesture is on and nothing else:
    /// the phrase narrows the hold within a row, it never widens it across
    /// rows.
    fn hold_phrase(&mut self, at: Instant, row: u16) {
        let rested = self
            .last_key_at
            .is_some_and(|t| at.saturating_duration_since(t).as_secs_f32() >= self.rest_s());
        let open = match self.phrase_open {
            Some(open) if !rested => open,
            _ => {
                self.open_phrase();
                let id = self.phrase_open.unwrap_or(1);
                for coh in &mut self.cohorts {
                    if !coh.abandoned && coh.row == row {
                        coh.phrase = id;
                    }
                }
                id
            }
        };
        self.last_key_at = Some(at);
        self.hold(open, row, None, at);
    }

    /// Refresh every un-abandoned cohort of phrase `open` at `at`: the
    /// laying clock, the phase, the frozen retract target, the grace — and
    /// the life floor of its cells, so a rest that lengthened under a
    /// slowing hand never outlives the cells it holds.
    fn hold(&mut self, open: u32, hand_row: u16, laid_into: Option<usize>, at: Instant) {
        let grace = self.rest_s();
        let floor = swoosh_life_s(grace);
        let held_by = |i: usize, c: &Cohort| {
            !c.abandoned && c.phrase == open && (c.row == hand_row || laid_into == Some(i))
        };
        for (i, coh) in self.cohorts.iter_mut().enumerate() {
            if held_by(i, coh) {
                coh.alive_at = coh.alive_at.max(at);
                coh.phase = Phase::Laying;
                coh.retract_col = None;
                coh.grace_s = grace;
            }
        }
        let cohorts = &self.cohorts;
        for cell in &mut self.cells {
            if cell.life_s < floor
                && cohorts
                    .iter()
                    .enumerate()
                    .any(|(i, c)| c.id == cell.cohort && held_by(i, c))
            {
                cell.life_s = floor;
            }
        }
    }

    /// **THE RETRACT TARGET OF A COHORT** (§2.6, per-row): the caret's
    /// column when the caret is on its row; otherwise the cohort's own end
    /// nearest the caret in LOGICAL order — its right end (the fold) for a
    /// row above the caret, its left end for a row below. The suck-in never
    /// slides a row toward a column that belongs to another row (audit
    /// hi/s9 frames 0041–0044; the refresh's s17: the row-4 remnant drained
    /// toward the pre-jump caret column). Returned with the drain order the
    /// same reading fixes ([`Cohort::drain_right_first`]).
    fn retract_target(&self, coh: &Cohort) -> (Option<u16>, bool) {
        Self::target_toward(coh, self.caret)
    }

    /// [`Ribbon::retract_target`]'s rule against an ARBITRARY hand, so a
    /// curtain can freeze the same targets against the band's own head
    /// ([`Ribbon::curtain_hand`]) instead of against a caret the seam has
    /// already moved.
    fn target_toward(coh: &Cohort, hand: Option<(u16, u16)>) -> (Option<u16>, bool) {
        match hand {
            None => (None, false),
            Some((hrow, hcol)) if coh.row == hrow => (Some(hcol), false),
            Some((hrow, _)) if coh.row < hrow => {
                (Some(coh.col1.saturating_sub(1).max(coh.col0)), false)
            }
            Some(_) => (Some(coh.col0), true),
        }
    }

    /// **AN ERASE** — a Backspace or a kill of `cells` at `caret`: retract
    /// the row's suffix from the caret farthest-first over `span_s`
    /// ([`Ribbon::retract_suffix`]), owe the retreat its echo has not yet
    /// made (`pending_erase`), and hold the row's light. The Backspace's
    /// `2 × 0.24 s` and the kill's `12·n + 240 ms` are this one law with
    /// two spans.
    ///
    /// MERGE, 0.86: upstream's one fn, holding with the candidate's
    /// [`Ribbon::hold_phrase`] rather than with `hold_row`, which no longer
    /// exists — v3 superseded it (same row scope, narrowed by the open
    /// phrase, and it re-prices the grace at the rest in force, which the
    /// bare row hold could not). "Hold the row's light" above is still
    /// exactly what happens; the phrase only says WHICH cohorts of that row.
    fn erase(&mut self, caret: (u16, u16), cells: u16, span_s: f32, at: Instant) {
        self.retract_suffix(caret.0, caret.1, cells, at, span_s);
        self.pending_erase = self.pending_erase.saturating_add(cells);
        self.hold_phrase(at, caret.0);
    }

    /// Whether `cell` is still LIVE at `at`: not leaving, and inside its
    /// life measured from the later of its birth and its cohort's revival
    /// ([`live_since`]).
    fn alive_at(&self, cell: &Cell, at: Instant) -> bool {
        !cell.leaving()
            && at
                .saturating_duration_since(live_since(&self.cohorts, cell))
                .as_secs_f32()
                < cell.life_s
    }

    /// Whether a live cell owns `(row, col)` at `at` — the cell a sweep or
    /// a relay leaves exactly as it is.
    fn owned(&self, row: u16, col: u16, at: Instant) -> bool {
        self.cells
            .iter()
            .any(|c| c.row == row && c.col == col && self.alive_at(c, at))
    }

    /// **A KEY WHOSE ECHO HAS NOT LANDED IS HELD, NOT LAID.** `Event::Typed`
    /// is replayed at the engine's mirror and [`Ribbon::lay`] lays
    /// `caret.col − n`, which is the key's own cell only when its echo moved
    /// the mirror in the same frame; a frame before the echo that is the
    /// PREVIOUS cell — the prompt's trailing space for the first key of a
    /// fresh line, or the previous row's last pane cell through the fold for
    /// a key typed at column 0 (after Enter, after backspacing to col 0,
    /// `cat` / `read` / an empty PS1) — a phantom over a blank the content
    /// witness never arms, alive with the row's cohort. The engine calls
    /// this instead of [`Ribbon::on_event`] for a `Typed` with no move after
    /// it in the frame's buffer: the `Typed` arm minus the lay — the erase
    /// count is closed, a Space's cell remembered, and the caret row HELD
    /// (the edit clock an erase refreshes, so a stall still keeps the row's
    /// light). The key's cell comes with its echo, from the seam's sweep;
    /// nothing typed goes dark.
    ///
    /// `cell` is the cell the held key WILL lay — the mirror, advanced past
    /// the keys held before it in the frame — or `None` for a key typed at
    /// a caret the engine never learned. A held Space is remembered at that
    /// cell, not at the mirror's left neighbour: `lay`'s `caret − 1` is the
    /// Space's cell only once its echo has moved the mirror past it, and
    /// the composer's word move measures the word from the Space's own cell.
    /// The echo's sweep cannot correct it later ([`super::Engine`]'s
    /// `swept_space_cell` reads the frame's own keys).
    pub(super) fn hold_typed(
        &mut self,
        ev: &super::Event,
        at: Instant,
        ctx: &Ctx<'_>,
        cell: Option<(u16, u16)>,
    ) {
        if let Event::Typed { class, .. } = *ev {
            self.pending_erase = 0;
            if class == TypedClass::Space
                && let Some(cell) = cell
            {
                self.last_space = Some(cell);
            }
            // `hold_row` was v3's `hold_phrase` before the merge: the same row
            // scope, narrowed by the open phrase. A key whose echo has not landed
            // is still the hand on that row, so it holds exactly as a laid one does.
            self.hold_phrase(at, ctx.caret.0);
        }
    }

    /// **A TYPED ECHO'S GLYPH CELLS** ([`Event::Sweep`]): lay every cell in
    /// `col0..col1` on `row` that no LIVE cell owns ([`Ribbon::owned`]), as
    /// typing, born at the echo. A key lays at the caret the tick replays with, so a key whose
    /// echo shares its tick already owns its glyph cell and is left exactly
    /// as it was — per-key typing is byte-identical (the deletion goldens
    /// pin it). What the sweep lights is what the keys could not: the cells
    /// of a batched echo behind the landing's own, and the glyph of a key
    /// whose echo landed a frame late (the key was HELD, not laid, on the
    /// frame it was typed — [`Ribbon::hold_typed`] — so its cell comes
    /// from this sweep alone).
    fn sweep(&mut self, row: u16, col0: u16, col1: u16, at: Instant, ctx: &Ctx<'_>) {
        let cols = u16::try_from(ctx.geom.cols).unwrap_or(u16::MAX);
        for col in col0..col1.min(cols) {
            if !self.owned(row, col, at) {
                self.lay_cell(row, col, at, ctx, true);
            }
        }
    }

    /// Focus came back. If the ember was still burning, carry the FADE it
    /// had reached into an `edge-in` rearm so the ribbon comes back UP through
    /// the one sanctioned attack instead of snapping to full in one frame
    /// (v1's `rearm`). A focus loss that already burned out has nothing to
    /// rearm — the pool was cleared by [`Ribbon::retire`].
    ///
    /// The stamp is the ember's common FADE, not a level: [`ember_level`]
    /// turns it into each cell's own level, which is the only way every cell
    /// resumes from exactly where the ember left it.
    fn regain_focus(&mut self, at: Instant) {
        if let Some(ember) = self.ember_at.take() {
            let r = at.saturating_duration_since(ember).as_secs_f32() / FOCUS_EMBER_S;
            if r < 1.0 {
                self.rearm = Some((at, spend(clamp01(r))));
            }
        }
    }

    /// Lay `n` cells ending at the caret. The caret sits one PAST the last
    /// echoed glyph, so the cells are `[caret.col − n, caret.col)` — laid
    /// oldest-first so the head is the newest cell in the pool, which is what
    /// makes the field index's last-writer-wins rule equal to v1's newest-first
    /// scan.
    ///
    /// **THE WRAP FOLDS** (§4, kept): a cell whose column runs off the left
    /// edge lands at the right end of the row above, in a cohort of its own —
    /// a cohort never spans rows — and the new row's cohort CONTINUES the
    /// folded one's walk, so the arc crosses the fold without a seam.
    fn lay(&mut self, n: u16, at: Instant, ctx: &Ctx<'_>) {
        if n == 0 || ctx.geom.cols == 0 {
            return;
        }
        let cols = i32::try_from(ctx.geom.cols).unwrap_or(i32::MAX);
        // A fold wraps at the FOCUSED PANE's edges (`set_pane`), not the
        // grid's: a split pane's line ends at its own right margin, and the
        // cell before its first column is the previous row's LAST pane cell
        // — never the neighbouring pane's.
        let (pane_col0, pane_cols) = self
            .pane
            .map_or((0, cols), |(c0, w)| (i32::from(c0), i32::from(w).max(1)));
        let pane_col1 = pane_col0.saturating_add(pane_cols).min(cols);
        let (row, col_end) = ctx.caret;
        for back in (1..=i32::from(n)).rev() {
            let mut c = i32::from(col_end) - back;
            let mut r = i32::from(row);
            while c < pane_col0 {
                r -= 1;
                c += pane_cols;
            }
            if r < 0 || c >= pane_col1 {
                continue;
            }
            let (Ok(r), Ok(c)) = (u16::try_from(r), u16::try_from(c)) else {
                continue;
            };
            self.lay_cell(r, c, at, ctx, true);
        }
    }

    /// **THE SPINE A BIRTH IS PRICED AT** on this tick: [`Ctx::birth_disp`]
    /// (the resume-floored spine), floored again by a live mend
    /// ([`Ctx::mend`] — §23's addendum "The mend", 2026-09-08). A typed key
    /// that fixes a typo within a breath is born at `max(birth_disp,
    /// mend.disp)`, the momentum its Backspace interrupted, so the fixed
    /// cell's `cov0`, life and hot-edge price match the run it re-joins and
    /// the bed shows no dip where the fix went in. A v2-local birth price,
    /// like `METRIC_GAIN`: the spine's metric and follower are untouched, and
    /// every live-spine consumer (the wave, the wedge, the caret, the stars'
    /// envelope) keeps reading the honest [`Ctx::disp`].
    #[inline]
    fn birth_price(ctx: &Ctx<'_>) -> f32 {
        clamp01(
            ctx.mend
                .map_or(ctx.birth_disp, |m| ctx.birth_disp.max(m.disp)),
        )
    }

    /// The life one cell earns, priced at BIRTH (§4's chain law, verbatim)
    /// — and spent from the cohort's clock ([`live_since`]): it is how long
    /// the cell outlives the LAST key of its run.
    fn cell_life(&self, at: Instant, ctx: &Ctx<'_>) -> f32 {
        let full = ctx.cfg.duration.as_secs_f32().max(0.001);
        let base = (full * LIFE_DURATION_GAIN).clamp(LIFE_BASE_MIN, LIFE_BASE_MAX);
        let d = Self::birth_price(ctx);
        let life = base * (1.0 + LIFE_SPINE_LINEAR * d + LIFE_SPINE_SQUARE * d * d);
        // THE FOUR-LETTER GUARANTEE. The observed inter-key gap is the newest
        // cohort's own idle time; the FIRST key of a burst has no gap and so
        // takes no floor, which is what lets a lone keystroke fade crisply.
        let gap = self
            .cohorts
            .iter()
            .map(|c| at.saturating_duration_since(c.alive_at).as_secs_f32())
            .fold(f32::INFINITY, f32::min);
        let life = if gap <= CHAIN_GAP_MAX {
            life.max((CHAIN_KEYS * gap + CHAIN_MARGIN).min(CHAIN_LIFE_MAX))
        } else {
            life
        };
        // The exit swoosh must get to finish before the natural melt steals the
        // ending — even for a lone keystroke, which earns a full mini-swoosh.
        life.max(swoosh_life_s(self.rest_s()))
    }

    /// **THE HAND LEFT THE ROW** (2026-09-13, the owner: "the rainbow cursor
    /// streak doesn't follow the new line down in claude code and codex.
    /// it's persistent on the screen"): a typed or delivered move onto
    /// another row. The origin row's cohorts are abandoned INTO THE POINT
    /// THE HAND LEFT AT (`from.1`, T5: toward the hand that let go — for a
    /// fold that is the row's right end; for a composer's re-wrap the
    /// column the moved word was typed up to) and may be taken back by a
    /// key that returns there (a Backspace across the wrap); the mark on
    /// the new row starts its own clock. Until this ruling the earlier rows
    /// of a wrapped paragraph were HELD by every key on the later one
    /// (`Cohort::alive_at`'s one-finger law), which is exactly the persistent
    /// old row the owner saw in Claude Code and Codex — and on a shell fold
    /// too, where it merely read as "still lit".
    ///
    /// A TYPED landing past the pane's first two columns is a composer's
    /// re-anchor: the app moved the row's last word down with the caret, so
    /// the word's cells are relaid where the caret reveals them
    /// ([`Relocate`]); a landing at the pane's edge is a plain fold, whose
    /// cells the host's fold sweep already lays.
    fn leave_row(&mut self, from: (u16, u16), to: (u16, u16), at: Instant, licence: Licence) {
        self.abandon_row(from.0, at, Some(from.1));
        self.pending_erase = 0;
        let w = self.moved_word_len(from);
        self.last_space = None;
        self.relocate = None;
        let pane_col0 = self.pane.map_or(0, |(c0, _)| c0);
        if licence == Licence::Typed && to.1 > pane_col0.saturating_add(1) {
            self.begin_relocate(to, w, at);
        }
    }

    /// **A COMPOSER'S SAME-ROW RE-ANCHOR** (2026-09-13): a typed move that
    /// lands LEFT of its origin on the same row with no erase behind it —
    /// Claude Code's bottom-pinned box growing UP (the old line moved a row
    /// up without the ribbon being told, the caret's row constant), Codex's
    /// rewrite. The old line's cells at and past the landing drain into the
    /// caret farthest-first inside the retract's own [`RETRACT_DUR_S`] — the
    /// whole stale line is gone in 0.64 s, however long it was — and the
    /// moved word is relaid where the caret reveals it.
    fn re_anchor(&mut self, from: (u16, u16), to: (u16, u16), at: Instant) {
        let n = from.1 - to.1;
        // The licensed backward re-anchor drains the old line toward its
        // landing — main's integration ruling (2026-09-13,
        // `a_re_anchor_onto_the_pane_s_first_column_lights_nothing_in_the_pane_beside_it`):
        // the row's stale cells past the landing leave on the retract's
        // clock whatever the app rewrites there; the hand's next keys relay
        // their own.
        self.retract_suffix(to.0, to.1, n, at, RETRACT_DUR_S);
        let w = self.moved_word_len(from);
        self.last_space = None;
        self.begin_relocate(to, w, at);
    }

    /// Correct a Space's cell from an exact ordered key batch and its licensed
    /// same-row sweep. The engine alone has both witnesses; a final frame
    /// caret cannot locate an earlier Space in a coalesced echo. Metadata
    /// only: this neither lays cells nor changes their births or clocks.
    pub(super) fn observe_space_cell(&mut self, row: u16, col: u16) {
        self.last_space = Some((row, col));
    }

    /// Cells of the word the app moved off `from`'s row: everything typed
    /// after the row's last Space, up to the origin column. `0` with no
    /// Space on the row (a line that is one word wraps mid-word: the app
    /// moved nothing).
    fn moved_word_len(&self, from: (u16, u16)) -> u16 {
        match self.last_space {
            Some((row, s)) if row == from.0 && s < from.1 => from.1 - s - 1,
            _ => 0,
        }
    }

    /// Relay a re-anchor's moved word (`w` cells) before the wrap key's own
    /// glyph at `landing − 1` — now, when the landing reveals it, else
    /// pending for the same-row forward move that will. The cells laid on
    /// the row THIS tick left of the landing (the wrap key's `Typed`, replayed
    /// against a caret parked at the box's inset — the prompt marker's cell)
    /// are dropped: nothing was ever typed there.
    fn begin_relocate(&mut self, to: (u16, u16), w: u16, at: Instant) {
        let pane_col0 = self.pane.map_or(0, |(c0, _)| c0);
        let (row, landing) = to;
        let Some(glyph) = landing.checked_sub(1) else {
            return;
        };
        if w == 0 {
            return;
        }
        if glyph >= w && glyph - w >= pane_col0 {
            self.relay_word(row, glyph - w, glyph, at);
            return;
        }
        // The landing cannot hold the word: the caret is parked at the
        // inset and the real landing comes with a later move.
        self.cells
            .retain(|c| !(c.row == row && c.col < landing && c.born >= at && c.typing));
        self.relocate = Some(Relocate { row, w, at });
    }

    /// The pending relocation's landing arrived: a same-row FORWARD typed
    /// move on its row whose origin `from_col` is the moved word's end plus
    /// the wrap key's glyph. Lays `[from_col − 1 − w, from_col)` — the word
    /// and that glyph, which the refused half of the rewrite left dark.
    fn settle_relocate(&mut self, row: u16, from_col: u16, at: Instant, ctx: &Ctx<'_>) {
        let Some(rel) = self.relocate else {
            return;
        };
        if rel.row != row {
            return;
        }
        if at.saturating_duration_since(rel.at).as_secs_f32() > RELOCATE_PATIENCE_S {
            self.relocate = None;
            return;
        }
        let pane_col0 = self.pane.map_or(0, |(c0, _)| c0);
        let Some(glyph) = from_col.checked_sub(1) else {
            return;
        };
        if glyph < rel.w || glyph - rel.w < pane_col0 {
            return;
        }
        let _ = ctx;
        self.relay_word(row, glyph - rel.w, from_col, at);
        self.relocate = None;
    }

    /// Lay `col0..col1` on `row` as typing born at `at`, only where no live
    /// cell already is — the relocation's own sweep. The cells join the
    /// row's cohort on the walk it has (C2).
    fn relay_word(&mut self, row: u16, col0: u16, col1: u16, at: Instant) {
        for col in col0..col1 {
            if self.owned(row, col, at) {
                continue;
            }
            let idx = self.join_cohort(row, col, at);
            let cohort = self.cohorts[idx];
            // Priced like its neighbour: the word is the hand's own glyphs
            // the app relocated, so it takes the light the row has.
            let like = self
                .cells
                .iter()
                .filter(|c| c.row == row && c.cohort == cohort.id && c.typing)
                .max_by(|a, b| a.born.cmp(&b.born))
                .copied();
            let cell = Cell {
                row,
                col,
                cohort: cohort.id,
                t: cohort.t_at(col),
                born: at,
                attack_at: at,
                life_s: like.map_or(SWOOSH_LIFE_S, |c| c.life_s),
                cov0: like.map_or(BODY_COLD_SHARE, |c| c.cov0),
                typing: true,
                retract_at: None,
                retire_at: None,
                birth_disp: like.map_or(0.0, |c| c.birth_disp),
                edge_cells: like.map_or(HOT_EDGE_CELLS, |c| c.edge_cells),
                layer: Layer::Base,
                rearm: None,
            };
            self.place(idx, cell, at, row);
        }
    }

    /// **THE SCRUB, AND THE LINE BETWEEN IT AND THE JUMP** (2026-09-14;
    /// the Rainbow Path v3 merge, 2026-09-14) — a same-row keyed move
    /// (`Nav`, `Synthetic`) from column `from` to `to` on `row` is a scrub
    /// when the corridor it crossed, `[min, max)`, is lit end to end by
    /// cells that are not leaving: every column owned by a live cell with
    /// light on the glass whose cohort is neither abandoned nor inside its
    /// retract. That is the hand moving over its own text. Then nothing is
    /// laid, taken over or abandoned, and the cohorts the corridor crosses
    /// are HELD — clock renewed to the hop, phase back to laying, the
    /// retract target released — exactly as [`Ribbon::hold_phrase`] holds
    /// the caret row's phrase for an editing hand: the band leaves through
    /// its swoosh ([`Cohort::swoosh_total_s`]) after the hand's LAST act on
    /// it, a hop included. Returns whether the move was a scrub; a move that
    /// is not one takes the jump or hop arm.
    ///
    /// **THIS PREDICATE IS THE BOUNDARY** between the two laws that
    /// answer a same-row keyed move, and it is the whole of it. Rainbow
    /// Path v3 §2.7 rules that a jump lays an OVERLAY corridor
    /// ([`Layer::Over`]) along the meteor's flight and abandons the
    /// [`Layer::Base`] band beneath it; the scrub rules that the same event
    /// lays nothing and holds the band. Both are right, about different
    /// events, and `scrub` decides which event this is:
    ///
    /// * **INSIDE the hand's own lit run → a scrub.** Every column the move
    ///   crossed is owned by live, not-leaving, not-melting light of a
    ///   cohort that is not abandoned and not retracting. Alt-B/Alt-F over
    ///   the words of the line you just typed; Ctrl-A and Ctrl-E between the
    ///   ends of a band that reaches both. Held: see the measurement below,
    ///   and `a_scrub_inside_the_band_keeps_every_cell_s_light_and_holds_the_row`,
    ///   `a_home_and_an_end_inside_the_band_abandon_nothing_and_take_nothing_over`.
    /// * **LEAVING the run → a jump, and v3's overlay wake.** Past the
    ///   band's OLDEST cell (a Ctrl-A to column 0 from a band that starts at
    ///   column 10 — the real prompt's case, the prompt's own columns are
    ///   dark), off its END onto dark ground (an Alt-F or an End past the
    ///   last letter), over a cell already melting or already leaving, or to
    ///   ANOTHER ROW (`same_row` is false before this is ever called). Then
    ///   [`Ribbon::wake`] lays its corridor on `Over` born on the flight
    ///   clock and [`Ribbon::abandon`] sends the `Base` band into its
    ///   retract beneath it — v3 §2.7 verbatim, K1–K4, and
    ///   `a_jump_lays_its_wake_over_the_live_band_and_leaves_the_band_s_cells_untouched`,
    ///   `a_jump_past_the_band_s_oldest_cell_or_off_its_row_still_abandons_it`.
    ///
    /// The two laws never disagree about one event because the corridor
    /// cannot be both lit end to end and not: a move INSIDE the run has no
    /// dark ground for a corridor to light and no band the hand has left to
    /// abandon, and a move that LEAVES the run has both. The v3 design's own
    /// prose says the same thing from the other side — the overlay exists so
    /// that "a leaving band is never taken over" (R9) can be literal — and a
    /// band under a hand that never left it is not leaving.
    ///
    /// The owner, on v0.85.0: *"the painting system for scrubbing back and
    /// forth with back word and forward word doesn't work correctly and
    /// leaves gaps."* Measured (`docs/measured/scrub-gaps-2026-09-14.md`):
    /// under the 2026-09-09 laws a word hop left the band's clock alone
    /// ("a hop must not touch the word's clock", 4ca68c467), so the swoosh
    /// ran on the last KEY's clock under the scrubbing hand — by the
    /// seventh hop of the owner's scrub the band was mid-retract, its tail
    /// drained and its boundaries pulled toward the column the retract
    /// began under, while the hops through it laid cold half-life stubs
    /// only where the drain had already emptied a cell; and a word hop of
    /// [`JUMP_MIN_CELLS`] or more ABANDONED the band ("a real jump on the
    /// same row abandons the band into its swoosh", d23b4f935 → 4ca68c467),
    /// handing the corridor to a 1.1 s wake and draining the rest, which the
    /// next hop back found mid-retract and, by the leaving-band clause,
    /// could not take over. Both are one defect: a band under a hand that
    /// never left it was treated as a band the hand had left. That law is
    /// discarded HERE, for the move that stays inside the run; a jump that
    /// leaves the run — past its oldest cell, off its end onto dark ground,
    /// to another row — keeps it ([`Ribbon::abandon`], the hop's cold wake).
    ///
    /// RENEW, not freeze: the band's clock is the hop's, so a scrub that
    /// lasts longer than a swoosh keeps the band whole for as long as it
    /// lasts, and the cells' life ([`Cell::life_s`], spent from the cohort's
    /// clock through [`live_since`]) is renewed with it — the same choice
    /// [`Ribbon::hold_phrase`] made for the erase. A band already inside its
    /// retract when the hop comes is a band already leaving: the 4ca68c467
    /// clause stands, and the move is judged as the hop or jump it is.
    ///
    /// The hold is the clock only — `alive_at`, `phase`, `retract_col`. It
    /// does NOT re-price `Cohort::grace_s` and does NOT raise
    /// [`Cell::life_s`] the way [`Ribbon::hold`] does for a typed key: a
    /// navigation mints no note, so the rest in force has not moved, and a
    /// wake cohort caught in a corridor keeps the floor grace
    /// [`PHRASE_REST_MIN_S`] it was minted with ([`Ribbon::mint_cohort`]).
    /// The hand does not adopt what it crossed into the open phrase either —
    /// it does not have to: a key that lands in or beside the band takes it
    /// back through [`Ribbon::join_cohort`] and [`Ribbon::place`] re-assigns
    /// it there, and an erase takes the whole row back through
    /// [`Ribbon::hold_phrase`].
    ///
    /// Two guards, from the fix-up review (2026-09-14), each the anti-stray
    /// law's own — "no light with no keystroke behind it", "never brighter
    /// than the cell was":
    ///
    /// * **A cell past its melt opening is a cell already going out.** The
    ///   expiry melt opens at `1 − EXPIRY_MELT_SHARE` of a cell's life
    ///   (the reduced-motion fade [`REDUCED_MOTION_FADE_MS`] before its
    ///   end), and a hop's cold stub ([`WAKE_HOP_LIFE_SHARE`]) or a jump's
    ///   wake reaches it while its cohort is still in grace. Renewing that
    ///   cohort's clock restarted `spent` at zero and snapped a stub at
    ///   0.09 of body to 1.0 in one frame — a ×7.5 step with no attack. So
    ///   the corridor is lit end to end only by cells not yet melting; over
    ///   a melting stub the move is the hop it is, lays nothing over the
    ///   live stub, and the stub finishes melting under the hand. The
    ///   typed band is never caught by this guard: its life is at least
    ///   [`swoosh_life_s`] of its phrase's grace, so its melt opens past
    ///   the retract the phase guard already refuses.
    /// * **The hold reaches exactly the cohorts that OWN the corridor's
    ///   columns** — the newest un-leaving cell of each, the one the guard
    ///   above read. A cohort shadowed under them is left on its clock: an
    ///   abandoned band's drained cells stay resident (not `leaving()`)
    ///   under the fresh cells a jump's wake laid in its holes until its
    ///   swoosh ends, and a hold that renewed "every cohort with a cell in
    ///   the corridor" brought that band back from `0 … 0, 111` to full
    ///   in one frame, Fading → Laying, 0.64 s after it was abandoned. As
    ///   [`Ribbon::hold_phrase`] and the one-finger hold in
    ///   [`Ribbon::place`] skip `abandoned`, a scrub skips what it did not
    ///   read. Under v3's layers the reading is narrower still: the owner
    ///   the guard reads is the corridor column's newest un-leaving cell on
    ///   EITHER layer, so a live `Over` corridor from an earlier jump counts
    ///   as light on the glass — which is what it is.
    fn scrub(&mut self, row: u16, from: u16, to: u16, at: Instant, ctx: &Ctx<'_>) -> bool {
        let (lo, hi) = (from.min(to), from.max(to));
        if lo == hi {
            return false;
        }
        // The owner of a column: its newest cell that is not leaving (the
        // pool is append-ordered; `build` and `wake` read the same one).
        let owner = |cells: &[Cell], col: u16| {
            cells
                .iter()
                .rposition(|c| c.row == row && c.col == col && !c.leaving())
        };
        let reduced = ctx.cfg.reduced_motion;
        for col in lo..hi {
            let Some(i) = owner(&self.cells, col) else {
                return false;
            };
            let cell = self.cells[i];
            if self.env_of(ctx, &cell) <= 0.0 {
                return false;
            }
            let Some(coh) = self.cohorts.iter().find(|k| k.id == cell.cohort) else {
                return false;
            };
            let idle = at.saturating_duration_since(coh.alive_at).as_secs_f32();
            if coh.abandoned || Phase::at(idle, coh.grace_s).is_none_or(Phase::is_retracting) {
                return false;
            }
            // THE MELT GUARD: `spent` on the clock `env_of` reads
            // (`live_since`), against the instant the cell's own melt opens.
            let spent = at
                .saturating_duration_since(cell.born.max(coh.alive_at))
                .as_secs_f32();
            let melt_opens = if reduced {
                (cell.life_s - REDUCED_MOTION_FADE_MS / 1000.0).max(0.0)
            } else {
                cell.life_s * (1.0 - EXPIRY_MELT_SHARE)
            };
            if spent > melt_opens {
                return false;
            }
        }
        // THE HOLD: the owner of each corridor column, and only it. Holding
        // a cohort twice is the same hold.
        for col in lo..hi {
            let Some(i) = owner(&self.cells, col) else {
                continue;
            };
            let id = self.cells[i].cohort;
            if let Some(coh) = self.cohorts.iter_mut().find(|k| k.id == id) {
                coh.alive_at = coh.alive_at.max(at);
                coh.phase = Phase::Laying;
                coh.retract_col = None;
            }
        }
        true
    }

    /// Abandon the cohorts of ONE row — see [`Ribbon::abandon`] — into the
    /// column the hand left the row at, taking them back if it returns.
    fn abandon_row(&mut self, row: u16, at: Instant, toward: Option<u16>) {
        for coh in &mut self.cohorts {
            if coh.row != row {
                continue;
            }
            coh.release_clock = None;
            if coh.abandoned {
                continue;
            }
            // ITS OWN retract start (Rainbow Path v3 §2.6): a cohort whose
            // phrase rested longer is sent the same `0.40 + 0.24 s` out.
            let Some(rewound) =
                at.checked_sub(std::time::Duration::from_secs_f32(coh.retract_start()))
            else {
                continue;
            };
            if coh.retract_col.is_none() {
                coh.retract_col = toward;
            }
            coh.abandoned = true;
            coh.rejoinable = true;
            if coh.alive_at > rewound {
                coh.alive_at = rewound;
            }
        }
    }

    /// Lay ONE cell, joining or minting its cohort.
    fn lay_cell(&mut self, row: u16, col: u16, at: Instant, ctx: &Ctx<'_>, typing: bool) {
        let life_s = self.cell_life(at, ctx);
        let birth_disp = Self::birth_price(ctx);
        let cov0 = BODY_COLD_SHARE + (1.0 - BODY_COLD_SHARE) * birth_disp;
        let idx = self.join_cohort(row, col, at);
        let cohort = self.cohorts[idx];
        // THE WALK IS A FUNCTION OF POSITION (C2, §4): the cell takes the stop
        // its column has in its cohort, from the cohort's immutable origin.
        // Chaining from "the highest t in the pool" instead would hand a cell
        // retyped after a backspace the stop of the erased cell to its right
        // plus one, and leave a colour seam when the erased cells fade.
        // A TYPED CELL LAID WHERE THE ATTACH STOOD inherits its light
        // (2026-09-14): the caret cell already carried the head's light at
        // full under the block, so the cell the key lays there starts with
        // its attack spent — edge-in and wipe alike — instead of dropping to
        // the birth floor and pouring back in; the pour is the NEXT attach's
        // (`plan_run`). A cell laid anywhere else attacks as before.
        let inherited = typing && self.attach == Some((row, col));
        let attack_at = if inherited {
            at.checked_sub(std::time::Duration::from_secs_f32(
                EDGE_IN_S + ATTACK_WIPE_S,
            ))
            .unwrap_or(at)
        } else {
            at
        };
        let cell = Cell {
            row,
            col,
            cohort: cohort.id,
            t: cohort.t_at(col),
            born: at,
            attack_at,
            life_s,
            cov0,
            typing,
            retract_at: None,
            retire_at: None,
            birth_disp,
            edge_cells: edge_cells(ctx.surge, ctx.flow.heat),
            layer: Layer::Base,
            rearm: None,
        };
        self.place(idx, cell, at, ctx.caret.0);
    }

    /// Put ONE built cell into the pool under the cohort at `idx`, laid at
    /// `at` while the hand is on `hand_row`: one owner per `(row, col,
    /// cohort)`, the cohort's bounds grown over it, and the cohort clocks
    /// that the lay refreshes.
    fn place(&mut self, idx: usize, mut cell: Cell, at: Instant, hand_row: u16) {
        let (row, col, typing) = (cell.row, cell.col, cell.typing);
        // ONE OWNER PER CELL. A retype replaces the light on that cell rather
        // than stacking a second body under it — the shadowed cell could never
        // be seen, and leaving it in the pool is how v1's field scan grew.
        if let Some(slot) = self
            .cells
            .iter()
            .position(|c| c.row == row && c.col == col && c.cohort == cell.cohort)
        {
            let old = self.cells.remove(slot);
            // RE-WETTING IS MONOTONE (2026-09-13): a LIVE cell laid again —
            // a key replayed one tick before its echo re-lays the PREVIOUS
            // glyph's cell — keeps the attack it already had and the higher
            // of the two prices. Restarting `born` re-ran the 18 ms edge-in
            // on a settled cell: a one-frame ~35–65 % dip on the cell beside
            // the hand for about two keys in five. A cell that was leaving
            // (retracting) or is out of time (a stalled key's replay, dated
            // at a press seconds ago, that the floored sweep then re-lays)
            // takes a fresh attack. Identity is ALWAYS the fresh birth: the
            // content witness must not read the new key as the old glyph.
            let alive = self.alive_at(&old, at);
            if alive && old.typing == typing {
                cell.attack_at = old.attack_at.min(cell.attack_at);
                cell.cov0 = cell.cov0.max(old.cov0);
                cell.life_s = cell.life_s.max(old.life_s);
            }
        }
        self.cells.push(cell);
        let coh = &mut self.cohorts[idx];
        coh.col0 = coh.col0.min(col);
        coh.col1 = coh.col1.max(col + 1);
        if typing {
            // The hand is on this cohort now, whatever minted it.
            coh.wake = false;
            // ONE FINGER, ONE PHRASE, ON THE HAND'S ROW (merged 2026-09-14
            // from 2026-09-12's row ruling and Rainbow Path v3 §2.3/S6).
            //
            // The shipped law is ONE FINGER, ONE ROW: a live key holds every
            // un-abandoned cohort ON THE HAND'S ROW in its laying phase. It
            // used to hold every row — "a hot paragraph typed across three
            // wraps is one mark" — which is what kept the row a composer had
            // re-wrapped away from lit for as long as the hand typed below
            // it (the owner's "persistent on the screen"). The streak
            // follows the hand: a row it has left leaves on its own clock
            // (`leave_row`). That ruling is model-checked — `aterm-spec`'s
            // `ribbon_row_hold_model`, invariant `OnlyOwnerRenews`, with a
            // global hold as its named falsifier — so v3's phrase does not
            // widen it back across rows; the phrase NARROWS it instead.
            //
            // v3's clause: the cohort a typed key lays into belongs to the
            // OPEN PHRASE — minted into it, or re-assigned to it when the
            // hand resumes a line whose phrase had closed — and the hold
            // renews only cohorts of THAT phrase. A cohort of a CLOSED
            // phrase on the same row (the hand paused past the rest and
            // started again beside it) is left on its own clock, where the
            // row law renewed it. The cohort the cell was laid INTO is
            // always refreshed — a fold's own sweep, dated at the key, lands
            // on the row the hand is leaving and is that row's last key.
            let open = self.phrase_for_lay();
            self.cohorts[idx].phrase = open;
            self.hold(open, hand_row, Some(idx), at);
        } else {
            // A WAKE CELL holds only its OWN cohort — and that is always a
            // wake cohort ([`Cohort::wake`]: a jump mints one, a hop joins
            // or mints one, never a typed word's): an arrow held down keeps
            // the wake it is laying out of the swoosh until the arrow stops,
            // and nothing else — a jump builds no momentum and lifts no other
            // mark's finger (`typing` is what the one-finger law reads).
            coh.alive_at = coh.alive_at.max(at);
            coh.phase = Phase::Laying;
            coh.retract_col = None;
        }
    }

    /// **THE WAKE** (R5) — lay the corridor a same-row move just crossed on
    /// `row`, from column `from` to the landing `to`, newest at the landing.
    /// The landing cell is included, so the caret block's fill, the train's
    /// phase-lock (`field_at(landing)`, §6.4) and the bed under the caret read
    /// ONE walk; the origin cell is the far end. New cells are born
    /// along the flight ([`flight_ms`] of the span: cell `j` from the origin
    /// at `T · j / n`) and live `life_s`; at most [`WAKE_MAX_CELLS`] of them,
    /// counted back from the landing.
    ///
    /// The wake's cohort takes THE BAND'S OWN WALK ORIGIN
    /// ([`Ribbon::wake_origin`]: the row's freshest cohort's `anchor_col`
    /// and `t0`, not its `t` read at the landing) — `walk_t` is `d/16` for
    /// sixteen cells and `1/36` a cell after, so only a cohort anchored where
    /// the band is anchored gives `t_at(col)` the band's number at EVERY
    /// column (C2); a wake anchored at the landing agreed with the band on a
    /// Ctrl-A to its origin and repainted stops past the kink on a Ctrl-E
    /// across it.
    ///
    /// **THE WAKE IS AN OVERLAY** (2026-09-13, Rainbow Path v3 §2.3/§2.7,
    /// laws K1–K4). Every wake cell is [`Layer::Over`] and the band's own
    /// cells — [`Layer::Base`] — are never touched: not taken over, not
    /// re-priced, not moved. `takeover` is the JUMP's arm: the corridor is
    /// minted as a cohort of its own and laid over every column it crosses
    /// (within [`WAKE_MAX_CELLS`] of the landing, or beyond it wherever the
    /// band still carries light — the cap bounds NEW light, never the
    /// corridor over light already on the glass). Over a live band cell the
    /// wake cell is born on the flight clock AT THE BAND'S OWN LEVEL
    /// ([`Cell::rearm`]) and at least its price, so the hand-off has no
    /// attack and no dip; the abandon that follows drains the band beneath
    /// on a schedule that follows the meteor ([`Ribbon::abandon`]), so no
    /// band cell starts spending before the corridor above it is born. A
    /// band ALREADY LEAVING is covered the same way, from whatever level it
    /// has: the old "lay nothing under a leaving cell" clause — R6's guard
    /// against a half-drained cell FROZEN at its level by a takeover — is
    /// unnecessary once nothing is taken over and a covered level is only
    /// ever a starting point for `edge-in` (M1), and it was the tear the
    /// refresh measured: the cells a reversal could not re-strike left
    /// before the rest. A cell an EARLIER WAKE laid is RE-OWNED by this one
    /// in place (K4): same birth, same price, re-armed from its current
    /// level, on this cohort's clock — so a Ctrl-A/Ctrl-E ping-pong at any
    /// cadence holds one corridor, one interval of light, one wake cohort
    /// on the row.
    ///
    /// Without `takeover` (a hop) a column any LIVE light owns is left
    /// exactly as it is — the band's cell on `Base`, and (merged 2026-09-14
    /// from the scrub round) an earlier hop's own cold stub on `Over`: a hop
    /// re-owns nothing, because a cell already inside its expiry melt that a
    /// hop renewed came back to full in one frame, which is the same
    /// counter-example [`Ribbon::scrub`]'s melt guard refuses. Re-owning is
    /// the jump's law (K4 names a jump), and a jump needs it because its
    /// corridor must be one interval. The hop's new cells join the row's
    /// adjacent WAKE cohort or mint one on the same origin
    /// ([`Ribbon::join_wake_cohort`]) — never a typed word's, whose clock and
    /// bounds a hop must not touch.
    ///
    /// Every wake cell is `typing: false` — load-bearing: the one-finger hold
    /// in [`Ribbon::place`] and the momentum spine read that flag, and
    /// `mod.rs`'s "a jump builds no momentum" depends on it (pinned by
    /// `a_wake_is_never_typing_and_holds_no_other_cohort`).
    ///
    /// Returns the minted cohort's id (the takeover arm) so the abandon can
    /// spare it.
    #[allow(clippy::too_many_arguments)]
    fn wake(
        &mut self,
        row: u16,
        from: u16,
        to: u16,
        at: Instant,
        ctx: &Ctx<'_>,
        life_s: f32,
        takeover: bool,
    ) -> Option<u32> {
        let cols = u16::try_from(ctx.geom.cols).unwrap_or(u16::MAX);
        if cols == 0 || from == to || to >= cols {
            return None;
        }
        let leftward = to < from;
        let far = from.min(cols - 1);
        let span = far.abs_diff(to);
        // THE FLIGHT CLOCK (K3): a jump's `T = flight_ms(cells)`, the one
        // number the meteor head, the bell and the pin already fly on; a
        // hop takes the flight floor. Cell `j` from the origin is born at
        // `t0 + T · j / n`.
        let flight_s = if takeover {
            flight_ms(f32::from(span))
        } else {
            FLIGHT_MIN_MS
        } / 1000.0;
        let n = f32::from(span).max(1.0);
        let birth_disp = clamp01(ctx.birth_disp);
        let cov0_new = BODY_COLD_SHARE + (1.0 - BODY_COLD_SHARE) * birth_disp;
        let (anchor_col, t0) = self.wake_origin(row, to, at);
        let minted = takeover.then(|| self.own_wake_cohort(row, to, from, anchor_col, t0, at));
        // Far end first, landing LAST — the pool is append-ordered and the
        // index's last-writer-wins rule reads it so. Birth order does NOT
        // make the landing the head. RESTATED AT THE 0.86 RC MERGE: main's
        // reason here reads "every new cell here shares one `born_new`",
        // and on this tree they do not — v3's flight clock gives cell `j`
        // its own `t0 + T · j / n` just below, so birth order IS the
        // corridor's own order. The conclusion is unchanged and now holds
        // for two reasons at once: a newest-born head would resolve to the
        // far end of a leftward corridor, to the landing of a rightward
        // one, and to the far end again of a re-owned one — none of which
        // is a rule. The landing is the head by `head_col`'s WAKE clause
        // (the caret standing on a wake run's first cell), which is what
        // puts the hot edge by the caret after a Ctrl-A.
        // (`Cohort::landing_col` and the `landing_idx` that stamped it are
        // gone with main's second review round, a5d400c38: the boundary is
        // one expression on every run again.)
        for k in (0..=span).rev() {
            let col = if leftward { to + k } else { to - k };
            let j = span - k;
            let born_new = at
                .checked_add(std::time::Duration::from_secs_f32(
                    flight_s * f32::from(j) / n,
                ))
                .unwrap_or(at);
            // THE OVER LAYER'S OWNER at this column — an older wake's cell,
            // whatever its clock — and the BASE cell beneath, if it is live
            // light: the pool is append-ordered; `build` reads the same
            // ones.
            let over = self
                .cells
                .iter()
                .rposition(|c| c.row == row && c.col == col && c.layer == Layer::Over);
            let base = self
                .cells
                .iter()
                .rposition(|c| c.row == row && c.col == col && c.layer == Layer::Base)
                .map(|i| {
                    let c = self.cells[i];
                    (
                        c.cov0,
                        if c.leaving() {
                            0.0
                        } else {
                            self.env_of(ctx, &c)
                        },
                    )
                });
            let base_cov = base.map_or(0.0, |(cov0, env)| cov0 * env);
            // The `Over` owner while it is still LIGHT ON THE GLASS — read
            // for the hop's guard alone. A jump re-owns whatever it finds
            // there, live or burnt out, because its corridor must be one
            // interval and a re-owned cell's clock restarts with its new
            // cohort's (`live_since`).
            let over_live = over.filter(|&i| {
                let c = self.cells[i];
                !c.leaving() && self.env_of(ctx, &c) > 0.0
            });
            // **A HOP LEAVES LIVE LIGHT EXACTLY AS IT IS** — the scrub
            // round's law (2026-09-14), read here over BOTH layers at the
            // merge. An arrow beside the word you just typed lays nothing
            // over the band (`Base`), and an arrow that turns back over its
            // own cold stub lets the stub finish melting instead of re-owning
            // it (`Over`): renewing a cell already inside its expiry melt is
            // "light no key bought" — a stub at 0.09 of body came back to
            // full, ×7.5 in one frame, which is the counter-example
            // `Ribbon::scrub`'s melt guard was added for. The two are ONE law
            // read at two seams: the scrub refuses to hold a melting cell, and
            // the hop that the refusal hands the move to refuses to re-own it.
            // Re-owning is the JUMP's law (v3 §2.7 K4, which says "a jump
            // whose corridor overlaps an `Over` cohort"): a corridor must be
            // one interval, so it re-owns what it covers.
            if !takeover && (base_cov > 0.0 || over_live.is_some()) {
                continue;
            }
            let (born, attack_at, cov0, rearm) = match over {
                Some(i) => {
                    // THE REVERSAL RE-OWNS (K4, M1): the overlapped cell of
                    // an earlier wake — a Ctrl-A/Ctrl-E ping-pong's last
                    // corridor, a held arrow's own trail — is taken into
                    // this wake in place: same birth, same price, its
                    // envelope re-armed from the level it has NOW so it
                    // rises through `edge-in` and never steps, on this
                    // cohort's clock from here. The old cohort keeps only
                    // the cells this corridor does not cover, on its own
                    // clock; emptied, it retires. Before this the old
                    // wake's cells were "leaving" and nothing was laid under
                    // them — the drain tear the refresh measured 852–889 ms
                    // after the last reversal (cols 37–41 dark for ~40 ms).
                    let old = self.cells[i];
                    let level = clamp01(self.env_of(ctx, &old));
                    self.cells.remove(i);
                    (old.born, old.attack_at, old.cov0, Some((at, level)))
                }
                None => {
                    if base_cov <= 0.0 && k >= WAKE_MAX_CELLS {
                        // Beyond the cap NEW light is not laid; light the band
                        // already carries there is covered, so the corridor
                        // stays whole however long the band was.
                        continue;
                    }
                    if base_cov > 0.0 {
                        // OVER A LIVE BAND CELL: born on the flight clock at
                        // the band's OWN level — its coverage share now — so
                        // the hand-off has no attack and no dip (M1), and at
                        // least the band's price, so the corridor is never
                        // dimmer than the band it covers.
                        let cov0 = cov0_new.max(base_cov);
                        (
                            born_new,
                            born_new,
                            cov0,
                            Some((born_new, clamp01(base_cov / cov0))),
                        )
                    } else {
                        (born_new, born_new, cov0_new, None)
                    }
                }
            };
            let idx = match minted {
                Some(idx) => idx,
                None => self.join_wake_cohort(row, col, anchor_col, t0, at),
            };
            let cohort = self.cohorts[idx];
            let cell = Cell {
                row,
                col,
                cohort: cohort.id,
                t: cohort.t_at(col),
                born,
                attack_at,
                life_s,
                cov0,
                typing: false,
                retract_at: None,
                retire_at: None,
                birth_disp,
                // A wake cell is the swoosh's reach, not a key: no key, no
                // surge, and the crisp edge is the head's property anyway.
                edge_cells: HOT_EDGE_CELLS,
                layer: Layer::Over,
                rearm,
            };
            self.place(idx, cell, at, row);
        }
        minted.map(|idx| self.cohorts[idx].id)
    }

    /// **ONE LIVE WAKE COHORT PER ROW** (Rainbow Path v3 §2.7, law K4;
    /// 2026-09-14, measured). A jump whose corridor overlaps a wake cohort
    /// still inside [`WAKE_LIFE_S`] of its own arrival RE-OWNS it — the
    /// cohort takes `alive_at := now` and the corridor's cells join it —
    /// instead of minting a second cohort beside it. Otherwise a fresh one.
    ///
    /// WHY, on glass. [`WAKE_MAX_CELLS`] binds at the far end of a corridor,
    /// so a reversal across more than 32 cells re-lays only the 32 nearest
    /// its landing; the cells past that stayed on the PREVIOUS cohort's
    /// clock while the re-laid ones rode the new one, and the two clocks
    /// came due 120–300 ms apart. The row tore exactly there: measured on
    /// a 44-cell run at 120 / 200 / 300 ms reversals, a 4–7 cell hole at
    /// columns 37–42 from +902 ms to +983 ms after the last Ctrl-E, on
    /// every cadence and on both the merged tree and main. With one cohort
    /// the whole corridor — re-laid and covered alike — rides one clock and
    /// leaves together, which is what K2 ("one interval ... until the
    /// wake's own drain") asks for.
    fn own_wake_cohort(
        &mut self,
        row: u16,
        to: u16,
        from: u16,
        anchor_col: u16,
        t0: f32,
        at: Instant,
    ) -> usize {
        let (lo, hi) = (to.min(from), to.max(from));
        let live = self.cohorts.iter().enumerate().find(|(_, c)| {
            c.row == row
                && c.wake
                && !c.abandoned
                && at.saturating_duration_since(c.alive_at).as_secs_f32() <= WAKE_LIFE_S
                && c.col0 <= hi
                && lo < c.col1
        });
        if let Some((i, _)) = live {
            let coh = &mut self.cohorts[i];
            coh.alive_at = at;
            coh.phase = Phase::Laying;
            coh.retract_col = None;
            return i;
        }
        self.mint_cohort(row, to, anchor_col, t0, at, true)
    }

    /// The walk origin a wake takes on `row`, as `(anchor_col, t0)`: the
    /// row's freshest cohort inside the chain window — abandoned, retracting
    /// or not: the band it continues may be leaving — shares its origin
    /// outright, so `t_at(col)` is the very number the band has at every
    /// column and continues past both its ends on the same walk. With no
    /// cohort on the row, the freshest anywhere is continued from one past
    /// its rightmost column, anchored at `landing`, as
    /// [`Ribbon::join_cohort`] does; red at the landing when nothing is laid.
    fn wake_origin(&self, row: u16, landing: u16, at: Instant) -> (u16, f32) {
        let fresh =
            |c: &&Cohort| at.saturating_duration_since(c.alive_at).as_secs_f32() <= CHAIN_GAP_MAX;
        if let Some(c) = self
            .cohorts
            .iter()
            .filter(fresh)
            .filter(|c| c.row == row)
            .max_by_key(|c| c.alive_at)
        {
            return (c.anchor_col, c.t0);
        }
        let t0 = self
            .cohorts
            .iter()
            .filter(fresh)
            .max_by_key(|c| c.alive_at)
            .map_or(0.0, |c| c.t_at(c.col1));
        (landing, t0)
    }

    /// The cohort a HOP's new cell at `col` joins: a WAKE cohort on the same
    /// row, not abandoned, inside the chain window, adjacent to or containing
    /// `col` — a held arrow's own trail, or a jump's wake it runs into.
    /// Otherwise a new wake cohort on the row's origin `(anchor_col, t0)`.
    /// Never a typed cohort: the hop leaves the word's clock and bounds alone.
    fn join_wake_cohort(
        &mut self,
        row: u16,
        col: u16,
        anchor_col: u16,
        t0: f32,
        at: Instant,
    ) -> usize {
        if let Some(i) = self.cohorts.iter().position(|c| {
            c.row == row
                && c.wake
                && !c.abandoned
                && at.saturating_duration_since(c.alive_at).as_secs_f32() <= CHAIN_GAP_MAX
                && col + 1 >= c.col0
                && col <= c.col1
        }) {
            return i;
        }
        self.mint_cohort(row, col, anchor_col, t0, at, true)
    }

    /// Push a new cohort covering `col` on `row` with the walk origin
    /// `(anchor_col, t0)`, born and alive at `at`; returns its index.
    fn mint_cohort(
        &mut self,
        row: u16,
        col: u16,
        anchor_col: u16,
        t0: f32,
        at: Instant,
        wake: bool,
    ) -> usize {
        let id = self.next_cohort;
        self.next_cohort = self.next_cohort.wrapping_add(1);
        // A wake keeps the floor grace: it is no phrase's, and its cells
        // expire on `WAKE_LIFE_S` before any longer rest could matter.
        let grace_s = if wake {
            PHRASE_REST_MIN_S
        } else {
            self.rest_s()
        };
        self.cohorts.push(Cohort {
            id,
            row,
            col0: col,
            col1: col + 1,
            anchor_col,
            t0,
            born: at,
            alive_at: at,
            abandoned: false,
            wake,
            retract_col: None,
            drain_right_first: false,
            phase: Phase::Laying,
            rejoinable: false,
            phrase: 0,
            grace_s,
            release_clock: None,
        });
        self.cohorts.len() - 1
    }

    /// The cohort this cell joins: one on the same row, adjacent to or
    /// containing `col`, still inside the chain window. Otherwise a new one —
    /// whose walk CONTINUES the freshest live cohort's from one past its
    /// rightmost column, so a wrap fold and a same-row rebirth both cross
    /// without a seam, and a genuinely cold start re-anchors at red.
    fn join_cohort(&mut self, row: u16, col: u16, at: Instant) -> usize {
        if let Some(i) = self.cohorts.iter().position(|c| {
            c.row == row
                && !c.abandoned
                && at.saturating_duration_since(c.alive_at).as_secs_f32() <= CHAIN_GAP_MAX
                && col + 1 >= c.col0
                && col <= c.col1
        }) {
            return i;
        }
        // THE HAND CAME BACK (2026-09-13): a typed key landing in or beside
        // a cohort this row's own program motion or a row change abandoned
        // (`Cohort::rejoinable`) takes it back and lifts the abandon — a
        // TUI's caret round trip, or a Backspace across a wrap. Without
        // this the band drained under a hand that never left it while the
        // new keys minted a second cohort beside it: a dark word inside a
        // live line. A jump the HAND made (Nav, Return) keeps its abandon.
        if let Some(i) = self.cohorts.iter().position(|c| {
            c.row == row
                && c.abandoned
                && c.rejoinable
                && at.saturating_duration_since(c.alive_at).as_secs_f32() <= CHAIN_GAP_MAX
                && col + 1 >= c.col0
                && col <= c.col1
        }) {
            let coh = &mut self.cohorts[i];
            coh.abandoned = false;
            coh.retract_col = None;
            return i;
        }
        let fresh = |t: Instant| at.saturating_duration_since(t).as_secs_f32() <= CHAIN_GAP_MAX;
        let t0 = self
            .cohorts
            .iter()
            .filter(|c| fresh(c.alive_at))
            .max_by_key(|c| c.alive_at)
            .map(|c| c.t_at(c.col1))
            .or_else(|| {
                // A finished swoosh's walk continues too (`Cohort::t0`'s
                // documented 5 s continuation, dead code until 2026-09-13:
                // `retire` drops the cohort with its cells).
                self.last_walk
                    .filter(|w| fresh(w.alive_at))
                    .map(|w| w.t0 + walk_t(f32::from(w.col1) - f32::from(w.anchor_col)))
            })
            .unwrap_or(0.0);
        self.mint_cohort(row, col, col, t0, at, false)
    }

    /// Retract the SUFFIX of `row` from `col` on — every live cell at or right
    /// of the caret, and at least `min_cells` columns — FARTHEST-FIRST inside
    /// `span_s`, each cell then spending [`RETRACT_FADE_S`] to reach exactly
    /// zero. The backspace's `2 × 0.24 s` and the kill's `12·n + 240 ms` are
    /// the same law with two spans (§4, v1 verbatim: "every live ribbon cell on
    /// the landing row at or beyond the new caret is stamped … CONTIGUOUS at
    /// every frame … nothing is ever removed from the MIDDLE").
    fn retract_suffix(&mut self, row: u16, col: u16, min_cells: u16, at: Instant, span_s: f32) {
        let far = self
            .cells
            .iter()
            .filter(|c| c.row == row && c.col >= col && !c.leaving())
            .map(|c| c.col)
            .max();
        let last = far
            .unwrap_or(col)
            .max(col.saturating_add(min_cells.saturating_sub(1)));
        let n = f32::from(last - col + 1);
        let step = span_s / n;
        for cell in &mut self.cells {
            if cell.row != row || cell.col < col || cell.col > last || cell.leaving() {
                continue;
            }
            // Farthest from the caret goes first, so the mark visibly slurps
            // back toward the hand rather than blinking out.
            let rank = f32::from(last - cell.col);
            cell.retract_at = at.checked_add(std::time::Duration::from_secs_f32(rank * step));
        }
    }

    /// A real jump that LEAVES THE RUN abandons the band (since
    /// 2026-09-14 a same-row jump whose corridor is lit end to end is a
    /// scrub and never reaches here — [`Ribbon::scrub`]; the 2026-09-09
    /// ruling "a real jump on the same row abandons the band into its
    /// swoosh" is kept only for a jump past the band's oldest cell, off its
    /// end onto dark ground, or to another row): every cohort still in its
    /// grace or reach — except the jump's own wake, `keep` — is sent
    /// straight into its retract by rewinding `alive_at` to
    /// [`RETRACT_START_S`] ago. The
    /// light leaves CONTINUOUSLY from the level it has (the retract's `spend`
    /// starts at exactly `1.0`), where v1's life clamp stepped the melt on
    /// the jump frame; and a cohort already retracting is left on its own
    /// clock. Under the corridor the wake has already taken the band's cells
    /// over ([`Ribbon::wake`]), so what retracts here is only the light the
    /// jump left behind beyond it — and a band that was already leaving
    /// when the jump came, which the wake did not touch.
    ///
    /// `toward` pre-sets the retract's target and its drain order. A
    /// PROGRAM's jump takes the band back into the hand's last column
    /// rather than wherever the program parked the cursor (a repaint
    /// parking at column 0 for one frame used to pull the whole band left
    /// across the line while the hand kept typing at its end: a lit island
    /// between two dark stretches), and `rejoinable` lets the hand's next
    /// key take the band back ([`Ribbon::join_cohort`]). A SAME-ROW jump
    /// the hand made passes `(landing, leftward)`: the abandoned cohorts
    /// take the landing as their frozen target (R1, the caret they let go
    /// under) and a drain order that FOLLOWS THE METEOR — origin end
    /// first, landing end last — so under the overlay corridor being born
    /// along the flight ([`Ribbon::wake`]) no band cell starts spending
    /// before the corridor cell above it is born: cell `j` from the origin
    /// is born at `T · j / n ≤ 0.12 · j / n` and starts spending at
    /// `0.40 · j / n`. A row change passes `None` and the per-row target
    /// law decides at the retract's first frame.
    fn abandon(
        &mut self,
        at: Instant,
        keep: Option<u32>,
        toward: Option<(u16, bool)>,
        rejoinable: bool,
    ) {
        for coh in &mut self.cohorts {
            if keep == Some(coh.id) {
                continue;
            }
            coh.release_clock = None;
            if !coh.abandoned {
                coh.rejoinable = rejoinable;
            }
            // Rewound to ITS OWN retract start: a cohort whose phrase
            // rested longer is sent the same `0.40 + 0.24 s` out.
            let Some(rewound) =
                at.checked_sub(std::time::Duration::from_secs_f32(coh.retract_start()))
            else {
                continue;
            };
            coh.abandoned = true;
            if coh.alive_at > rewound {
                coh.alive_at = rewound;
            }
            if let Some((col, leftward)) = toward
                && coh.retract_col.is_none()
            {
                coh.retract_col = Some(col);
                coh.drain_right_first = leftward;
            }
        }
    }

    /// **RETIRE SPECIFIC LIVE CELLS ON THE FAST MELT** (2026-09-12, the
    /// abandoned band). Every live cell of this ribbon that IS one of
    /// `cells` — named by identity, `(row, col, born)`, not by position —
    /// takes [`Cell::retire_at`] `= at`: it spends to exactly zero over
    /// [`RETIRE_MELT_S`] on the retract's own alpha law
    /// ([`super::timing::spend`]), its attack frozen at the stamp so it never
    /// brightens after it ([`Ribbon::env_of`]), and leaves the pool on the
    /// first plan past the melt ([`Ribbon::retire`]). The host's CONTENT
    /// WITNESS calls this for a cell whose glyph it has seen REPLACED
    /// (`Engine::witness_rows`) — under a live cell, or under a cell it
    /// released earlier whose cohort is still retracting: the melt then
    /// multiplies into the retract's envelope ([`Ribbon::env_of`]), so the
    /// cell is out in `RETIRE_MELT_S` and never brighter than it was. A
    /// cell laid over ink that is still there is never touched, which is
    /// D2's "a prompt redraw must not wipe the ribbon" kept: an erase that
    /// puts the same text back before the host samples changes nothing
    /// here.
    ///
    /// BY IDENTITY, because a position can hold more than one live cell:
    /// one owner per cell holds per COHORT ([`Ribbon::place`]) and a key
    /// typed back over a cell an abandoned cohort still owns
    /// ([`Ribbon::abandon`]: rewound into its retract, resident and not
    /// [`Cell::leaving`] for `RETRACT_DUR_S + RETRACT_FADE_S`) mints a
    /// second. The witness names the stale cell for the glyph it was laid
    /// under; a stamp by position took the key's own cell with it on its
    /// birth frame — light WITH a keystroke behind it, gone in 120 ms, the
    /// exact inverse of the anti-stray law. The birth is the one thing two
    /// cells at a position never share.
    ///
    /// Returns how many cells were NEWLY stamped. A cell already leaving —
    /// retracting after a Backspace, or retired — keeps the clock it has,
    /// so a witness that fires twice cannot restart a melt, and a cell that
    /// is not laid (or laid at that position with another birth) is not
    /// counted. `O(pool × cells)`, off the frame path's steady state: a
    /// retirement is an event, not a frame.
    pub fn retire_cells(&mut self, cells: &[(u16, u16, Instant)], at: Instant) -> usize {
        let mut n = 0;
        for cell in &mut self.cells {
            if cell.leaving() || !cells.contains(&(cell.row, cell.col, cell.born)) {
                continue;
            }
            cell.retire_at = Some(at);
            if let Some(coh) = self.cohorts.iter_mut().find(|c| c.id == cell.cohort) {
                coh.release_clock = None;
            }
            n += 1;
        }
        n
    }

    /// **RELEASE SPECIFIC LIVE CELLS TO THE SWOOSH** (2026-09-14, the new
    /// line's fade — the owner, on the 0.85 build: *"when I went to a new
    /// line the contrail disappeared versus nicely fading"*). The content
    /// witness's OTHER verdict: the glyph under each of `cells` (by
    /// identity, as [`Ribbon::retire_cells`]) has GONE, and nothing replaced
    /// it — Claude Code's Enter clearing the composer, a Ctrl-U, a cleared
    /// line. The light has nothing to be wrong over, so it does not take
    /// the [`RETIRE_MELT_S`] cut that stale light over REPLACED text takes;
    /// it leaves the way a row the hand left leaves everywhere else: every
    /// named cell's cohort is ABANDONED into its retract
    /// ([`Ribbon::abandon_row`]'s law — `alive_at` rewound to
    /// [`RETRACT_START_S`] ago, so the retract's `spend` starts at exactly
    /// the light it has and the mark is drawn into the hand farthest-first
    /// over `RETRACT_DUR_S + RETRACT_FADE_S`, the head the last light to
    /// go), the target left for `advance_swoosh` to resolve on the retract
    /// frame (the caret on its own row, the row's own end otherwise). NOT
    /// rejoinable: the text is gone, so a key landing back on the row lays
    /// its own light and never takes this back. A cohort already abandoned
    /// keeps the clock it has (its retract is under way) and only loses its
    /// rejoin.
    ///
    /// Returns how many named cells were resident and not already leaving
    /// — the cells whose text went, as `retire_cells` counts — so
    /// `ribbon_retired=` says a band went out because its text went
    /// whichever clock it left on. Nothing is stamped on the cells
    /// themselves: they stay owners of their columns for the retract's
    /// span, exactly as an abandoned row's do, and so they can still be
    /// named again — by the witness, for a glyph landing under them, onto
    /// `retire_cells` — which is why the witness keeps their records and
    /// reports such re-verdicts, and `Engine::witness_rows` counts each
    /// cell once. Off the frame path's steady state: a release is an event.
    pub fn release_cells(&mut self, cells: &[(u16, u16, Instant)], at: Instant) -> usize {
        let Some(rewound) = at.checked_sub(std::time::Duration::from_secs_f32(RETRACT_START_S))
        else {
            return 0;
        };
        let mut n = 0;
        for i in 0..self.cells.len() {
            let cell = self.cells[i];
            if cell.leaving() || !cells.contains(&(cell.row, cell.col, cell.born)) {
                continue;
            }
            n += 1;
            let Some(coh) = self.cohorts.iter_mut().find(|c| c.id == cell.cohort) else {
                continue;
            };
            if !coh.abandoned {
                coh.release_clock = Some(ReleaseClock {
                    alive_at: coh.alive_at,
                    retract_col: coh.retract_col,
                    phase: coh.phase,
                    rejoinable: coh.rejoinable,
                    cells: self.cells.iter().filter(|c| c.cohort == coh.id).count(),
                });
            }
            coh.rejoinable = false;
            if coh.abandoned {
                continue;
            }
            coh.abandoned = true;
            if coh.alive_at > rewound {
                coh.alive_at = rewound;
            }
        }
        n
    }

    /// Whether a release still owns the cohort's clock. The witness checks
    /// the original glyph identities separately; an explicit abandon or
    /// retirement never permits restoration through this seam.
    pub(super) fn restore_releases(
        &mut self,
        candidates: &[(u32, usize)],
        accepted: &mut Vec<u32>,
    ) {
        accepted.clear();
        if candidates.is_empty() {
            return;
        }
        for coh in &mut self.cohorts {
            let Some(saved) = coh.release_clock else {
                continue;
            };
            let Ok(i) = candidates.binary_search_by_key(&coh.id, |&(id, _)| id) else {
                continue;
            };
            if candidates[i].1 != saved.cells {
                continue;
            }
            // Recover the exact saved clock; no new life or birth is earned.
            coh.release_clock = None;
            coh.alive_at = saved.alive_at;
            coh.retract_col = saved.retract_col;
            coh.phase = saved.phase;
            coh.rejoinable = saved.rejoinable;
            coh.abandoned = false;
            accepted.push(coh.id);
        }
        accepted.sort_unstable();
    }

    /// **NO TWO TYPED COHORTS ABUT ON A ROW** (2026-09-13 — the owner's
    /// slits). A typed key whose echo lands on the same tick as another's
    /// is replayed against the tick's landing caret and lays the cell one
    /// PAST the live cohort's end; `join_cohort` refuses it, a second cohort
    /// is minted, and the host's sweep then fills the hole into the OLD one.
    /// The row is contiguous but `build_runs` splits it into two runs at the
    /// cohort change, and `plan_run` feathers the newer run's left boundary
    /// at [`RUN_TAIL_EASE`] — a 5 px column at a quarter of the body's
    /// coverage, then 4 px at half, then 5 px at 85 %, measured on the
    /// owner's crops and reproduced 14/14 on the installed app with paired
    /// keys. Whatever the replay order, two live typed cohorts that abut on
    /// a row are ONE mark: the newer is folded into the older here, every
    /// frame, before the geometry is planned. The walk is continuous by
    /// construction (the newer cohort continued the older's from its end).
    fn heal_seams(&mut self) {
        let mut i = 0;
        while i < self.cohorts.len() {
            let a = self.cohorts[i];
            let hot = |c: &Cohort| !c.wake && !c.abandoned && c.retract_col.is_none();
            let next = self.cohorts.iter().position(|b| {
                b.id != a.id && b.row == a.row && b.col0 == a.col1 && hot(b) && hot(&a)
            });
            let Some(j) = next else {
                i += 1;
                continue;
            };
            let b = self.cohorts[j];
            let (into, from) = if a.born <= b.born { (i, j) } else { (j, i) };
            let (into_id, from_id) = (self.cohorts[into].id, self.cohorts[from].id);
            // The newer writer of a column wins, as `build`'s sort would
            // have picked it; the shadowed older cell leaves the pool.
            let mut idx = 0;
            while idx < self.cells.len() {
                let c = self.cells[idx];
                let shadowed = c.cohort == into_id
                    && self.cells.iter().any(|d| {
                        d.cohort == from_id && d.row == c.row && d.col == c.col && d.born >= c.born
                    });
                if shadowed {
                    self.cells.remove(idx);
                } else {
                    idx += 1;
                }
            }
            for c in &mut self.cells {
                if c.cohort == from_id {
                    c.cohort = into_id;
                }
            }
            let merged = self.cohorts[from];
            let coh = &mut self.cohorts[into];
            coh.col0 = coh.col0.min(merged.col0);
            coh.col1 = coh.col1.max(merged.col1);
            coh.alive_at = coh.alive_at.max(merged.alive_at);
            coh.phase = Phase::Laying;
            self.cohorts.remove(from);
            // Re-examine from the start: a merge can create a new abutment.
            i = 0;
        }
    }

    // -- the frame ---------------------------------------------------------

    /// Build this frame's [`Ribbon::plan`] and [`FieldIndex`] — the geometry
    /// pass, run once per tick BEFORE any producer emits and before stardust
    /// deals a birth, because §5.4's zones are relative to [`Band::top`].
    ///
    /// The order is fixed and each step depends on the one before it: sync the
    /// ink table for the live theme, advance the exit swoosh (which may lay
    /// the reach's extension cells), retire what is out of time, then sort
    /// ONCE and fill the index and the geometry from that same pass (§18).
    pub fn plan(&mut self, ctx: &Ctx<'_>) {
        self.ink.sync(ctx.cfg);
        self.ground = ctx.cfg.theme_bg;
        self.caret = Some(ctx.caret);
        self.reduced = ctx.cfg.reduced_motion;
        self.cell_h = ctx.geom.ch as f32;
        self.wedge_px = (Self::body_profile(ctx.cfg).dn_ch - DN_FLOOR_CH).max(0.0) * self.cell_h;
        self.advance_swoosh(ctx);
        self.retire(ctx);
        self.heal_seams();
        self.build(ctx);
    }

    /// Drive the exit choreography and lay the reach's extension cells.
    ///
    /// The beats are staged in a FIXED array rather than a `Vec`: the reach
    /// can only ever want [`REACH_BEATS`] cells per cohort and §18's rule is
    /// that nothing on the frame path allocates, not that nothing on the
    /// STEADY frame path does.
    fn advance_swoosh(&mut self, ctx: &Ctx<'_>) {
        let now = ctx.now;
        let mut reach = [(0u16, 0u16, 0u32); REACH_STAGE];
        let mut n_reach = 0usize;
        for i in 0..self.cohorts.len() {
            let coh = self.cohorts[i];
            let idle = now.saturating_duration_since(coh.alive_at).as_secs_f32();
            let Some(phase) = Phase::at(idle, coh.grace_s) else {
                self.cohorts[i].phase = Phase::Fading;
                continue;
            };
            self.cohorts[i].phase = phase;
            // THE RETRACT'S TARGET is fixed the frame the retract begins,
            // and it stays: the mark is drawn back into the hand that let go
            // of it — or, on a row the caret is not on, into its own end
            // nearest the hand in logical order ([`Ribbon::retract_target`])
            // — not toward wherever the caret goes next (see
            // `Cohort::retract_col`). The drain order is frozen with it.
            if phase.is_retracting() && self.cohorts[i].retract_col.is_none() {
                // PER ROW (2026-09-13, restated by Rainbow Path v3 §2.6):
                // a cohort on the caret's row is drawn back into the caret;
                // one on a row ABOVE it into its own right end (the fold,
                // the point the hand left the row at), one BELOW into its
                // left end, and the drain order is frozen with it. The live
                // caret's column on another row used to pull an abandoned
                // row's light sideways toward a column that belongs to a
                // different line.
                let coh = self.cohorts[i];
                let (target, right_first) = self.retract_target(&coh);
                self.cohorts[i].retract_col = target;
                self.cohorts[i].drain_right_first = right_first;
            }
            // Under reduced motion the mark is STATIC (§6.11): no reach; and
            // under a curtain nothing is laid, the reach included.
            if phase != Phase::Reaching || ctx.cfg.reduced_motion || self.curtain.is_some() {
                continue;
            }
            // ONE EXTENSION CELL PER BEAT while the mark is still short of the
            // four-letter span. They may overlap letters the burst never typed
            // — accepted by design; the ribbon is source-over and capped.
            let beats = (((idle - coh.grace_s) / REACH_STEP_S).floor() as i32 + 1)
                .clamp(0, i32::from(REACH_BEATS)) as u16;
            let span = coh.col1.saturating_sub(coh.col0);
            let want = FOUR_LETTER_CELLS.saturating_sub(span).min(beats);
            for k in 0..want {
                let Some(c) = coh.col0.checked_sub(k + 1) else {
                    break;
                };
                if n_reach == REACH_STAGE {
                    break;
                }
                reach[n_reach] = (coh.row, c, coh.id);
                n_reach += 1;
            }
        }
        for &(row, col, cohort) in &reach[..n_reach] {
            if self.cells.iter().any(|c| c.row == row && c.col == col) {
                continue;
            }
            self.lay_reach_cell(row, col, cohort);
        }
    }

    /// An extension cell of the exit swoosh's reach, for cohort `cohort`.
    ///
    /// It INHERITS the mark's own envelope — `born`, `life_s` and `cov0` come
    /// from ITS OWN cohort's tail cell — for two reasons. It is one object
    /// with the mark, so it must die with it; and because its light can
    /// therefore never exceed the light already on glass, the reach cannot
    /// brighten a ribbon whose hand has already left (the `never brightens`
    /// law). Its stop is the cohort's walk read one cell further back.
    fn lay_reach_cell(&mut self, row: u16, col: u16, cohort: u32) {
        let Some(coh) = self.cohorts.iter().find(|c| c.id == cohort).copied() else {
            return;
        };
        let Some(&tail) = self
            .cells
            .iter()
            .filter(|c| c.row == row && c.cohort == cohort)
            .min_by_key(|c| c.col)
        else {
            return;
        };
        self.cells.push(Cell {
            row,
            col,
            cohort,
            t: coh.t_at(col),
            born: tail.born,
            attack_at: tail.attack_at,
            life_s: tail.life_s,
            cov0: tail.cov0,
            typing: false,
            retract_at: tail.retract_at,
            retire_at: tail.retire_at,
            birth_disp: tail.birth_disp,
            edge_cells: tail.edge_cells,
            layer: tail.layer,
            rearm: tail.rearm,
        });
        if let Some(coh) = self.cohorts.iter_mut().find(|c| c.id == cohort) {
            coh.col0 = coh.col0.min(col);
        }
    }

    /// Drop what is out of time. A cell that has expired is REMOVED here, not
    /// left in place for a deadline read to notice, which is what lets
    /// [`Ribbon::at_rest`] answer "is anything visible" without a clock.
    fn retire(&mut self, ctx: &Ctx<'_>) {
        let now = ctx.now;
        let ember_done = self
            .ember_at
            .is_some_and(|at| now.saturating_duration_since(at).as_secs_f32() >= FOCUS_EMBER_S);
        let curtain_done = self
            .curtain
            .is_some_and(|at| now.saturating_duration_since(at).as_secs_f32() >= CURTAIN_S);
        if ember_done || curtain_done {
            // The ember clock goes with the pool it burnt: left standing it
            // re-offered `ember_at + FOCUS_EMBER_S` — a past instant, floored
            // to the tail — on every deadline read, a wake with nothing on
            // glass until the next focus gain (the 2026-09-14 audit). The
            // curtain's clock goes for the same reason, and the next key lays
            // afresh in the new space.
            self.cells.clear();
            self.cohorts.clear();
            self.ember_at = None;
            self.curtain = None;
            return;
        }
        let cohorts = &self.cohorts;
        self.cells.retain(|cell| {
            if now
                .saturating_duration_since(live_since(cohorts, cell))
                .as_secs_f32()
                >= cell.life_s
            {
                return false;
            }
            if let Some(at) = cell.retract_at
                && now.saturating_duration_since(at).as_secs_f32() >= RETRACT_FADE_S
            {
                return false;
            }
            if let Some(at) = cell.retire_at
                && now.saturating_duration_since(at).as_secs_f32() >= RETIRE_MELT_S
            {
                return false;
            }
            cohorts.iter().any(|c| {
                c.id == cell.cohort
                    && Phase::at(
                        now.saturating_duration_since(c.alive_at).as_secs_f32(),
                        c.grace_s,
                    )
                    .is_some()
            })
        });
        let cells = &self.cells;
        // The walk a cohort leaves when it goes, so a same-row rebirth can
        // continue it (`join_cohort`).
        if let Some(gone) = self
            .cohorts
            .iter()
            .filter(|coh| !cells.iter().any(|c| c.cohort == coh.id))
            .max_by_key(|coh| coh.alive_at)
        {
            self.last_walk = Some(LastWalk {
                col1: gone.col1,
                anchor_col: gone.anchor_col,
                t0: gone.t0,
                alive_at: gone.alive_at,
            });
        }
        self.cohorts
            .retain(|coh| cells.iter().any(|c| c.cohort == coh.id));
    }

    /// The plan and the index, from ONE sort.
    fn build(&mut self, ctx: &Ctx<'_>) {
        let cols = u16::try_from(ctx.geom.cols).unwrap_or(u16::MAX);
        self.plan.clear();
        self.runs.clear();
        // WHAT THE BLOCK LEAVES BEHIND DRAINS FROM THE LEVEL IT SHOWED
        // (2026-09-14): a leaving cell the attach floored last frame
        // (`attach`) that is no longer under the caret — a held Backspace
        // moves the block one cell left per key, off the cell it erased a
        // key ago — has been spending on its own clock under the floor,
        // so uncovering it would drop the rail under it by a quarter in
        // one frame at 30/s (`spend(33 ms / 240 ms)`). Its retract restarts
        // here, at the uncovering: it drains from full over the fade it
        // always had, one cell behind the block. A retract not yet begun (a
        // kill's farthest-first schedule) keeps its date; a content
        // retirement keeps its own clock (its law is "never brighter than
        // the cell was"); a cell a fresh lay shadows is left alone.
        if let Some((row, col)) = self.attach
            && ctx.caret != (row, col)
            && !self
                .cells
                .iter()
                .any(|c| c.row == row && c.col == col && !c.leaving())
        {
            for c in &mut self.cells {
                if c.row == row
                    && c.col == col
                    && let Some(at) = c.retract_at
                    && at < ctx.now
                {
                    c.retract_at = Some(ctx.now);
                }
            }
        }
        self.attach = None;
        self.attach_floor = None;
        self.index.begin(cols);
        if self.cells.is_empty() {
            return;
        }
        // ONE SORT, and it is a NEWEST-FIRST sort: `row` and `col` ascending,
        // then the pool index DESCENDING. The pool is append-ordered, so the
        // first entry of a `(row, col)` group is that cell's newest — its
        // owner — and both the index (first writer wins) and the geometry
        // (`dedup_by` keeps the first of a group) take the same one.
        //
        // Sorting the other way and letting the LAST writer win would give the
        // index the right answer and hand the GEOMETRY the shadowed cell,
        // which is a bug that draws one colour and reports another.
        let mut sorted = mem::take(&mut self.sorted);
        sorted.clear();
        sorted.reserve(self.cells.len());
        for (i, c) in self.cells.iter().enumerate() {
            sorted.push((c.row, c.col, u32::try_from(i).unwrap_or(u32::MAX)));
        }
        // THE LAYERS (K1): where a BORN `Over` cell shares a position with a
        // `Base` cell, the `Over` cell is the position's owner — the
        // corridor draws over the band it covers, whatever the band's
        // clock — and an `Over` cell not yet born (the flight clock) leaves
        // the band beneath it on glass until it is: the corridor is dark
        // ahead of the meteor only where nothing was lit.
        let now = ctx.now;
        let cells = &self.cells;
        // The position's owner, by rank: a BORN `Over` cell first, then the
        // newest `Base` cell, and an UNBORN `Over` cell last of all — it is
        // dark, and the band under it is what is on glass until it is born.
        let rank = |i: u32| -> u8 {
            match cells.get(i as usize) {
                Some(c) if c.layer == Layer::Over => u8::from(now < c.born) * 2,
                _ => 1,
            }
        };
        sorted.sort_unstable_by(|a, b| {
            a.0.cmp(&b.0)
                .then(a.1.cmp(&b.1))
                .then(rank(a.2).cmp(&rank(b.2)))
                .then(b.2.cmp(&a.2))
        });
        for &(row, col, idx) in &sorted {
            let Some(cell) = self.cells.get(idx as usize) else {
                continue;
            };
            self.index.set(row, col, cell.t);
        }
        // Keep only the owner of each cell for the geometry pass, so a run is
        // one polyline and not two stacked on the same columns.
        sorted.dedup_by(|a, b| a.0 == b.0 && a.1 == b.1);
        self.slabs = Self::slabs_for(ctx, sorted.len());
        self.build_runs(ctx, &sorted);
        self.sorted = sorted;
        // EMIT ORDER: the run the hand is on first, then newest first. The
        // budget sheds from the END of the emit, so this is what makes "shed
        // from the tail, never from the head" true ACROSS rows and not only
        // within one: a hot wrapped paragraph keeps its current row lit and
        // loses the oldest row's far end, never the other way round.
        self.runs
            .sort_unstable_by(|a, b| b.at_caret.cmp(&a.at_caret).then(b.born.cmp(&a.born)));
    }

    /// The vertex density the budget allows this frame (§18), from the
    /// frame's TOTAL live cells across every row: the transverse profile
    /// costs one quad per device row of every slab, and `ribbon_beam` tiles
    /// at least one slab per vertex pair, so the frame's cost is
    /// `cells × slabs × rows` and `slabs` is the one term that can give.
    /// Sized against the total rather than per run, so two hot rows fit the
    /// same budget one does instead of each spending it in full.
    fn slabs_for(ctx: &Ctx<'_>, cells: usize) -> usize {
        let prof = Self::body_profile(ctx.cfg);
        let ch = ctx.geom.ch as f32;
        // The estimate reads the SETTLED reach below the spine (`DN_TOP_CH`),
        // not the comet's lobe at the hand: the lobe spans `BLOOM_REACH_CELLS`
        // of one run, and `ribbon_beam`'s own truncation is the backstop for
        // the rows an estimate misses. Under the flat spelling the profile's
        // `dn_ch` IS `DN_TOP_CH`, so this is the number it always was.
        let body = ((prof.up_ch + DN_TOP_CH) * ch).ceil().max(1.0) + 2.0;
        // THE RAIL is a second polyline over the same slabs, one quad per
        // device row of its reach (2026-09-13).
        let rail = if Self::rail_lit(ctx.cfg) {
            (DN_TOP_CH * ch).ceil() + 1.0
        } else {
            0.0
        };
        let rows = body + rail;
        // THE ATTACH (2026-09-14): the run at the hand continues one cell
        // under the caret (`plan_run`), one more cell of slabs to price.
        let cells = cells + 1;
        let per_slab = (cells as f32 * rows).max(1.0);
        let fit = (RIBBON_QUAD_BUDGET as f32 / per_slab).floor();
        (fit.max(1.0) as usize).clamp(1, SLABS_PER_CELL)
    }

    /// Walk the deduped index into contiguous runs — contiguous in COLUMN and
    /// in COHORT — and plan each one.
    fn build_runs(&mut self, ctx: &Ctx<'_>, sorted: &[(u16, u16, u32)]) {
        fn cohort_of(cells: &[Cell], e: &(u16, u16, u32)) -> Option<u32> {
            cells.get(e.2 as usize).map(|c| c.cohort)
        }
        let mut s = 0usize;
        while s < sorted.len() {
            let mut e = s;
            while e + 1 < sorted.len()
                && sorted[e + 1].0 == sorted[s].0
                && sorted[e + 1].1 == sorted[e].1 + 1
                && cohort_of(&self.cells, &sorted[e + 1]) == cohort_of(&self.cells, &sorted[e])
            {
                e += 1;
            }
            // THE RUN'S NEIGHBOURS (2026-09-14): where another run abuts
            // this one on its row — another cohort's cells, a wake's, a
            // draining cell's — the two runs SHARE their seam vertex: the
            // run to the left was planned first (`sorted` is row-major),
            // so its last vertex is handed to this run as its first, and
            // this run's first cell hands its light to the run on the
            // left's last boundary. The seam column then carries the older
            // side's light at the older side's shape, not a fresh feather
            // (`plan_run`).
            let row = sorted[s].0;
            let nb = |i: Option<usize>, col: Option<u16>| -> Option<Cell> {
                let (i, col) = (i?, col?);
                let e = sorted.get(i)?;
                (e.0 == row && e.1 == col).then(|| self.cells.get(e.2 as usize).copied())?
            };
            let abuts_left = nb(s.checked_sub(1), sorted[s].1.checked_sub(1)).is_some();
            let seam = if abuts_left {
                self.runs
                    .last()
                    .filter(|r| r.row == row && r.col1.checked_add(1) == Some(sorted[s].1))
                    .and_then(|r| self.plan.get(r.hi.checked_sub(1)?).copied())
            } else {
                None
            };
            let right = nb(Some(e + 1), sorted[e].1.checked_add(1));
            self.plan_run(ctx, &sorted[s..=e], seam, right);
            s = e + 1;
        }
    }

    /// The per-cell sample every boundary is the midpoint of: `(spine, up,
    /// dn, cov)`. The plateau is NOT sampled here — it is a constant of
    /// [`BodyProfile`], and computing it per cell would be exactly the dead
    /// per-station work §18 deletes.
    fn sample(&self, ctx: &Ctx<'_>, cell: &Cell, sp: f32) -> (f32, f32, f32, f32) {
        let chf = ctx.geom.ch as f32;
        let prof = Self::body_profile(ctx.cfg);
        let reduced = ctx.cfg.reduced_motion;
        // THE WAVE rides the LIVE spine, and only ever settles: its amplitude
        // is `WAVE_AMP_CELLS · ch · disp` (0.110 ch — twice v1's, 2026-09-08),
        // so when the hand lifts the mark flattens. The head is pinned
        // (`WAVE_HEAD_PIN`) so the cell under the hand never moves. Under
        // reduced motion there is no wave at all (§6.11).
        let room = (DN_TOP_CH - DN_FLOOR_CH) * chf;
        let amp = if reduced {
            0.0
        } else {
            (WAVE_AMP_CELLS * chf * clamp01(ctx.disp)).min(room) * smoothstep01(sp / WAVE_HEAD_PIN)
        };
        let wave = ((sp * std::f32::consts::TAU * WAVE_CYCLES) - ctx.phase * std::f32::consts::TAU)
            .sin()
            * amp;
        // THE WEDGE: fatter under the hand, settling into the leading behind
        // it. Shape, not light. Under the comet (2026-09-13) the wedge opens
        // to `COMET_DN_HAND_CH` — `prof.dn_ch` — at the hand: the comet's
        // lower lobe, which the rail fills with vivid ink.
        let behind = sp * self.mark_span(cell);
        let bloom = clamp01(ctx.disp) * (1.0 - smoothstep01(behind / BLOOM_REACH_CELLS));
        // The lobe needs the LEADING below the row. The grid's last row has
        // none — its spine is clamped up by `dn` so the body ends at the
        // grid bottom — and a deeper lobe there would only push the body
        // further into the row above and the rail into the typed cell, so
        // that row keeps the flat wedge. (Under the flat spelling the two
        // are the same number.)
        let room_below = ctx.geom.fx_bot() as f32
            - (f32::from(ctx.geom.origin_y) + (f32::from(cell.row) + 1.0) * chf);
        let last_row = room_below < prof.dn_ch * chf;
        let dn_ch = if last_row { DN_TOP_CH } else { prof.dn_ch };
        let dn_open = chf * (DN_FLOOR_CH + (dn_ch - DN_FLOOR_CH) * clamp01(bloom));
        let dn = (dn_open - wave.max(0.0)).max(DN_FLOOR_CH * chf);
        // THE COMET'S TAPER (2026-09-13): the reach ABOVE the spine thins
        // from the head's full `up` to `COMET_UP_TAIL_CH / TALL_UP_CH` of it
        // over `COMET_TAPER_CELLS` behind the head, so the tail is thinner
        // than the head and the hand reads as the origin of the light. The
        // head cell (`behind == 0`) keeps the full body. Shape, not light;
        // the flat spelling keeps the same `up` at every cell.
        // The grid's last row keeps the FLAT body whole (review, 2026-09-13):
        // without the lobe and the rail below it, a tapered tail there was
        // the worst of both bodies — thinner than the flat one and no more
        // poured from the hand.
        let up_ch = if Self::comet(ctx.cfg) && !last_row {
            prof.up_ch
                * (1.0
                    - (1.0 - COMET_UP_TAIL_CH / TALL_UP_CH)
                        * smoothstep01(behind / COMET_TAPER_CELLS))
        } else {
            prof.up_ch
        };
        let up = (up_ch * chf + wave.min(0.0)).max(0.0);
        let spine = (f32::from(ctx.geom.origin_y) + (f32::from(cell.row) + 1.0) * chf + wave)
            .min(ctx.geom.fx_bot() as f32 - dn)
            .max(ctx.geom.fx_top() as f32);
        (spine, up, dn, self.cov_of(ctx, cell))
    }

    /// The mark's own length in cells — the denominator `sp` was normalized
    /// by, so `sp · span` is "how many cells behind the head this one is",
    /// which is the coordinate the wedge is stated in.
    fn mark_span(&self, cell: &Cell) -> f32 {
        self.cohorts
            .iter()
            .find(|c| c.id == cell.cohort)
            .map_or(1.0, |c| f32::from(c.col1.saturating_sub(c.col0)).max(1.0))
    }

    /// **THE CELL'S ENVELOPE**, `0..1` — everything that opens and closes a
    /// cell's light AFTER its birth price: the 18 ms `edge-in`, the expiry
    /// melt, the retract's `spend`, the focus ember and its `edge-in` rearm,
    /// and the exit swoosh's farthest-first drain. It is the ONE clock the
    /// body and the hot edge share: [`Ribbon::cov_of`] multiplies it into the
    /// body's request, and [`Ribbon::emit_hot_edge`] multiplies the HEAD
    /// cell's into the hairline's gain, so the white line embers, spends,
    /// melts and swooshes WITH the body under it and reaches exactly zero
    /// with it. A hairline priced from `birth_disp` alone rode the retract at
    /// full coverage over a body that had spent to nothing and then snapped
    /// off when the cells retired — a white line with no keystroke behind it,
    /// which is the anti-stray law's own complaint.
    ///
    /// Except for the two sanctioned attacks (`edge-in` at birth, and the
    /// rearm's `edge-in` on focus regain) every term is non-increasing in
    /// `now`, which is what lets "never brightens after the last keystroke"
    /// hold for the hairline by construction.
    fn env_of(&self, ctx: &Ctx<'_>, cell: &Cell) -> f32 {
        let now = ctx.now;
        // A wake cell laid AHEAD of its birth (`WAKE_BORN_LAG_S`) is dark
        // until it is born: the attack starts at `born`, never before it.
        if now < cell.born {
            return 0.0;
        }
        let reduced = ctx.cfg.reduced_motion;
        let coh = self.cohorts.iter().find(|c| c.id == cell.cohort);
        // The attack runs from its visual origin, which a live re-wet may
        // retain across a fresh identity; expiry runs from the COHORT's
        // clock (`live_since`): a key refreshes the whole run. A
        // RETIRED cell's attack is FROZEN at the instant it was retired
        // (`min(now, retire_at)`, and the rearm's attack below reads the
        // same clock): the melt is multiplied in further down, and a cell
        // retired inside its 18 ms `edge-in` must not keep climbing under
        // it — "never brighter than the cell was" is the retirement's law.
        let attack_now = cell.retire_at.map_or(now, |at| now.min(at));
        let age = attack_now
            .saturating_duration_since(cell.attack_at)
            .as_secs_f32();
        let spent = now
            .saturating_duration_since(coh.map_or(cell.born, |c| cell.born.max(c.alive_at)))
            .as_secs_f32();
        let u = (spent / cell.life_s.max(1e-3)).clamp(0.0, 1.0);
        // UNDER REDUCED MOTION every mark is static and takes the theme's ONE
        // linear fade over the last `REDUCED_MOTION_FADE_MS` of whatever ends
        // it (§6.11) — the natural life, the retract stamp, the ember and the
        // swoosh all resolve through `reduced_fade`; the 18 ms `edge-in`
        // stays, because it is the one sanctioned attack and not a motion.
        // THE ATTACK: from dark through `edge-in` at birth — or, for a cell
        // RE-ARMED from a level (`Cell::rearm`, M1), from exactly that level
        // at the re-arm's instant up to full through the same `edge-in`.
        let attack = match cell.rearm {
            Some((re_at, level)) => {
                let r = attack_now.saturating_duration_since(re_at).as_secs_f32();
                level + (1.0 - level) * edge_in(r)
            }
            None => BIRTH_EDGE_FLOOR + (1.0 - BIRTH_EDGE_FLOOR) * edge_in(age),
        };
        let mut env = attack
            * if reduced {
                reduced_fade(cell.life_s - spent)
            } else {
                expiry_melt(u)
            };
        if let Some(at) = cell.retract_at {
            let r = now.saturating_duration_since(at).as_secs_f32();
            env *= if reduced {
                reduced_fade(RETRACT_FADE_S - r)
            } else {
                spend(clamp01(r / RETRACT_FADE_S))
            };
        }
        if let Some(at) = cell.retire_at {
            // THE CONTENT RETIREMENT (`Ribbon::retire_cells`): the retract's
            // own `spend` on the short `RETIRE_MELT_S` span — exactly 1.0 at
            // the stamp, exactly 0 at its end, monotone between.
            let r = now.saturating_duration_since(at).as_secs_f32();
            env *= if reduced {
                reduced_fade(RETIRE_MELT_S - r)
            } else {
                spend(clamp01(r / RETIRE_MELT_S))
            };
        }
        if let Some(at) = self.curtain {
            // THE CURTAIN: the retract's own `spend`, and the retract's own
            // FARTHEST-FIRST order — the cell furthest from the hand in
            // logical order ([`Cohort::drain_rank`], the same frozen
            // [`Cohort::drain_right_first`] the natural drain reads) starts
            // spending at the curtain's instant, the cell under the hand at
            // [`CURTAIN_STAGGER_S`], and each takes [`CURTAIN_SPEND_S`] to
            // reach exactly zero — so the last of them is exactly zero at
            // `CURTAIN_S` and the band gathers INTO the hand instead of
            // dropping whole. A curtain on ONE clock is what made the count
            // fall 19 → 0 in a single frame on glass; see
            // [`CURTAIN_STAGGER_S`].
            let r = now.saturating_duration_since(at).as_secs_f32();
            env *= if reduced {
                reduced_fade(CURTAIN_S - r)
            } else {
                let rank = coh.map_or(1.0, |c| c.drain_rank(cell.col));
                let t0 = CURTAIN_STAGGER_S * (1.0 - rank);
                spend(clamp01((r - t0) / CURTAIN_SPEND_S))
            };
        }
        if let Some(coh) = coh
            && coh.phase.is_retracting()
        {
            let idle = now.saturating_duration_since(coh.alive_at).as_secs_f32();
            if reduced {
                env *= reduced_fade(coh.swoosh_total() - idle);
            } else {
                // FARTHEST-FIRST, on the retract's own schedule: the cell
                // FURTHEST FROM THE HAND in logical order starts spending
                // first ([`Cohort::drain_rank`] — the left end on the
                // caret's row and on a row above it, the right end on a row
                // below) and each takes `RETRACT_FADE_S` to reach exactly
                // zero, so the mark is drawn back INTO the hand. Draining
                // head-first would be the same arithmetic reading the mark
                // backwards, and it looks like the ribbon abandoning the
                // caret.
                let t0 = coh.retract_start() + RETRACT_DUR_S * (1.0 - coh.drain_rank(cell.col));
                env *= spend(clamp01((idle - t0) / RETRACT_FADE_S));
            }
        }
        // THE FOCUS EMBER AND ITS REARM RUN LAST (2026-09-14), because the
        // ember's floor is priced against the light the cell ACTUALLY has —
        // everything above, the melt and the retract and the swoosh's own
        // drain included. Priced against the birth ceiling instead, a cell
        // that had already melted a little would reach its colour floor
        // before its neighbours, which is the very thing the floor exists to
        // forbid.
        if let Some(at) = self.ember_at {
            // THE EMBER, PER STOP: the fall is read into this cell's OWN
            // range by [`ember_level`] and the go-out is one common
            // multiplier over every stop at its own floor, so no stop can
            // cross the colour floor before its neighbours. The band leaves
            // as one object.
            let r = now.saturating_duration_since(at).as_secs_f32();
            let f = if reduced {
                reduced_fade(FOCUS_EMBER_S - r)
            } else {
                spend(clamp01(r / FOCUS_EMBER_S))
            };
            env *= ember_level(self.ember_floor(ctx, cell, env), f);
        }
        if let Some((at, fade)) = self.rearm {
            // Focus regained mid-ember: back up from the level it had, on the
            // one sanctioned attack, so the return is continuous. The stamp
            // carries the ember's own FADE at the regain, not one level: the
            // ember's level is per stop, so each cell rearms from exactly
            // where it was (a single shared number would step every cell but
            // one, up or down, on the regain frame).
            let level = ember_level(self.ember_floor(ctx, cell, env), fade);
            let r = attack_now.saturating_duration_since(at).as_secs_f32();
            env *= level + (1.0 - level) * edge_in(r);
        }
        env
    }

    /// **THE CELL'S COVERAGE** — the flat body (head floor 1.0, crest gain
    /// 0.0) at its birth-priced ceiling ([`Cell::cov0`]), closed by
    /// [`Ribbon::env_of`], and scaled by the host's intensity BEFORE the cap
    /// (§3.4: requests are clipped by the ledger, never by ad-hoc caps — this
    /// is the bed's own published ceiling, and the only one this producer
    /// applies).
    fn cov_of(&self, ctx: &Ctx<'_>, cell: &Cell) -> f32 {
        let cap = cov_cap(ctx.cfg);
        (cap * cell.cov0 * self.env_of(ctx, cell) * clamp01(ctx.cfg.intensity)).clamp(0.0, cap)
    }

    /// **THE EMBER'S FLOOR FOR THIS CELL**, as a share of the ember's own
    /// factor: the alpha byte at which its stop stops being seen over the
    /// live theme's ground ([`BedInkLut::seen_floor_at`], with
    /// [`EMBER_FLOOR_MARGIN`]) divided by the coverage the cell has with
    /// every OTHER envelope factor already spent (`carried`). `0` — no floor,
    /// today's curve — for a cell with no coverage to spend, and for an
    /// unsynced ink table.
    fn ember_floor(&self, ctx: &Ctx<'_>, cell: &Cell, carried: f32) -> f32 {
        self.ember_floor_of(
            cov_cap(ctx.cfg) * cell.cov0 * clamp01(ctx.cfg.intensity) * carried,
            cell.t,
        )
    }

    /// [`Ribbon::ember_floor`] against a coverage already priced — the form
    /// the cadence fold reads, which has a peak and a walk position and no
    /// [`Config`].
    fn ember_floor_of(&self, full_cov: f32, t: f32) -> f32 {
        if !full_cov.is_finite() || full_cov <= 0.0 {
            return 0.0;
        }
        // NOT clamped to 1: a floor over the cell's own full coverage means
        // "hold full through the dim", and [`ember_level`]'s clamp says so —
        // clamping the floor instead would take that cell out of the common
        // go-out's units and it would leave on its own.
        (self.ink.seen_floor_at(t) * EMBER_FLOOR_MARGIN / full_cov).max(0.0)
    }

    /// **THE FROM-THE-HAND ATTACK** (2026-09-13, `RAINBOW-KITTY-V2.md` §30)
    /// — the multiplier on a YOUNG cell's coverage at `dist` across the cell
    /// (`0.0` at the cell's caret side, `1.0` at its far side), so that its
    /// light enters as a wipe from the hand instead of fading in in place.
    ///
    /// The cell's level already carries the uniform attack
    /// (`BIRTH_EDGE_FLOOR + (1 − floor) · edge_in(age)`, [`Ribbon::env_of`]);
    /// this returns the ratio of the WIPED attack — the same ramp gated by a
    /// smoothstep front that crosses the cell from the caret side over
    /// [`ATTACK_WIPE_S`] ([`ATTACK_WIPE_FRONT`] wide) — to the uniform one,
    /// and [`Ribbon::plan_run`] multiplies it into the slab. The floor is
    /// never gated: on the echo frame the whole cell is at
    /// [`BIRTH_EDGE_FLOOR`] (T2, the frame that acknowledges the key already
    /// shows the colour), and only the ramp above it pours in from the
    /// caret. Exactly `1.0` — the same bytes as before — for a cell past the
    /// wipe, for the flat spelling, under reduced motion (the wipe is a
    /// motion; the edge-in is not, §6.11) and for a wake cell not yet born.
    fn wipe_of(&self, ctx: &Ctx<'_>, cell: &Cell, dist: f32) -> f32 {
        // A TYPED cell's attack only: a wake's cells are all born at one
        // instant (the landing plus `WAKE_BORN_LAG_S`), so wiping each from
        // its own caret-side edge printed a comb at cell pitch instead of
        // light leaving the hand — its lay sweep IS its from-the-landing
        // attack (review, 2026-09-13).
        // The wipe runs on the cell's VISUAL clock (`attack_at`, the age the
        // edge-in reads), not its identity (`born`, which a re-lay refreshes
        // for the content witness): a re-wet cell keeps its wipe as it keeps
        // its edge-in (integration, 2026-09-13).
        if !cell.typing
            || !Self::comet(ctx.cfg)
            || ctx.cfg.reduced_motion
            || ctx.now < cell.attack_at
        {
            return 1.0;
        }
        let age = ctx
            .now
            .saturating_duration_since(cell.attack_at)
            .as_secs_f32();
        if age >= ATTACK_WIPE_S {
            return 1.0;
        }
        let ramp = edge_in(age);
        if ramp <= 0.0 {
            return 1.0;
        }
        let front = clamp01(age / ATTACK_WIPE_S) * (1.0 + ATTACK_WIPE_FRONT);
        let gate = smoothstep01((front - clamp01(dist)) / ATTACK_WIPE_FRONT);
        let uniform = BIRTH_EDGE_FLOOR + (1.0 - BIRTH_EDGE_FLOOR) * ramp;
        let wiped = BIRTH_EDGE_FLOOR + (1.0 - BIRTH_EDGE_FLOOR) * ramp * gate;
        wiped / uniform
    }

    /// The run's own retract: every boundary of a RETRACTING cohort is pulled
    /// toward the caret on `suck-in`, so the mark shortens INTO the hand (T5)
    /// rather than fading in place, and at `u = 1` every boundary is the
    /// caret's own x — a run of exactly zero length, which draws exactly
    /// nothing. The caret it pulls toward is the one the retract BEGAN
    /// under ([`Cohort::retract_col`]), so a jump landing mid-retract does
    /// not lurch the leaving mark across the row. Per COHORT, never per row:
    /// a cohort still being typed beside one that is swooshing keeps every
    /// boundary where its letters are. Under reduced motion nothing moves
    /// (§6.11).
    fn retract_x(&self, ctx: &Ctx<'_>, cohort: u32, x: f32) -> f32 {
        if ctx.cfg.reduced_motion {
            return x;
        }
        // THE CURTAIN draws every boundary into the column its own cohort
        // was frozen toward when the curtain fell ([`Ribbon::curtain`] —
        // per COHORT, exactly as a natural retract, and derived from the
        // band's own head rather than from a caret the seam has moved), on
        // the same `suck-in` the retract uses and toward the same edge of
        // the same block, whatever the cohort's phase: the eye reads the one
        // exit it already knows. Reading [`Ribbon::caret`] here is wrong
        // twice over — the engine's caret mirror is forgotten with the space
        // that ended, `plan` re-stamps it from the NEW space every frame,
        // and at the alt-screen seam the Return that launched the program
        // has already taken it to column 0 of the next line.
        if let Some(at) = self.curtain {
            let u = clamp01(ctx.now.saturating_duration_since(at).as_secs_f32() / CURTAIN_S);
            let target = self
                .cohorts
                .iter()
                .find(|c| c.id == cohort)
                .and_then(|c| c.retract_col);
            let Some(col) = target else {
                return x;
            };
            let caret_x = f32::from(ctx.geom.origin_x) + f32::from(col) * ctx.geom.cw as f32;
            return caret_x + (x - caret_x) * (1.0 - suck_in(u));
        }
        let Some(coh) = self
            .cohorts
            .iter()
            .find(|c| c.id == cohort && c.phase.is_retracting())
        else {
            return x;
        };
        let idle = ctx
            .now
            .saturating_duration_since(coh.alive_at)
            .as_secs_f32();
        let u = clamp01((idle - coh.retract_start()) / (RETRACT_DUR_S + RETRACT_FADE_S));
        let target = coh.retract_col.or_else(|| self.caret.map(|(_, col)| col));
        // The target is the caret cell's LEFT EDGE (2026-09-13), not its
        // centre: the mark shortens into the hand and stops at the block,
        // where a centre target crept up to half a cell INTO the caret cell
        // on quiet ticks — fresh columns lighting with no key behind them.
        let caret_x = target.map_or(x, |col| {
            f32::from(ctx.geom.origin_x) + f32::from(col) * ctx.geom.cw as f32
        });
        caret_x + (x - caret_x) * (1.0 - suck_in(u))
    }

    /// **THE HEAD OF A RUN** (§4.1 "a property of the head's position", §20.1
    /// "ends at the head cell every frame"): `(head column, at caret, wet)`.
    ///
    /// The head is the cell under the hand — the caret's own cell (`caret −
    /// 1`) when the caret stands in or just past the run and that cell is
    /// live, un-retracting light, whatever was typed and erased to its right.
    /// Otherwise it is the run's newest cell that is not retracting. A run
    /// with no such cell (the whole word backspaced, every cell draining) has
    /// its head at its right end and is NOT wet: nothing at the hand is still
    /// being laid, so nothing there is still wet, and the hot edge stays off
    /// it. Resolving the head from the highest `t` parked the hot edge on the
    /// fading erased cells for up to half a second after a backspace and then
    /// jumped it left.
    ///
    /// **THE HEAD OF A LEFTWARD WAKE IS ITS LANDING** (2026-09-10, the
    /// stream round). When the caret stands ON the run's FIRST cell and that
    /// run is a WAKE cohort ([`Cohort::wake`]) — a same-row jump to a lower
    /// column laid the corridor it crossed and the landing cell is under the
    /// block — the landing is the head and the stream runs toward HIGHER
    /// columns (`stream_dir` `+1`). Before this clause the own-cell branch
    /// failed at the landing (`caret − 1` is not laid, or underflows at col
    /// 0) and the run fell to `newest`: [`Ribbon::wake`] lays every new cell
    /// with ONE `born_new`, the taken-over typed cells keep their older
    /// births, and `max_by` returns the LAST equal maximum of a
    /// column-ascending run — the HIGHEST-column new cell, the FAR end of
    /// the corridor — so after every Ctrl-A the standing hot edge came up
    /// where the caret LEFT, up to 32 cells from the hand, 100 ms after the
    /// jump. `wake()`'s own comment stated the intent the code missed. The
    /// typed-cohort rule ([`Run::head`]: "the head is always the run's
    /// right-hand side") is not touched: a typed cohort is `wake == false`,
    /// so a Backspace can never reach this clause, and a rightward wake —
    /// the caret on its LAST cell — keeps the own-cell head.
    ///
    /// `at caret` is the emit-order key (the run the hand is on goes first)
    /// and is true whenever the caret is in or beside the run, wet or not.
    /// The fourth value is the run's `stream_dir`: `+1` from the wake clause,
    /// `−1` from every other branch.
    fn head_col(&self, ctx: &Ctx<'_>, run: &[(u16, u16, u32)]) -> (u16, bool, bool, i8) {
        let row = run[0].0;
        let (col0, col1) = (run[0].1, run[run.len() - 1].1);
        let (crow, ccol) = ctx.caret;
        let at_caret = crow == row && (col0..=col1 + 1).contains(&ccol);
        let live = |col: u16| -> bool {
            run.get(usize::from(col - col0))
                .and_then(|e| self.cells.get(e.2 as usize))
                .is_some_and(|c| c.col == col && !c.leaving())
        };
        if at_caret
            && let Some(own) = ccol.checked_sub(1)
            && (col0..=col1).contains(&own)
            && live(own)
        {
            return (own, true, true, -1);
        }
        if at_caret
            && ccol == col0
            && live(col0)
            && self
                .cells
                .get(run[0].2 as usize)
                .and_then(|c| self.cohorts.iter().find(|k| k.id == c.cohort))
                .is_some_and(|k| k.wake)
        {
            return (col0, true, true, 1);
        }
        let newest = run
            .iter()
            .filter_map(|e| self.cells.get(e.2 as usize))
            .filter(|c| !c.leaving())
            .max_by(|a, b| a.born.cmp(&b.born))
            .map(|c| c.col);
        match newest {
            Some(col) => (col, at_caret, true, -1),
            None => (col1, at_caret, false, -1),
        }
    }

    /// Plan ONE contiguous run into [`Ribbon::plan`], boundary by boundary,
    /// with [`Ribbon::slabs_per_cell`] vertices per cell.
    ///
    /// `seam` is the last vertex of the run abutting this one on its left,
    /// if any, and `right_nb` the owner cell abutting it on its right (another
    /// cohort's — see [`Ribbon::build_runs`]): the run's first boundary IS the
    /// seam vertex (shape and colour bit for bit, its light the older side's
    /// under the tail-ease law below), and its last boundary prices the
    /// right neighbour's light into the same law, so two abutting runs meet
    /// at one vertex instead of a feather.
    ///
    /// **THE ATTACH (2026-09-14, the owner's notch).** A typed run whose head
    /// stands under a live caret — the caret one column right of the head
    /// cell, the head the run's last cell, nothing laid under the caret —
    /// continues ONE CELL past its head boundary with the head boundary's own
    /// sample, so the body, the strip and the rail run on under the caret
    /// cell and the block stands ON the band: the leading under the block
    /// and the crown above it carry the head's light instead of the ground.
    /// Before this the band ended at the block's left edge — its rail 22
    /// device rows deep at the owner's cell, its crown one row above the
    /// cell top — and the ground showed as a black foot under the block and
    /// a black step beside it: "a right angle of black from the cursor to
    /// the underline". The extension is the head's light and nothing else
    /// (no key, no clock of its own): it is priced from the head boundary,
    /// pulled by the head's retract, and it is gone with the head.
    ///
    /// **…AND IT SURVIVES AN ERASE** (review of the attach, 2026-09-14).
    /// The attach's law is "the caret cell carries no LIVE light": a cell
    /// under the caret that is LEAVING — the erased cell a Backspace left
    /// the caret over, draining on `retract_at`; a cell the content witness
    /// retired under the caret — does not own the caret cell either. Its
    /// slabs are planned as before (its own shape, its own colour, its own
    /// spend), FLOORED at the attach's level ([`AttachFloor`]): the block
    /// stands on the band while the cell spends, and the frame the cell
    /// retires and the attach's own segments take its place changes no
    /// byte. With the first cut's predicate (`head == col1 && right_nb is
    /// none`) the erased cell's spend took the caret cell from 215 to 0
    /// under the block over 224 ms, then its retirement switched the attach
    /// on at full in ONE frame — a 107-level rise at the peak, 214 at the
    /// darkest column — on every Backspace. The floor is applied inside the
    /// boundary loop, never as extra segments: two polylines co-owning the
    /// caret cell's x would composite the source-over bed twice.
    fn plan_run(
        &mut self,
        ctx: &Ctx<'_>,
        run: &[(u16, u16, u32)],
        seam: Option<Segment>,
        right_nb: Option<Cell>,
    ) {
        let row = run[0].0;
        let (col0, col1) = (run[0].1, run[run.len() - 1].1);
        let Some(cohort) = self.cells.get(run[0].2 as usize).map(|c| c.cohort) else {
            return;
        };
        let slabs = self.slabs_per_cell();
        let (head_col, at_caret, wet, stream_dir) = self.head_col(ctx, run);
        let born = run
            .iter()
            .filter_map(|e| self.cells.get(e.2 as usize))
            .filter(|c| c.typing)
            .map(|c| c.born)
            .max()
            .unwrap_or(ctx.now);
        let span = f32::from(col1 - col0).max(1.0);
        // The run is contiguous and column-sorted, so a cell is an INDEX, not
        // a search: v1's per-boundary scan is exactly the shape of work §18
        // deletes, and it is the same shape twice per boundary here.
        let cell_at = |col: u16| -> Option<&Cell> {
            let i = usize::from(col.checked_sub(col0)?);
            let e = run.get(i)?;
            (e.1 == col).then(|| self.cells.get(e.2 as usize))?
        };
        let cw = ctx.geom.cw as f32;
        // THE ATTACH's condition (see the doc above): a typed run, wet at
        // the hand, the caret one column right of the head and inside the
        // pane, and no LIVE light under the caret — nothing laid there, or
        // a cell that is leaving (its own run's, or the neighbour's).
        let pane_end = self.pane.map_or(
            u16::try_from(ctx.geom.cols).unwrap_or(u16::MAX),
            |(c0, w)| c0.saturating_add(w),
        );
        let caret_cell = head_col.wrapping_add(1);
        let under_caret_leaving = match cell_at(caret_cell) {
            Some(c) => c.leaving(),
            None => right_nb.as_ref().is_some_and(Cell::leaving),
        };
        let caret_free = cell_at(caret_cell).is_none() && right_nb.is_none() || under_caret_leaving;
        let attach = stream_dir < 0
            && wet
            && at_caret
            && ctx.caret.1 == caret_cell
            && ctx.caret.1 < pane_end
            && caret_free;
        // THE ATTACH'S OWN ATTACK: the birth floor and the 18 ms edge-in
        // (T2, T3 — the level's own ramp), and the from-the-hand wipe
        // over `ATTACK_WIPE_S` from the head's edge outward, all on the
        // head cell's BIRTH clock (the key that put the caret here) —
        // whatever attack the head cell itself inherited. `wipe_of` is
        // a RATIO to the uniform attack, so the uniform attack is put
        // back here: the head's light is already at full.
        let attack = attach
            .then(|| cell_at(head_col).copied())
            .flatten()
            .map(|head| {
                let probe = Cell {
                    attack_at: head.born,
                    ..head
                };
                let age = ctx.now.saturating_duration_since(head.born).as_secs_f32();
                (
                    BIRTH_EDGE_FLOOR + (1.0 - BIRTH_EDGE_FLOOR) * edge_in(age),
                    probe,
                )
            });
        if attack.is_some() {
            self.attach = Some((row, caret_cell));
        }
        let lo = self.plan.len();
        let mut prev: Option<(f32, Sample)> = None;
        // The boundary (index from `col0`) a hard edge doubled, if any —
        // see THE BLOCK'S RIGHT EDGE below; the head index accounts for it.
        let mut doubled: Option<usize> = None;
        for boundary in u32::from(col0)..=u32::from(col1) + 1 {
            let left = boundary
                .checked_sub(1)
                .and_then(|c| u16::try_from(c).ok())
                .and_then(cell_at);
            let right = u16::try_from(boundary).ok().and_then(cell_at);
            let (a, b) = match (left, right) {
                (Some(a), Some(b)) => (a, b),
                (Some(a), None) => (a, a),
                (None, Some(b)) => (b, b),
                (None, None) => continue,
            };
            let sp = |c: &Cell| f32::from(head_col.abs_diff(c.col)) / span;
            let sa = self.sample(ctx, a, sp(a));
            let sb = self.sample(ctx, b, sp(b));
            // THE WIPE'S COORDINATE (2026-09-13): `dist` runs from the cell's
            // CARET side to its far side. The head is the run's right end
            // for a typed run (`stream_dir` −1), so a cell's caret side is
            // its right edge; a leftward wake's is its left edge.
            let caret_side_right = stream_dir < 0;
            // The boundary is `a`'s right edge and `b`'s left edge.
            let wa = self.wipe_of(ctx, a, if caret_side_right { 0.0 } else { 1.0 });
            let wb = self.wipe_of(ctx, b, if caret_side_right { 1.0 } else { 0.0 });
            let mid = |u: f32, v: f32| (u + v) * 0.5;
            // THE BRIGHTER SIDE OWNS THE BOUNDARY (2026-09-13). Shape and
            // colour are the midpoint of the two cells; COVERAGE is the
            // brighter cell's, and the dimmer cell's own level is reached
            // inside the dimmer cell (its first slab). A midpoint coverage
            // sagged the settled cell's outer third to half while its erased
            // neighbour spent, then POPPED it back when the neighbour
            // retired (+74..+100 levels in one tick, ~240 ms after the last
            // Backspace), and dimmed the previous glyph's cell by a third for
            // one frame on every key while the new cell's edge-in ran.
            //
            // …AND THE YOUNGER SIDE ENTERS AT THE TAIL EASE (2026-09-14).
            // The LEFT side is the older light on every run — the tail of a
            // typed run, and, for want of anything the caret cannot move,
            // of a wake too (see the note below the seam branches) — and the
            // right side feathers in at [`RUN_TAIL_EASE`], the boundary
            // taking the brighter of the two. An absent side gives no light, so the
            // run's outer tail boundary is the one-cell feather it always
            // was; a live neighbour at full light — another run's cell —
            // owns the seam and no feather shows; a neighbour that is
            // LEAVING hands the feather over exactly as fast as its light
            // goes, so the frame that retires it changes no byte. The flat
            // tenth at `left.is_none()` printed a dark column and a step
            // inside a continuous band wherever two live runs met, and a
            // pop the frame a drained neighbour retired.
            //
            // At a SEAM with the run on the left, this run's first vertex
            // is that run's last — the same spine, reach and stop, so the
            // two polylines meet at one point whatever their own heads and
            // waves say — and its light is what that vertex already carries
            // (which priced this run's first cell in, below). At a seam with
            // a run on the right, the neighbour's light enters this run's
            // last boundary the same way.
            let mut la = left.map_or(0.0, |_| sa.3 * wa);
            let mut lb = right.map_or(0.0, |_| sb.3 * wb);
            let mut shape = (
                mid(sa.0, sb.0),
                mid(sa.1, sb.1),
                mid(sa.2, sb.2),
                mid(a.t, b.t),
            );
            if left.is_none()
                && let Some(seam) = seam
            {
                la = f32::from(seam.cov);
                shape = (seam.spine, seam.up, seam.dn, seam.t);
            }
            if right.is_none()
                && let Some(nb) = right_nb.as_ref()
            {
                // THE NEIGHBOUR'S OWN EDGE (2026-09-14, review): this
                // boundary is `nb`'s LEFT edge by construction, whatever
                // way THIS run's light streams — `wipe_of`'s `dist` runs
                // from a cell's caret side, and the neighbour's is its own
                // run's business. A by-construction correction: the term
                // only enters through the ease, and `wipe_of` is 1.0 for
                // any cell that is not `typing` or is older than
                // `ATTACK_WIPE_S`, so it is reachable only where a leftward
                // run abuts a neighbour's freshly typed cell inside 40 ms.
                // `caret_side_right` below still picks the wipe's edge for
                // this run's OWN cells, so the boundary's light is not yet
                // free of the caret — only its choice of soft end is.
                lb = self.cov_of(ctx, nb) * self.wipe_of(ctx, nb, 1.0);
            }
            // …AND IT IS THE SAME EXPRESSION ON EVERY RUN (2026-09-14, at
            // the merge with the scrub round). The left side owns the
            // boundary and the right enters at the ease, whichever way this
            // run's light streams: `la` is the neighbour's at a run's first
            // boundary (the seam, or nothing — which is the one-cell
            // feather the oldest end has always opened with) and the run's
            // OWN at its last (where the head, the attach and the block
            // stand, whole). Mirroring the expression for a leftward wake
            // put the choice in `head_col`, which resolves the head — and
            // so the stream — from the CARET, every frame: a caret
            // scrubbing rightward into a leftward corridor's own cells (a
            // Ctrl-A's holes, then two Rights) took the own-cell head
            // instead of the wake's landing, the stream flipped, and the
            // corridor's far boundary went 23 → 233 in ONE frame — a
            // 210-level rise inside a settled band, over a drained cell
            // with no key behind it. Which end of a mark is soft is not
            // something the hand may change by moving.
            let natural = la.max(lb * RUN_TAIL_EASE);
            // THE BLOCK'S RIGHT EDGE IS A HARD EDGE. At a leaving caret
            // cell's far edge the vertex is floored (`AttachFloor`), and
            // when the band goes on past it — the next erased cell of a
            // held Backspace, a neighbour run — the SAME x carries a second
            // vertex at the boundary's natural light, so the cell to the
            // right lerps from its own seam and not from the floor:
            // `ribbon_beam` prices a slab once at its centre, so a slab
            // running from the floored vertex to a nearly-spent neighbour
            // read half the floor (97 levels in the 10 px right of the
            // block) and vanished in one frame when that neighbour retired.
            // The zero-length segment between the two draws nothing (the
            // beam skips a span under 1e-3), and the attach's own segments
            // end at the block's right edge the same way.
            let floored = left
                .and_then(|l| self.attach_floor_at(ctx, row, l, 1.0))
                .map(|floor| natural.max(floor));
            let cov = floored.unwrap_or(natural);
            let here = (shape.0, shape.1, shape.2, shape.3, cov);
            // THE ATTACH'S LEVEL is the head boundary's: from here on the
            // caret cell's slabs — this run's or the neighbour run's — and
            // the attach's own segments read it.
            if let Some((uniform, probe)) = attack
                && boundary == u32::from(caret_cell)
            {
                self.attach_floor = Some(AttachFloor {
                    row,
                    col: caret_cell,
                    base: cov,
                    uniform,
                    probe,
                });
            }
            let x = f32::from(ctx.geom.origin_x) + boundary as f32 * cw;
            if let Some((px, p)) = prev {
                // The cell between `prev` and `here` is `a`: its interior
                // never exceeds its own coverage, so a transition between
                // two levels lives inside the dimmer cell — and inside a
                // young cell that level is the WIPE's at the slab (its far
                // side still at the floor while its caret side has opened).
                for j in 1..slabs {
                    let f = j as f32 / slabs as f32;
                    let l = |u: f32, v: f32| u + (v - u) * f;
                    let own =
                        sa.3 * self.wipe_of(ctx, a, if caret_side_right { 1.0 - f } else { f });
                    let mut cov = l(p.4, here.4).min(own);
                    // THE FLOOR inside a leaving caret cell (`AttachFloor`):
                    // `f` runs from the head's edge, the attach's own wipe
                    // coordinate.
                    if let Some(floor) = self.attach_floor_at(ctx, row, a, f) {
                        cov = cov.max(floor);
                    }
                    self.plan.push(Segment {
                        // ROUNDED TO THE PIXEL LATTICE: `ribbon_beam` tiles
                        // half-open at `ceil`, and a fractional interior vertex
                        // made the two segments either side of it co-own a
                        // column — which a source-over bed composites twice.
                        x: self.retract_x(ctx, cohort, l(px, x)).round(),
                        spine: l(p.0, here.0),
                        up: l(p.1, here.1),
                        dn: l(p.2, here.2),
                        t: l(p.3, here.3),
                        cov: cov.clamp(0.0, 255.0) as u8,
                    });
                }
            }
            self.plan.push(Segment {
                x: self.retract_x(ctx, cohort, x).round(),
                spine: here.0,
                up: here.1,
                dn: here.2,
                t: here.3,
                cov: here.4.clamp(0.0, 255.0) as u8,
            });
            let continues =
                right.is_some() || (boundary == u32::from(col1) + 1 && right_nb.is_some());
            let onward = if floored.is_some() && continues {
                doubled = Some((boundary - u32::from(col0)) as usize);
                let seam = (shape.0, shape.1, shape.2, shape.3, natural);
                self.plan.push(Segment {
                    x: self.retract_x(ctx, cohort, x).round(),
                    spine: seam.0,
                    up: seam.1,
                    dn: seam.2,
                    t: seam.3,
                    cov: seam.4.clamp(0.0, 255.0) as u8,
                });
                seam
            } else {
                here
            };
            prev = Some((x, onward));
        }
        // THE ATTACH'S OWN SEGMENTS: one cell on under a live caret, the
        // head boundary's own sample at every slab (see the doc above) —
        // only where NOTHING is laid under the caret. A leaving cell there
        // keeps its own slabs, floored above.
        if attach
            && !under_caret_leaving
            && let Some((px, p)) = prev
            && let Some(af) = self.attach_floor
        {
            let x = px + cw;
            for j in 1..=slabs {
                let f = j as f32 / slabs as f32;
                let cov = af.level(self, ctx, f);
                self.plan.push(Segment {
                    x: self.retract_x(ctx, cohort, px + (x - px) * f).round(),
                    spine: p.0,
                    up: p.1,
                    dn: p.2,
                    t: p.3,
                    cov: cov.clamp(0.0, 255.0) as u8,
                });
            }
        }
        let hi = self.plan.len();
        if hi > lo {
            // The head BOUNDARY: the head cell's RIGHT edge. Boundary `b` of
            // the run sits at `lo + (b − col0) · slabs`. A LEFTWARD wake's
            // head is the landing's LEFT edge — the run's first boundary —
            // and its stream runs the other way (`Run::stream_dir`).
            let head_boundary = usize::from(head_col - col0) + 1;
            // A hard edge doubled before the head shifts every later
            // vertex by one (the head itself is live light, never the
            // doubled boundary).
            let shift = usize::from(doubled.is_some_and(|d| d < head_boundary));
            let head = if stream_dir > 0 {
                lo
            } else {
                (lo + head_boundary * slabs + shift).min(hi - 1)
            };
            self.runs.push(Run {
                row,
                col0,
                col1,
                lo,
                hi,
                head_col,
                head,
                wet,
                at_caret,
                born,
                stream_dir,
            });
        }
    }

    /// The attach's floor under `cell` at `dist` across it (`0` at the
    /// head's edge), or `None`: the floor applies to a LEAVING cell at the
    /// caret cell of this frame's attach ([`AttachFloor`]) and to nothing
    /// else — a live cell owns its light, and a leaving cell anywhere else
    /// drains as it always did.
    fn attach_floor_at(&self, ctx: &Ctx<'_>, row: u16, cell: &Cell, dist: f32) -> Option<f32> {
        let af = self.attach_floor?;
        (af.row == row && af.col == cell.col && cell.leaving()).then(|| af.level(self, ctx, dist))
    }

    // -- emit --------------------------------------------------------------

    /// Draw the ribbon body into `frame.under` and the hot edge into
    /// `frame.out`.
    ///
    /// Called THIRD in the emit order — after the meteor (§6.5's bolts-first
    /// law) and before stardust — so a truncation sheds sky before it sheds
    /// the laid rainbow.
    ///
    /// ONE `aterm_render::ribbon_beam` CALL PER RUN for the body, with the
    /// shoulder taken from [`BodyProfile`] and `GlowBlend::Over` on both
    /// grounds: the bed is the one stream in the family that composites
    /// SOURCE-OVER, which is why its ceiling is a luminance rather than an
    /// additive budget, and why the light theme's fork is a change of INK
    /// and of nothing else (§3.3, L6: never additive light on white).
    ///
    /// **AND A SECOND CALL PER RUN FOR THE VIVID RAIL** (2026-09-13, §30,
    /// dark themes, [`Ribbon::rail_lit`]): the same slabs, the same `x`, a
    /// polyline whose top is the row bottom — `max(spine, row bottom)`, so
    /// the wave's crest never lifts it into the typed row's descenders —
    /// and whose reach is the body's own `dn` below it, in [`rail_ink`] at
    /// the body's coverage ([`RAIL_GAIN`]), Over-composited on the bed. It
    /// needs no renderer change: `up` is `0.0`, so `ribbon_profile` is
    /// exactly zero above the rail's top, and the row that holds the top
    /// gets the same sub-pixel edge the body's edges get. It rides the same
    /// budget as the body, after the body of its run — a saturated frame
    /// sheds the oldest run's rail before that run's body.
    pub fn emit(&mut self, ctx: &Ctx<'_>, frame: &mut Frame<'_>) {
        self.ink_quads = 0;
        if self.plan.is_empty() || self.runs.is_empty() {
            return;
        }
        let ink_from = frame.under.len();
        let shoulder = Self::body_profile(ctx.cfg).shoulder;
        let rail = Self::rail_lit(ctx.cfg);
        let chf = ctx.geom.ch as f32;
        let ceiling = if ctx.cfg.dark_theme {
            BODY_FRAME_TOP
        } else {
            light_role(ctx.cfg).alpha_cap()
        };
        let clip = ctx.geom.beam_clip();
        let budget = frame.under.len() + RIBBON_QUAD_BUDGET;
        let core_up = Self::body_profile(ctx.cfg).core_up_ch * ctx.geom.ch as f32;
        let lift_span = LEAD_CH * ctx.geom.ch as f32;
        let mut verts = mem::take(&mut self.verts);
        let runs = mem::take(&mut self.runs);
        // ONE SLAB PER VERTEX PAIR: the plan already put a vertex on every
        // slab boundary (`slabs_per_cell`), so the stride is the slab's own
        // width and the density the budget chose is the density that prints.
        let stride = ctx.geom.cw.max(1).div_ceil(self.slabs_per_cell());
        // `runs` is in EMIT ORDER — the run at the caret, then newest first —
        // so when the budget runs out it is the OLDEST row's far end that is
        // shed (see `build`).
        for run in &runs {
            verts.clear();
            verts.reserve(run.hi - run.lo);
            for seg in &self.plan[run.lo..run.hi] {
                // THE STRIP IS PRICED OUT OF THE SAME CEILING THE BODY IS,
                // never beside it: `cov + lift` is what the spine actually
                // composites, so a strip clamped against the byte instead of
                // against the frame top is a second, higher ceiling — which is
                // exactly the "two ceilings for one bed" §19.1 deletes.
                let strip = (f32::from(seg.cov) * STRIP_LIFT_GAIN)
                    .min(ceiling - f32::from(seg.cov))
                    .max(0.0);
                verts.push(RibbonVertex {
                    x: seg.x,
                    spine: seg.spine,
                    up: seg.up,
                    dn: seg.dn,
                    core_up,
                    core_dn: 0.0,
                    color: self.ink.at(seg.t),
                    cov: f32::from(seg.cov),
                    lift: strip,
                    lift_span,
                });
            }
            // HEAD FIRST: `ribbon_beam` walks the polyline in order and stops
            // at the budget, so starting from the run's RIGHT end — the head's
            // side, always (see `Run::head`) — sheds the left, older light
            // when it runs out.
            verts.reverse();
            if !ribbon_beam(
                frame.under,
                clip,
                &verts,
                shoulder,
                stride,
                budget,
                GlowBlend::Over,
            ) {
                break;
            }
            if !rail {
                continue;
            }
            // THE VIVID RAIL: the ink-free reach below the row bottom, in the
            // full-value spectrum. Its top IS the row bottom — the spine
            // carries the wave, and a crest would otherwise put vivid ink
            // under the typed row's descenders — and its bottom is the
            // body's own, floored at `DN_FLOOR_CH` (the wave's trough lifts
            // the body's bottom edge by up to the amplitude, and a rail that
            // followed it thinned to a sliver at the tail), so the rail is
            // never thinner than the leading the flat body always reached
            // and is thickest under the hand (the comet's lower lobe). The
            // grid's last row has no leading below it and draws no rail
            // (see `sample`: it keeps the flat wedge).
            let row_bottom = f32::from(ctx.geom.origin_y) + (f32::from(run.row) + 1.0) * chf;
            if row_bottom >= ctx.geom.fx_bot() as f32 {
                continue;
            }
            verts.clear();
            verts.reserve(run.hi - run.lo);
            for seg in &self.plan[run.lo..run.hi] {
                // The top follows a crest DOWN (the accent's upper half is
                // never covered) and never rises above the row bottom; the
                // reach is capped at [`RAIL_REACH_MAX_CH`] — the leading's
                // ink-free margin the flat body proved — so the lobe's
                // deeper part below it stays bed ink, at the bar, under the
                // row below's caps and ascenders (review, 2026-09-13).
                let top = row_bottom.max(seg.spine);
                let dn = (seg.spine + seg.dn - top)
                    .max(DN_FLOOR_CH * chf)
                    .min(RAIL_REACH_MAX_CH * chf);
                verts.push(RibbonVertex {
                    x: seg.x,
                    spine: top,
                    up: 0.0,
                    dn,
                    core_up: 0.0,
                    core_dn: dn * RAIL_CORE_SHARE,
                    color: self.ink.rail_at(seg.t),
                    cov: f32::from(seg.cov) * RAIL_GAIN,
                    lift: 0.0,
                    lift_span: 0.0,
                });
            }
            verts.reverse();
            if !ribbon_beam(
                frame.under,
                clip,
                &verts,
                1.0,
                stride,
                budget,
                GlowBlend::Over,
            ) {
                break;
            }
        }
        self.runs = runs;
        self.verts = verts;
        self.emit_hot_edge(ctx, frame);
        // WHAT THIS FRAME PUT ON THE GLASS, off the frame's own pixels (see
        // [`Ribbon::ink_quads`]). One saturating compare per emitted quad,
        // no allocation.
        let bg = ctx.cfg.theme_bg;
        self.ink_quads = frame.under[ink_from..]
            .iter()
            .filter(|q| reads_as_ink(over_premul(bg, q.color, q.alpha)))
            .count();
    }

    /// How many of the last frame's ribbon quads read as band ink on the
    /// glass — `trail status`'s `ribbon_drawn=`. See [`Ribbon::ink_quads`].
    #[must_use]
    pub fn ink_quad_count(&self) -> usize {
        self.ink_quads
    }

    /// The gain of §4.1's hot edge, `0..1`, from the spine the head cell was
    /// BORN at. **THE A/B GATE**: the owner has struck a "highlighter" line
    /// twice and this is the first item on the A/B sheet (§22), so the whole
    /// refinement is one `0.0` away from gone — dark themes only, and only
    /// while the hand is on the mark.
    ///
    /// Priced from the head cell's [`Cell::birth_disp`], never the live
    /// follower: a hairline read off the follower brightens for ~100 ms after
    /// the last key while the attack finishes, which is light with no
    /// keystroke behind it. This is the PRICE only; [`Ribbon::emit_hot_edge`]
    /// multiplies in the head cell's envelope ([`Ribbon::env_of`]) so the
    /// hairline goes out with its body.
    ///
    /// `typed` is whether the head cell is a KEY's ([`Cell::typing`]): the
    /// floor is a typed key's alone, so a cold arrow's wake — no key, no
    /// momentum — carries exactly the hairline it always had, none.
    #[must_use]
    pub fn hot_edge_gain(cfg: &Config, disp: f32, typed: bool) -> f32 {
        if !cfg.dark_theme || cfg.reduced_motion {
            return 0.0;
        }
        // THE FLOOR (2026-09-14): the edge EXISTS on a cold hand — the first
        // key of a word attaches the band's top to the caret like any other
        // key — and the rest of it ramps with the hand as before. A wake's
        // head is no key (review, 2026-09-14): a cold hop landed under a
        // half-gain hairline it never had.
        let floor = if typed { HOT_EDGE_GAIN_FLOOR } else { 0.0 };
        floor
            + (1.0 - floor) * smoothstep01((clamp01(disp) - HOT_EDGE_DISP_MIN) / HOT_EDGE_DISP_SPAN)
    }

    /// **REFINEMENT A** (§4.1) — a 1-px additive hairline IN THE SPECTRUM
    /// along the ribbon's TOP edge, from the head cell back
    /// [`HOT_EDGE_CELLS`] cells: every vertex carries the stop under it,
    /// lifted to [`HOT_EDGE_LUMA_FLOOR`] ([`BedInkLut::hot_at`]), so at the
    /// hand it IS the head's stop, hot — brighter than the bed on every stop
    /// of the arc. (`#FFFFFF` at 38 until 2026-09-08; the owner: "I want the
    /// meteor to have rainbow! be a bigger more special rainbow impact!".)
    ///
    /// It is drawn as a 2-vertex-per-cell `comet_beam` hairline (one vertex
    /// per cell BOUNDARY) sampled at the same `spine − up` the body used,
    /// because the top edge carries the 0.75-cycle wave (≈ 2 px at `ch 18`,
    /// 3 px at retina) and three axis-aligned rects cannot follow it. It is
    /// SPATIAL rather than temporal — a property of where the head IS, with no
    /// clock of its own — so it can never lag the head: it ends at the head
    /// boundary [`Run::head`] resolves and reaches back only toward the tail.
    /// The one clock it does ride is the head cell's own
    /// ([`Ribbon::env_of`]): its gain is the birth-priced
    /// [`Ribbon::hot_edge_gain`] times the head cell's envelope, so it can
    /// never rise after the last key and it reaches exactly zero WITH the
    /// body — through the focus ember, the expiry melt and the exit swoosh
    /// alike — instead of holding at full coverage over a spent body and
    /// snapping off when the cells retire.
    fn emit_hot_edge(&self, ctx: &Ctx<'_>, frame: &mut Frame<'_>) {
        // THE RUN THE HAND IS ON, not merely the last one planned: the hot
        // edge is the head's own property, so it belongs to the run the caret
        // stands in, and failing that to the run carrying the newest cell.
        // `runs` is in emit order, which is exactly that order.
        let Some(run) = self.runs.first() else {
            return;
        };
        // "STILL WET AT THE HAND": the head cell itself, and only while it is
        // live light — a run whose every cell is draining has nothing wet.
        // The gain is priced from THAT cell's birth, not from the run's
        // newest-born cell, which after a backspace is the erased one.
        if !run.wet {
            return;
        }
        let Some(head_cell) = self
            .cells
            .iter()
            .filter(|c| c.row == run.row && c.col == run.head_col)
            .max_by(|a, b| a.born.cmp(&b.born))
        else {
            return;
        };
        // BIRTH-PRICED so it can never rise; ENVELOPE-SCALED so it goes out
        // with its body. The head cell's envelope is the mark's own clock at
        // the hand — the ember, the expiry melt, the swoosh's drain — and a
        // hairline that ignored it rode the retract at full coverage over a
        // body that had spent to zero, then snapped off with the cells.
        let gain = Self::hot_edge_gain(ctx.cfg, head_cell.birth_disp, head_cell.typing)
            * self.env_of(ctx, head_cell);
        if gain <= 0.0 {
            return;
        }
        let mouth_x = self.plan[run.head].x;
        // WHICH WAY IS BEHIND: `−1` (a typed run, a rightward wake) puts the
        // tail at lower x, `+1` (a leftward wake, `Run::stream_dir`) at
        // higher x — `behind` is positive on the tail side either way.
        let dir = f32::from(run.stream_dir);
        // THE HEAD CELL'S OWN REACH ([`edge_cells`]), priced when it was laid:
        // the hairline is the head's property, and the key that bought a
        // longer streak keeps it for as long as that key is the head.
        let cells = head_cell
            .edge_cells
            .clamp(HOT_EDGE_CELLS, HOT_EDGE_CELLS_MAX);
        let reach = cells * ctx.geom.cw as f32;
        frame.beams.clear();
        for seg in self.plan[run.lo..run.hi]
            .iter()
            .step_by(self.slabs_per_cell())
        {
            // Only the TAIL side of the head (see `Run::head`, `stream_dir`):
            // behind the hand, never over the cells it has erased.
            let behind = (mouth_x - seg.x) * (-dir);
            if !(0.0..=reach).contains(&behind) {
                continue;
            }
            let d = behind / ctx.geom.cw as f32;
            let a = HOT_EDGE_ALPHA * (1.0 - d / cells).powi(2);
            let cov = (HOT_EDGE_COV_MAX * (a / HOT_EDGE_ALPHA) * gain * clamp01(ctx.cfg.intensity))
                .clamp(0.0, HOT_EDGE_COV_MAX);
            frame.beams.push(BeamVertex {
                x: seg.x,
                y: seg.spine - seg.up,
                color: self.ink.hot_at(seg.t),
                cov: cov as u8,
            });
        }
        if frame.beams.len() < 2 {
            frame.beams.clear();
            return;
        }
        comet_beam(frame.out, ctx.geom.beam_clip(), frame.beams, 1.0, 1, 0.0);
        frame.beams.clear();
    }

    // -- the reads ---------------------------------------------------------

    /// This frame's field index — read by stardust (tint deal, §5.3) and by
    /// `Engine::field_at` (seam point 4).
    #[must_use]
    pub fn field(&self) -> &FieldIndex {
        &self.index
    }

    /// The field at a cell, or `None`.
    #[must_use]
    pub fn field_at(&self, row: u16, col: u16) -> Option<f32> {
        self.index.at(row, col)
    }

    /// **THE WALK ORIGIN AT A CELL** (2026-09-14) — `(anchor_col, t0)` of
    /// the cohort that owns the cell at `(row, col)`, or `None` where nothing
    /// is laid. The pair [`Cohort::t_at`] reads, so `t0 + walk_t(col −
    /// anchor_col)` is exactly [`Ribbon::field_at`]'s number at this cell
    /// and CONTINUES past it at the band's own pace — `d/16` on the run's
    /// first sixteen cells, `d/36` after — which is what a mark that must
    /// wear the band's colour at a column the band does not reach (the
    /// landing starburst's jets, `meteor::LandingWalk`) needs and the field
    /// alone cannot give. The owner is the same cell the index takes: the
    /// newest in pool order at the position ([`Ribbon::build`]'s
    /// newest-first sort). Read after [`Ribbon::plan`], like `field_at`.
    #[must_use]
    pub fn walk_origin_at(&self, row: u16, col: u16) -> Option<(u16, f32)> {
        let owner = self
            .cells
            .iter()
            .rev()
            .find(|c| c.row == row && c.col == col)?;
        let cohort = self.cohorts.iter().find(|c| c.id == owner.cohort)?;
        Some((cohort.anchor_col, cohort.t0))
    }

    /// **THE CARET'S POSITION IN THE FIELD** (v1's `rainbow_field`, seam point
    /// 4). The caret's own cell when it owns light; otherwise the newest cell
    /// REAL TYPING laid on the caret's row, then on any row (v1's "newest
    /// typing spark" — a reach cell or a retracting cell of an older, higher-t
    /// row is not the hand's colour); red when nothing is laid.
    ///
    /// Also §6.4's `t_land`: the meteor phase-locks its arc to THIS value at
    /// its spawn edge, which is why "the station under the caret is the
    /// caret's own stop by construction".
    #[must_use]
    pub fn field_at_caret(&self) -> f32 {
        if let Some(t) = self.caret.and_then(|(row, col)| self.field_at(row, col)) {
            return t;
        }
        let newest_typing = |row: Option<u16>| {
            self.cells
                .iter()
                .filter(|c| c.typing && !c.leaving() && row.is_none_or(|r| c.row == r))
                .max_by(|a, b| a.born.cmp(&b.born))
                .map(|c| c.t)
        };
        newest_typing(self.caret.map(|(row, _)| row))
            .or_else(|| newest_typing(None))
            .unwrap_or(0.0)
    }

    /// The source RGB of the ribbon HEAD's body, before premultiplication —
    /// what a companion cursor inherits to meet the laid ribbon with no
    /// palette seam (v1's `rainbow_head_rgb`, seam point 5). `None` before any
    /// cursor has been observed.
    ///
    /// **Dark themes return the AUTHORED spectrum colour** at the caret's
    /// field stop, exactly as v1 did — the consumer applies its own ink: the
    /// caret block lifts the stop to its light floor (§3.2 rank 3), and a
    /// companion inherits the stop, not the bed's dimmed on-glass ink. Handing
    /// out [`bed_ink`] here would paint a yellow caret olive. Light themes
    /// apply the body's own leading-ink recipe to that same position, so the
    /// caret meets the rail with no seam. Both arms resolve
    /// [`Ribbon::field_at_caret`] once, folded through the same `tri` the bed
    /// uses (C2).
    #[must_use]
    pub fn head_rgb(&self, cfg: &Config) -> Option<u32> {
        self.caret?;
        let arc = spectrum(clamp01(tri(self.field_at_caret())));
        Some(if cfg.dark_theme {
            arc
        } else {
            light_role(cfg).ink(arc)
        })
    }

    /// The body's vertical extent at a cell, or `None` where no cell is laid
    /// (§4.2, §5.4, D16). Valid only after [`Ribbon::plan`] has run for this
    /// frame.
    ///
    /// **REFINEMENT B'S PUBLICATION.** Stardust's sky band is
    /// `[top − 0.30 ch, top − 0.04 ch]` measured from THIS `top`, which is the
    /// whole of D16: under the shipped tall spelling the body covers the entire
    /// cell, so a birth zone stated in cell coordinates would put every strike
    /// star inside the stroke.
    #[must_use]
    pub fn band(&self, row: u16, col: u16) -> Option<Band> {
        let run = self
            .runs
            .iter()
            .find(|r| r.row == row && (r.col0..=r.col1).contains(&col))?;
        let slabs = self.slabs_per_cell();
        let idx = run.lo + usize::from(col - run.col0) * slabs + slabs / 2;
        let seg = self.plan.get(idx.min(run.hi.saturating_sub(1)))?;
        Some(Band {
            top: seg.spine - seg.up,
            spine: seg.spine,
            bottom: seg.spine + seg.dn,
        })
    }

    /// Live cells — `Status::cells` and one term of the idle test.
    #[must_use]
    pub fn live_cells(&self) -> usize {
        self.cells.len()
    }

    /// The planned boundaries that are LIT on the last frame — coverage at
    /// or over [`STATUS_LIT_COV`] after the time-fade, the edge-in and the
    /// expiry melt. This is what `trail status` claims as its ribbon
    /// (`ribbon_segments=` / `ribbon_hue_bands=`), so a claim is never made
    /// for a cell that is resident but has melted under what the glass shows:
    /// the paint-conformance bind pairs every status read with its frame and
    /// charges a claimed-but-dark frame as `ribbon_dark` (measured 2-3 such
    /// frames per image row on the first matrix run, all in the retract's
    /// fade, when the count was `live_cells`).
    pub fn lit_segments(&self) -> impl Iterator<Item = &Segment> {
        self.plan.iter().filter(|s| s.cov >= STATUS_LIT_COV)
    }

    /// **WHAT IS ACTUALLY ON THE GLASS** — every planned boundary whose own
    /// composite over the theme ground reads as band ink to the census
    /// ([`reads_as_ink`]), not only those over the CLAIM floor
    /// [`Ribbon::lit_segments`] keeps.
    ///
    /// [`STATUS_LIT_COV`] is a bound on what the status verb may CLAIM: it
    /// is solved for the bed's DIMMEST stop, so a claim made under it could
    /// not be honoured by the pixels of that stop. It is not a fact about
    /// the band. Reporting it as one is how `trail status` came to answer
    /// `ribbon_segments=0 ribbon_active=false` from +99 ms to +231 ms of
    /// EVERY curtain, while 1065 quads at alpha 102 down to 9 were still
    /// being composited — the shipped instrument saying the band was gone
    /// for the last half of every fall. `trail status` reports this number
    /// beside the claim (`ribbon_drawn=`), and `ribbon_active` reads it
    /// while a curtain is falling.
    pub fn ink_segments(&self) -> impl Iterator<Item = &Segment> {
        let bg = self.ground;
        let ink = &self.ink;
        self.plan.iter().filter(move |s| {
            if s.cov == 0 {
                return false;
            }
            // THE LEVEL THE SPINE ACTUALLY COMPOSITES, which is what the
            // census sees: the body's `cov` PLUS the baseline strip's accent
            // above it ([`STRIP_LIFT_GAIN`]), priced against the same
            // [`BODY_FRAME_TOP`] the emitter prices it against. Reading
            // `cov` alone here under-reports the last frames of a fall — the
            // emitter puts `cov · 1.35` down and the glass counts it.
            let a = (f32::from(s.cov) * (1.0 + STRIP_LIFT_GAIN)).min(BODY_FRAME_TOP) as u8;
            reads_as_ink(over_premul(bg, premul_rgb(ink.at(s.t), a), a))
        })
    }

    /// Milliseconds left in a falling curtain, or `None` when none is
    /// falling — `trail status`'s `ribbon_curtain_ms=`, so the verb says
    /// WHY a band on the glass is under the claim floor.
    #[must_use]
    pub fn curtain_left_ms(&self, now: Instant) -> Option<u32> {
        let at = self.curtain?;
        let r = now.saturating_duration_since(at).as_secs_f32();
        Some(((CURTAIN_S - r).max(0.0) * 1000.0).round() as u32)
    }

    /// True when nothing is laid and no transient is finishing — one of the
    /// three pools `Engine::next_change_deadline` folds (§18).
    ///
    /// Clockless on purpose: the POOLS are the state. A cell that has expired
    /// is removed by [`Ribbon::plan`], not left in place for a deadline read
    /// to notice, so "is anything laid" and "is anything visible" are the same
    /// question — which is what lets `needs_frame_cadence()` keep v1's
    /// argument-free signature at seam point 8.
    #[must_use]
    pub fn at_rest(&self) -> bool {
        self.cells.is_empty() && self.cohorts.is_empty()
    }

    /// **THE CADENCE LAW, brisk half** — whether per-frame MOTION is on the
    /// mark: a cell inside its 18 ms `edge-in` (the one attack, T3), the
    /// focus rearm's `edge-in`, or a cohort RETRACTING into the caret on
    /// `suck-in` — §4's 0.40 s, the swoosh's one motion; its grace, its
    /// reach beats and its fade are tails. Under reduced motion nothing
    /// moves (§6.11); the attack still ramps.
    #[must_use]
    pub fn brisk(&self, now: Instant) -> bool {
        let ramping = |at: Instant| now.saturating_duration_since(at).as_secs_f32() < EDGE_IN_S;
        // A falling curtain is the retract's own motion, on every cell.
        (!self.reduced && self.curtain.is_some() && !self.cells.is_empty())
            || self.cells.iter().any(|c| ramping(c.attack_at))
            || self.rearm.is_some_and(|(at, _)| ramping(at))
            || (!self.reduced
                && self.cohorts.iter().any(|c| {
                    Phase::at(
                        now.saturating_duration_since(c.alive_at).as_secs_f32(),
                        c.grace_s,
                    ) == Some(Phase::Retracting)
                }))
    }

    /// Seconds until the mark's SETTLE under a falling spine moves a pixel
    /// — the engine's own tail, because the wave and the wedge read
    /// `Ctx::disp` (§4: "both only SETTLE") and the ribbon cannot see the
    /// spine on its own. The head cell moves fastest: the wedge closes at
    /// `wedge_px · disp / DISP_RELEASE_TAU` px/s and the wave (amplitude
    /// `WAVE_AMP_CELLS·ch·disp`, travelling at `PHASE_RATE·disp` cycles/s)
    /// at its crest velocity; one pixel over their sum. `None` with nothing
    /// laid or the spine at rest.
    #[must_use]
    pub fn settle_step_s(&self, disp: f32) -> Option<f32> {
        if self.cells.is_empty() || !disp.is_finite() || disp <= 0.0 {
            return None;
        }
        let wedge = self.wedge_px * disp / DISP_RELEASE_TAU;
        let wave = if self.reduced {
            0.0
        } else {
            WAVE_AMP_CELLS * self.cell_h * disp * std::f32::consts::TAU * PHASE_RATE * disp
        };
        let rate = wedge + wave;
        if rate > 0.0 { Some(1.0 / rate) } else { None }
    }

    /// **THE CADENCE LAW** — the next instant this ribbon changes what is
    /// on glass. `None` when at rest, which is what lets the host go to 0 %
    /// idle (T6).
    ///
    /// Read as offers to the [`Cadence`] fold: the `edge-in`s and the
    /// retract are brisk ([`Ribbon::brisk`]); the reach beats (a cell
    /// appears on each), the retract's first frame and the swoosh's end are
    /// exact EDGES; and everything else is a TAIL — every fade (the expiry
    /// melt, a retracted cell's `spend`, the focus ember, the swoosh's
    /// farthest-first fade) at the next visible step solved from its own
    /// curve ([`level_step_melt`], [`level_step_spend`]) with the cell's
    /// other factors frozen into the peak, and every disappearance or
    /// fade-start (a cell's expiry, a kill's staggered retract stamps, the
    /// ember's end) at its instant — all floored. The peak is the dark
    /// cap's ([`UNDER_COV_CAP`]): the light role's is lower, and a lower
    /// peak only puts the step later, so the dark reading is never late.
    /// Under reduced motion every fade is the theme's one linear 120 ms
    /// fade (§6.11): its opening, then the floor inside it.
    #[must_use]
    pub fn next_change_deadline(&self, now: Instant) -> Option<Instant> {
        let mut cad = Cadence::at(now);
        if self.brisk(now) {
            cad.brisk();
        }
        let since = |at: Instant| now.saturating_duration_since(at).as_secs_f32();
        let dur = std::time::Duration::from_secs_f32;
        let reduced_fade_s = REDUCED_MOTION_FADE_MS / 1000.0;
        for cell in &self.cells {
            // The expiry clock is the cohort's (`live_since`), as `env_of`
            // and `retire` read it.
            let age = since(live_since(&self.cohorts, cell));
            let life = cell.life_s.max(1e-3);
            cad.tail(cell.life_s - age);
            let coh = self.cohorts.iter().find(|c| c.id == cell.cohort);
            if self.reduced {
                let ends = [
                    Some(cell.life_s - age),
                    cell.retract_at.map(|at| RETRACT_FADE_S - since(at)),
                    cell.retire_at.map(|at| RETIRE_MELT_S - since(at)),
                    self.ember_at.map(|at| FOCUS_EMBER_S - since(at)),
                    coh.map(|c| c.swoosh_total() - since(c.alive_at)),
                ];
                for left in ends.into_iter().flatten() {
                    cad.tail((left - reduced_fade_s).max(0.0));
                }
                continue;
            }
            let base = UNDER_COV_CAP * cell.cov0;
            let u = clamp01(age / life);
            let melt = expiry_melt(u);
            let retract_u = cell
                .retract_at
                .map(|at| clamp01(since(at) / RETRACT_FADE_S));
            let ember_u = self.ember_at.map(|at| clamp01(since(at) / FOCUS_EMBER_S));
            let swoosh_u = coh.and_then(|c| {
                let idle = since(c.alive_at);
                if Phase::at(idle, c.grace_s) != Some(Phase::Fading) {
                    return None;
                }
                let t0 = c.retract_start() + RETRACT_DUR_S * (1.0 - c.drain_rank(cell.col));
                Some(clamp01((idle - t0) / RETRACT_FADE_S))
            });
            // The content retirement's melt is one more `spend` factor on
            // the same product, and one more ending.
            let retire_u = cell.retire_at.map(|at| clamp01(since(at) / RETIRE_MELT_S));
            let f_r = retract_u.map_or(1.0, spend);
            let f_s = swoosh_u.map_or(1.0, spend);
            let f_m = retire_u.map_or(1.0, spend);
            // The ember is priced against the light already spent, exactly as
            // `env_of` prices it.
            let carried = base * melt * f_r * f_s * f_m;
            let ember_floor = self.ember_floor_of(carried, cell.t);
            let f_e = ember_u.map_or(1.0, |u| ember_level(ember_floor, spend(u)));
            if let Some(step) = level_step_melt(base * f_r * f_e * f_s * f_m, u, life) {
                cad.tail(step);
            }
            if let Some(ur) = retract_u
                && let Some(step) =
                    level_step_spend(base * melt * f_e * f_s * f_m, ur, RETRACT_FADE_S)
            {
                cad.tail(step);
            }
            if let Some(ue) = ember_u
                && let Some(step) = level_step_spend(
                    // Only `1 - floor` of the carried peak moves with the
                    // ember now: `ember_level` is affine in the common fade,
                    // so this is that fade's own step, exactly.
                    carried * (1.0 - ember_floor).max(0.0),
                    ue,
                    FOCUS_EMBER_S,
                )
            {
                cad.tail(step);
            }
            if let Some(us) = swoosh_u
                && let Some(step) =
                    level_step_spend(base * melt * f_r * f_e * f_m, us, RETRACT_FADE_S)
            {
                cad.tail(step);
            }
            if let Some(um) = retire_u
                && let Some(step) =
                    level_step_spend(base * melt * f_r * f_e * f_s, um, RETIRE_MELT_S)
            {
                cad.tail(step);
            }
            if let Some(at) = cell.retract_at {
                cad.tail_at(at);
                cad.tail_at(at + dur(RETRACT_FADE_S));
            }
            if let Some(at) = cell.retire_at {
                cad.tail_at(at + dur(RETIRE_MELT_S));
            }
        }
        for coh in &self.cohorts {
            // Appearances and the one motion's start are EDGES: the grace
            // ending on the first reach beat, the two beats after it, the
            // retract's first frame, and the swoosh's end (the cohort
            // retires — the idle instant). The retract's end is a tail: the
            // retract is brisk until then, and what follows is a fade.
            let grace = coh.grace_s.max(PHRASE_REST_MIN_S);
            for beat in [
                grace,
                grace + REACH_STEP_S,
                grace + 2.0 * REACH_STEP_S,
                coh.retract_start(),
                coh.swoosh_total(),
            ] {
                cad.edge(coh.alive_at + dur(beat));
            }
            cad.tail_at(coh.alive_at + dur(coh.retract_start() + RETRACT_DUR_S));
        }
        if let Some(at) = self.ember_at {
            cad.tail_at(at + dur(FOCUS_EMBER_S));
        }
        if let Some(at) = self.curtain {
            cad.tail_at(at + dur(CURTAIN_S));
        }
        cad.take()
    }

    /// **A SCROLL CARRIES THE RIBBON WITH ITS TEXT** (seam point 12): move
    /// every laid cell by a scroll of `rows` rows, with everything that is
    /// addressed by a cell's row — the cohorts, the field index the other
    /// producers read, and the caret the field is resolved against. What
    /// leaves the grid is dropped rather than clamped: a ribbon pinned to row
    /// 0 by a clamp is light on a line nobody typed.
    ///
    /// **There is no pixel to move, so no cell height is taken.** Every piece
    /// of ribbon STATE is addressed by grid row and clock: a cell is `(row,
    /// col)` plus its instants and its prices, a cohort is a row and a column
    /// span, the retract clocks and the focus ember are instants, and the
    /// wave's phase is the spine's ([`Ctx::phase`]), not the mark's. The pixel
    /// geometry — [`Ribbon::plan`], the runs, the hot edge's hairline, the
    /// bands stardust reads — is this frame's scratch, rebuilt from the cells
    /// by the next tick's [`Ribbon::plan`] before any producer reads it, so it
    /// is dropped here. The seam's `cell_h` belongs to the meteor's and the
    /// sky's halves, whose marks are window-absolute px; a parameter this
    /// half accepted and ignored read as a pixel path that did not exist.
    ///
    /// The index and the caret are TRANSLATED, not dropped. Between this call
    /// and the next plan the ribbon is still asked — the engine seeds the
    /// tick's `caret_t` from [`Ribbon::field_at_caret`] BEFORE it plans, and
    /// [`Ribbon::field_at`], [`Ribbon::head_rgb`] and [`Ribbon::caret`] are
    /// public reads the host may make between its `note_scroll` and its tick
    /// — and the honest answer is the MOVED cell's, which a reset index and a
    /// caret left on the pre-scroll row could not give (the index answered
    /// `None` for the moved cell, and the caret fell through to the newest
    /// key's stop). On glass the old spelling never showed: the same tick's
    /// plan rebuilt both before anything drew. The caret is a position, not
    /// a mark, so it saturates at row 0 as the engine's does; the next plan
    /// re-observes it.
    pub fn translate_scroll(&mut self, rows: u16) {
        if rows == 0 {
            return;
        }
        self.cells.retain_mut(|c| {
            if c.row < rows {
                return false;
            }
            c.row -= rows;
            true
        });
        self.cohorts.retain_mut(|c| {
            if c.row < rows {
                return false;
            }
            c.row -= rows;
            true
        });
        let cells = &self.cells;
        self.cohorts
            .retain(|coh| cells.iter().any(|c| c.cohort == coh.id));
        self.cells
            .retain(|c| self.cohorts.iter().any(|coh| coh.id == c.cohort));
        self.caret = self.caret.map(|(row, col)| (row.saturating_sub(rows), col));
        // The re-anchor's bookkeeping rides the scroll with its text: a
        // composer at the screen's bottom grows a row UP by scrolling the
        // screen in the very repaint that moves the word down.
        self.last_space = self
            .last_space
            .filter(|&(row, _)| row >= rows)
            .map(|(row, col)| (row - rows, col));
        self.relocate = self.relocate.filter(|r| r.row >= rows).map(|r| Relocate {
            row: r.row - rows,
            ..r
        });
        self.index.translate(rows);
        self.plan.clear();
        self.runs.clear();
    }

    /// **A ROW BAND CARRIES ITS RIBBON WITH ITS TEXT** (seam point 12, the
    /// band path) — [`Ribbon::translate_scroll`] restated for the motion a
    /// whole-grid scroll cannot express: screen rows `top..=bottom` moved by
    /// `delta` rows and every other row stood still. Codex's inline viewport
    /// sliding DOWN one row per streamed line under the hand that is typing
    /// into it (`[vt..56] +1`), its pinned transcript archiving UP under a
    /// fixed composer (`[0..51] −1`), an Enter's `RI×k`, a tmux pane or a
    /// vim status row scrolling inside its DECSTBM region, IL/DL — before
    /// this fence every one of them reached the engine as a reset, and the
    /// measured Codex session read `ribbon_segments 16 → 0` on the first
    /// streamed line (docs/measured/codex-on-glass-2026-09-10.md).
    ///
    /// The law is [`crate::cursor_glow::band_row`]'s, member by member: a
    /// cell or cohort outside the band is untouched, one inside moves by
    /// `delta` with every clock and price it carries, and one carried past
    /// the band's edge is DROPPED rather than clamped — the edge row is a
    /// real cell nobody typed this light on. The two orphan sweeps then keep
    /// "a cohort has cells" and "a cell has a cohort" true, exactly as the
    /// scroll twin does. The caret is a POSITION ([`crate::cursor_glow::
    /// band_pos`]): saturated at the band's edge, re-observed by the host's
    /// next move. The field index is translated lane by lane so
    /// [`Ribbon::field_at`] / [`Ribbon::field_at_caret`] answer for the
    /// MOVED cell between this call and the next plan (the engine seeds the
    /// tick's `caret_t` from the caret's field BEFORE it plans). No pixel is
    /// taken, for the reason the scroll twin gives: every piece of ribbon
    /// STATE is a grid row and a clock; the pixel geometry is this frame's
    /// scratch, dropped here and rebuilt by the next [`Ribbon::plan`].
    pub fn translate_band(&mut self, top: u16, bottom: u16, delta: i16) {
        if delta == 0 || top > bottom {
            return;
        }
        self.cells
            .retain_mut(|c| match band_row(c.row, top, bottom, delta) {
                Some(row) => {
                    c.row = row;
                    true
                }
                None => false,
            });
        self.cohorts
            .retain_mut(|c| match band_row(c.row, top, bottom, delta) {
                Some(row) => {
                    c.row = row;
                    true
                }
                None => false,
            });
        let cells = &self.cells;
        self.cohorts
            .retain(|coh| cells.iter().any(|c| c.cohort == coh.id));
        self.cells
            .retain(|c| self.cohorts.iter().any(|coh| coh.id == c.cohort));
        self.caret = self
            .caret
            .map(|(row, col)| (band_pos(row, top, bottom, delta), col));
        self.last_space = self
            .last_space
            .and_then(|(row, col)| band_row(row, top, bottom, delta).map(|row| (row, col)));
        self.relocate = self
            .relocate
            .and_then(|r| band_row(r.row, top, bottom, delta).map(|row| Relocate { row, ..r }));
        self.index.translate_band(top, bottom, delta);
        self.plan.clear();
        self.runs.clear();
    }

    /// Drop everything — style switch, layout change, `Engine::reset`.
    pub fn reset(&mut self) {
        self.cells.clear();
        self.cohorts.clear();
        self.plan.clear();
        self.runs.clear();
        self.sorted.clear();
        self.verts.clear();
        self.index.reset();
        self.caret = None;
        self.ember_at = None;
        self.rearm = None;
        self.pending_erase = 0;
        self.last_space = None;
        self.relocate = None;
        self.last_walk = None;
        // The phrase went with its cells; the hand's tempo and its last key
        // are the hand's, not the coordinate space's, and stay.
        self.phrase_open = None;
        self.curtain = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cursor_glow::{Geom, SoundCue};
    use aterm_render::{GlowQuad, RainHalo, over_premul, premul_rgb};
    use std::time::Duration;

    use super::super::spine::{SURGE_FULL_RISE, SURGE_MIN_RISE};
    use super::super::{CaretSeam, Dir, TypedClass};

    /// The shipped dark theme, which every legibility number in the family is
    /// solved against.
    const DEFAULT_BG: u32 = 0x001A_1B26;
    const DEFAULT_FG: u32 = 0x00C8_D3F5;
    /// The owner's theme (`theme = "Nord"`): the pair the black-gaps report
    /// was made on and re-measured against.
    const NORD_BG: u32 = 0x002E_3440;
    const NORD_FG: u32 = 0x00D8_DEE9;

    /// **THE THEME THE PRODUCT ACTUALLY OPENS WITH** — `ColorScheme::default`,
    /// what a config with no `theme` line gets, and the DARKEST ground aterm
    /// ships. `DEFAULT_BG` above is Tokyo Night's `#1A1B26` and is not this
    /// one: the ember's first twin ran on Tokyo Night and Nord, the theme
    /// every user sees on first launch was in neither arm, and that is where
    /// the ember fragmented.
    const SHIPPED_BG: u32 = 0x0011_1318;
    const SHIPPED_FG: u32 = 0x00D0_D0D0;

    /// The 1× fixture: `cw 9`, `ch 18`, 120 × 40 cells.
    fn geom() -> Geom {
        Geom {
            cw: 9,
            ch: 18,
            rows: 40,
            cols: 120,
            origin_x: 0,
            origin_y: 0,
            win_w: 1080,
            win_h: 720,
            head: 0,
        }
    }

    /// The 2× (retina) fixture §18's worst case is stated at.
    fn geom2x() -> Geom {
        Geom {
            cw: 18,
            ch: 36,
            rows: 40,
            cols: 120,
            origin_x: 0,
            origin_y: 0,
            win_w: 2160,
            win_h: 1440,
            head: 0,
        }
    }

    fn cfg(dark: bool, tall: bool) -> Config {
        Config {
            dark_theme: dark,
            intensity: 1.0,
            duration: Duration::from_millis(400),
            ribbon_tall: tall,
            ribbon_flat: false,
            theme_fg: if dark { DEFAULT_FG } else { 0x0016_161C },
            theme_bg: if dark { DEFAULT_BG } else { 0x00FF_FFFF },
            reduced_motion: false,
        }
    }

    /// The `… flat` spelling's config: the body as it shipped on 2026-09-13,
    /// before the comet and its rail (`Config::ribbon_flat`).
    fn flat(dark: bool, tall: bool) -> Config {
        let mut c = cfg(dark, tall);
        c.ribbon_flat = true;
        c
    }

    fn ctx_in<'a>(now: Instant, cfg: &'a Config, caret: (u16, u16), disp: f32, g: Geom) -> Ctx<'a> {
        Ctx {
            now,
            geom: g,
            cfg,
            disp,
            birth_disp: disp,
            phase: 0.0,
            caret,
            caret_t: 0.0,
            caret_walk: None,
            mend: None,
            surge: 0.0,
            flow: Default::default(),
        }
    }

    fn ctx<'a>(now: Instant, cfg: &'a Config, caret: (u16, u16), disp: f32) -> Ctx<'a> {
        ctx_in(now, cfg, caret, disp, geom())
    }

    #[derive(Default)]
    struct Sink {
        under: Vec<GlowQuad>,
        out: Vec<GlowQuad>,
        halos: Vec<RainHalo>,
        beams: Vec<BeamVertex>,
        cues: Vec<SoundCue>,
    }

    impl Sink {
        fn frame(&mut self) -> Frame<'_> {
            self.under.clear();
            self.out.clear();
            self.halos.clear();
            self.cues.clear();
            Frame {
                under: &mut self.under,
                out: &mut self.out,
                halos: &mut self.halos,
                beams: &mut self.beams,
                cues: &mut self.cues,
                caret: CaretSeam::default(),
                companion: None,
                fp: 0,
            }
        }
    }

    fn at(t0: Instant, ms: u64) -> Instant {
        t0.checked_add(Duration::from_millis(ms)).expect("clock")
    }

    fn typed() -> Event {
        Event::Typed {
            cells: 1,
            shifted: false,
            class: TypedClass::Glyph,
        }
    }

    fn nav(from: (u16, u16), to: (u16, u16)) -> Event {
        Event::Move {
            from,
            to,
            licence: Licence::Nav,
            dir: Dir::Right,
        }
    }

    /// A program-driven move (no credit): abandons, lays no wake.
    fn pty(from: (u16, u16), to: (u16, u16)) -> Event {
        Event::Move {
            from,
            to,
            licence: Licence::Pty,
            dir: Dir::Right,
        }
    }

    /// A typing burst: `n` keys on `row` from `col0`, one per `period_ms`,
    /// at spine `disp`, in `g`.
    #[derive(Clone, Copy)]
    struct Keys {
        g: Geom,
        row: u16,
        col0: u16,
        n: u16,
        period_ms: u64,
        disp: f32,
    }

    /// Type `keys` from `t0`, planning after every key as the engine does.
    fn type_keys(rib: &mut Ribbon, t0: Instant, keys: Keys, c: &Config) {
        for i in 0..keys.n {
            let now = at(t0, u64::from(i) * keys.period_ms);
            let caret = (keys.row, keys.col0 + i + 1);
            let cx = ctx_in(now, c, caret, keys.disp, keys.g);
            rib.on_event(&typed(), now, &cx);
            rib.plan(&cx);
        }
    }

    /// Type `n` cells starting at column `col0` on row 2, one key per 60 ms.
    fn type_run(rib: &mut Ribbon, t0: Instant, col0: u16, n: u16, c: &Config, disp: f32) {
        let keys = Keys {
            g: geom(),
            row: 2,
            col0,
            n,
            period_ms: 60,
            disp,
        };
        type_keys(rib, t0, keys, c);
    }

    /// One Backspace landing the caret at `caret`, planned.
    fn erase_at(rib: &mut Ribbon, now: Instant, caret: (u16, u16), c: &Config) {
        let cx = ctx(now, c, caret, 0.7);
        rib.on_event(&Event::Erase, now, &cx);
        rib.plan(&cx);
    }

    /// True when a body quad lands in the UPPER HALF of cell `(row, col)`'s
    /// own band. The tall body covers the whole cell, so an unlit cell is a
    /// shed cell; the upper half, because the row below reaches `0.10 ch`
    /// into this row's bottom and would vouch for a cell that is not there.
    fn cell_lit(under: &[GlowQuad], g: Geom, row: u16, col: u16) -> bool {
        let x0 = f32::from(g.origin_x) + f32::from(col) * g.cw as f32;
        let x1 = x0 + g.cw as f32;
        let y0 = f32::from(g.origin_y) + f32::from(row) * g.ch as f32;
        let y1 = y0 + g.ch as f32 * 0.5;
        under.iter().any(|q| {
            let qx0 = f32::from(q.x);
            let qx1 = qx0 + f32::from(q.w);
            let qy = f32::from(q.y);
            qx0 < x1 && qx1 > x0 && (y0..y1).contains(&qy)
        })
    }

    /// **THE DEVICE-COLUMN CENSUS** — one entry per device column of
    /// `xs`, in window-absolute px: the topmost device row carrying at least
    /// `floor` levels of light inside the device rows `ys`, the bottommost,
    /// and the greatest level in the column. `None` where no quad reaches
    /// the floor. A source-over quad's
    /// level is its opacity byte; an ADDITIVE quad's (`alpha == 0`, the hot
    /// edge in `out`) is its brightest premultiplied channel — the light it
    /// adds. It is the unit the head's attach and the seam pins are stated
    /// in: a per-CELL census cannot see a step inside a cell, and the
    /// owner's notch is inside the head cell.
    fn column_census(
        quads: &[GlowQuad],
        xs: std::ops::Range<i32>,
        ys: std::ops::Range<i32>,
        floor: u8,
    ) -> Vec<Option<(i32, i32, u8)>> {
        let level = |q: &GlowQuad| -> u8 {
            if q.alpha > 0 {
                q.alpha
            } else {
                let c = q.color;
                ((c >> 16) & 0xff).max((c >> 8) & 0xff).max(c & 0xff) as u8
            }
        };
        xs.map(|x| {
            let mut top = i32::MAX;
            let mut bot = i32::MIN;
            let mut peak = 0u8;
            for q in quads {
                let lv = level(q);
                if lv < floor || q.w == 0 {
                    continue;
                }
                let qx0 = i32::from(q.x);
                if x < qx0 || x >= qx0 + i32::from(q.w) {
                    continue;
                }
                let y = i32::from(q.y);
                if !ys.contains(&y) {
                    continue;
                }
                top = top.min(y);
                bot = bot.max(y + i32::from(q.h) - 1);
                peak = peak.max(lv);
            }
            (top != i32::MAX).then_some((top, bot, peak))
        })
        .collect()
    }

    fn contrast(a: u32, b: u32) -> f32 {
        let (la, lb) = (relative_luminance(a), relative_luminance(b));
        let (hi, lo) = if la >= lb { (la, lb) } else { (lb, la) };
        (hi + 0.05) / (lo + 0.05)
    }

    fn peak_channel(quads: &[GlowQuad]) -> u32 {
        quads
            .iter()
            .map(|q| {
                ((q.color >> 16) & 0xff)
                    .max((q.color >> 8) & 0xff)
                    .max(q.color & 0xff)
            })
            .max()
            .unwrap_or(0)
    }

    fn plan_peak(rib: &Ribbon) -> u8 {
        rib.plan_segments().iter().map(|s| s.cov).max().unwrap_or(0)
    }

    fn plan_sum(rib: &Ribbon) -> u32 {
        rib.plan_segments().iter().map(|s| u32::from(s.cov)).sum()
    }

    /// The planned light up to and including the boundary at column `col`
    /// on the 1× fixture — the BAND's own, without the attach under the
    /// caret cell beyond it (2026-09-14), which is the caret's light and
    /// leaves with the caret.
    fn band_sum(rib: &Ribbon, col: u16) -> u32 {
        let x = f32::from(col) * geom().cw as f32;
        rib.plan_segments()
            .iter()
            .filter(|s| s.x <= x + 0.5)
            .map(|s| u32::from(s.cov))
            .sum()
    }

    // -- §2.2 L3, §3.2: the bed's one ceiling ------------------------------

    #[test]
    fn letters_stay_legible_under_the_ribbon_on_the_default_dark_theme() {
        let budget = bed_luma_budget(DEFAULT_FG);
        let mut worst = f32::INFINITY;
        for i in 0..=512u32 {
            let t = i as f32 / 512.0;
            let ink = bed_ink(spectrum(t), budget);
            // Every level the emitter can composite — the body's own request
            // AND the strip's accent above it — not merely the bed's ceiling:
            // the bar must hold on the whole ramp, not only at the top of it.
            for cov in 1..=(BODY_FRAME_TOP as u32) {
                let cov = cov as u8;
                let lit = over_premul(DEFAULT_BG, premul_rgb(ink, cov), cov);
                worst = worst.min(contrast(DEFAULT_FG, lit));
            }
        }
        assert!(
            worst >= BODY_CONTRAST_BAR,
            "the bed composited to {worst:.3}:1 against the default foreground; the bar is {BODY_CONTRAST_BAR}:1"
        );
    }

    #[test]
    fn the_dimmest_stop_of_the_bed_composites_at_a_max_channel_of_about_80_under_the_bar() {
        // "Dim and muddy" gets a NUMBER. Under L3's 5.25:1 bar every stop is
        // put on `bed_luma_budget(fg)`, and a warm stop at that luminance IS
        // an olive: on the default theme a full-coverage yellow composites at
        // `(80, 80, 3)`, at or below the ≈ 84 v1's coverage table produced.
        // The module does not fix that half of the measured defect, and this
        // pin is what keeps the module doc from claiming it does.
        //
        // RE-PINNED 2026-09-08, 78 → 80 (measured 79 → 80): the owner struck
        // the restraint under the bar ("I feel like you are diminishing the
        // specialness and emphasis of this theme? why?") and kept the bar —
        // the 0.15 guard is the 0.05 the rounding needs, the solver answers
        // from under its target, and the clamp is the bar's own answer at a
        // white foreground. On the default theme that is one level; the
        // bright-foreground gain is pinned beside this in
        // `a_bright_foreground_s_bed_takes_the_bar_s_whole_budget_not_a_tenth`.
        //
        // TWO-SIDED on purpose. A ruling that relaxes the bar for the bed
        // over blank cells (REJECTED 2026-09-06, module doc) would land the
        // warm mids above 84 and must re-pin this line; a change that dims the
        // bed trips the floor. Either way the number moves HERE first, not in
        // a screenshot.
        let budget = bed_luma_budget(DEFAULT_FG);
        let cov = UNDER_COV_CAP as u8;
        let dimmest = (0..=32u32)
            .map(|i| {
                let t = i as f32 / 32.0;
                let lit = over_premul(
                    DEFAULT_BG,
                    premul_rgb(bed_ink(spectrum(t), budget), cov),
                    cov,
                );
                (max_channel(lit), i, lit)
            })
            .min()
            .expect("33 stops");
        let (max, i, lit) = dimmest;
        let t = i as f32 / 32.0;
        assert!(
            (79..=81).contains(&max),
            "the dimmest stop (t = {t:.3}, composited #{lit:06X}) peaks at max channel {max}; the bar puts it at ≈ 80 and v1's table at ≈ 84 — a brighter warm mid is a ruling on the bar, a dimmer one is light left under it"
        );
    }

    /// The worst contrast the bed can composite against `fg` over `bg` —
    /// every position of the arc, every level the emitter can reach (the
    /// body's request and the strip's accent, to the ledger's frame top).
    fn worst_bed_contrast(fg: u32, bg: u32) -> f32 {
        worst_bed_contrast_at(fg, bg, bed_luma_budget(fg))
    }

    /// The same, with the bed put on a stated luminance instead of the
    /// theme's own budget — what a different budget WOULD read at.
    fn worst_bed_contrast_at(fg: u32, bg: u32, budget: f32) -> f32 {
        let mut worst = f32::INFINITY;
        for i in 0..=512u32 {
            let ink = bed_ink(spectrum(i as f32 / 512.0), budget);
            for cov in 1..=(BODY_FRAME_TOP as u32) {
                let cov = cov as u8;
                worst = worst.min(contrast(fg, over_premul(bg, premul_rgb(ink, cov), cov)));
            }
        }
        worst
    }

    fn max_channel(c: u32) -> u32 {
        ((c >> 16) & 0xff).max((c >> 8) & 0xff).max(c & 0xff)
    }

    fn min_channel(c: u32) -> u32 {
        ((c >> 16) & 0xff).min((c >> 8) & 0xff).min(c & 0xff)
    }

    #[test]
    fn the_bed_sits_at_the_bar_not_under_it_on_the_default_dark_theme() {
        // THE OWNER, 2026-09-08: "I feel like you are diminishing the
        // specialness and emphasis of this theme? why?" — and the one
        // restraint that stays is the bar itself, not a margin under it. The
        // bed carried a 0.15 guard ("more than all three [rounding costs]
        // can spend together") and composited at 5.3729:1 at its worst.
        // Measured: `solve_for_luma` answering from UNDER its target makes
        // the ink's own rounding cost nothing, and the composite's one level
        // of rounding costs 0.05 of a ratio point at the dimmest theme above
        // the floor (`letters_stay_legible_under_the_ribbon_on_every_dark_theme_above_the_floor`)
        // — so that is the guard, and the bed sits within a tenth of the bar.
        let worst = worst_bed_contrast(DEFAULT_FG, DEFAULT_BG);
        assert!(
            worst >= BODY_CONTRAST_BAR,
            "the bed composited to {worst:.4}:1; the bar is {BODY_CONTRAST_BAR}:1"
        );
        assert!(
            worst - BODY_CONTRAST_BAR <= 0.10,
            "the bed's worst stop composites at {worst:.4}:1 against a {BODY_CONTRAST_BAR}:1 bar — light left under the bar is diminished light; the guard under it is one level of rounding, not {:.4} of a ratio point",
            worst - BODY_CONTRAST_BAR
        );
    }

    #[test]
    fn letters_stay_legible_under_the_ribbon_on_every_dark_theme_above_the_floor() {
        // THE NET UNDER THE GUARD. 67 foregrounds — the greys `#909090` to
        // `#FFFFFF` in steps of three, 27 tints from `{A0, D0, FF}³`, the
        // shipped defaults — over four grounds, each only where the ground
        // sits under the bed's own ceiling (a ground brighter than the bed is
        // the theme's contrast, not the bed's), through the ledger's frame
        // top plus one Bayer level. Foregrounds the floor catches
        // (`bed_luma_budget` clamped up to `BED_LUMA_MIN`, fg ≤ ≈ `#999999`)
        // are the documented exception — "below this the ribbon stops obeying
        // the bar" — and are skipped. This sweep is what sized
        // `BODY_CONTRAST_GUARD`: at 0 the worst was 5.2019:1 (fg `#A0A0A0`,
        // one level of composite rounding at a 0.026 budget); 0.05 is the
        // first step of the sweep that holds — and it is a law the 0.15 guard
        // did NOT hold, because its solver could answer a level over its
        // target (5.2039:1 at fg `#999999` before).
        let mut fgs: Vec<u32> = (0x90u32..=0xFF)
            .step_by(3)
            .map(|g| (g << 16) | (g << 8) | g)
            .collect();
        for r in [0xA0u32, 0xD0, 0xFF] {
            for g in [0xA0u32, 0xD0, 0xFF] {
                for b in [0xA0u32, 0xD0, 0xFF] {
                    fgs.push((r << 16) | (g << 8) | b);
                }
            }
        }
        fgs.extend([
            DEFAULT_FG,
            0x00E8_E8F0,
            0x00F8_F8F2,
            0x00AB_B2BF,
            0x00CD_D6F4,
        ]);
        // Nord's pair — the owner's own theme — rides in the sweep too.
        fgs.push(NORD_FG);
        let grounds = [0x0000_0000u32, DEFAULT_BG, 0x0028_2C34, NORD_BG];
        let mut worst = f32::INFINITY;
        let mut at = String::new();
        let mut themes = 0;
        for fg in fgs {
            // Floor-clamped (`BED_LUMA_MIN`) is the documented exception:
            // those foregrounds are skipped whole. This and the
            // ground-brighter-than-the-bed skip below are the ONLY two skips
            // — a third was added on 2026-09-09 to hide the pairs a ground
            // floor lifted over the bar, and it was taken out with the floor.
            let budget = bed_luma_budget(fg);
            if budget <= BED_LUMA_MIN {
                continue;
            }
            themes += 1;
            for bg in grounds {
                if relative_luminance(bg) >= budget {
                    continue;
                }
                for i in 0..=128u32 {
                    let ink = bed_ink(spectrum(i as f32 / 128.0), budget);
                    for cov in 1..=(BODY_FRAME_TOP as u32 + 1) {
                        let cov = cov as u8;
                        let lit = over_premul(bg, premul_rgb(ink, cov), cov);
                        let c = contrast(fg, lit);
                        if c < worst {
                            worst = c;
                            at = format!(
                                "fg #{fg:06X} over #{bg:06X}, t {:.3}, cov {cov}, #{lit:06X}",
                                i as f32 / 128.0
                            );
                        }
                    }
                }
            }
        }
        assert!(
            themes >= 60,
            "the sweep must cover the themes it claims ({themes})"
        );
        assert!(
            worst >= BODY_CONTRAST_BAR,
            "the bed composited to {worst:.4}:1 at {at}; the bar is {BODY_CONTRAST_BAR}:1 on every dark theme above the floor"
        );
    }

    #[test]
    fn a_bright_foreground_s_bed_takes_the_bar_s_whole_budget_not_a_tenth() {
        // MORE COLOUR where the bar allows it (the owner, 2026-09-08: "make
        // this rainbow theme truly magical and special and dynamic and
        // beautiful"). `BED_LUMA_MAX` was 0.100 — a clamp that bound on every
        // foreground brighter than ≈ `#D8D8D8`: the offline renderer's
        // `#E8E8F0` and a white foreground both solved to the clamp, under
        // the bar's own answers (0.114 and 0.150). The ceiling is now the
        // bar's answer at a white foreground, so under the bar the clamp
        // binds nowhere.
        let white = bed_luma_budget(0x00FF_FFFF);
        assert!(
            white > 0.14,
            "a white foreground's bed budget is {white:.4}; the bar itself allows ≈ 0.148 and the bed took 0.100"
        );
        let cov = UNDER_COV_CAP as u8;
        let yellow_at = |fg: u32, bg: u32| {
            max_channel(over_premul(
                bg,
                premul_rgb(bed_ink(0x00FF_FF00, bed_luma_budget(fg)), cov),
                cov,
            ))
        };
        let (white_bed, renderer_bed) = (
            yellow_at(0x00FF_FFFF, 0),
            yellow_at(0x00E8_E8F0, 0x0011_1318),
        );
        assert!(
            (102..=104).contains(&white_bed),
            "yellow at the bed's ceiling under a white foreground composites at max channel {white_bed}; the bar puts it at ≈ 103 (it was 87 under the 0.100 clamp)"
        );
        assert!(
            (91..=93).contains(&renderer_bed),
            "yellow at the bed's ceiling under the renderer's `#E8E8F0` composites at max channel {renderer_bed}; the bar puts it at ≈ 92 (it was 88 under the 0.100 clamp)"
        );
    }

    #[test]
    fn every_stop_of_the_arc_composites_at_one_weight() {
        // The EVEN-ARC law — the "navy mids" half of the measured defect:
        // after the ink recipe, no two positions of the arc differ in
        // composited luminance by more than the byte quantization can
        // explain. It is NOT the "olive mids" half: the shared weight is the
        // bar's own, and the pin above says what that weight looks like.
        let budget = bed_luma_budget(DEFAULT_FG);
        let (mut lo, mut hi) = (f32::INFINITY, 0.0f32);
        for i in 0..=256u32 {
            let y = relative_luminance(bed_ink(spectrum(i as f32 / 256.0), budget));
            lo = lo.min(y);
            hi = hi.max(y);
        }
        assert!(
            hi - lo <= 0.004,
            "the arc's composited weight spans {lo:.4}..{hi:.4}; the bed is supposed to be even"
        );
    }

    /// HSV saturation of a composited pixel, `0..=1`.
    fn sat(c: u32) -> f32 {
        let (mx, mn) = (max_channel(c), min_channel(c));
        if mx == 0 {
            0.0
        } else {
            (mx - mn) as f32 / mx as f32
        }
    }

    /// The bed's composite at the body's own cap on a theme.
    fn bed_at_cap(t: f32, fg: u32, bg: u32) -> u32 {
        let cov = UNDER_COV_CAP as u8;
        over_premul(
            bg,
            premul_rgb(bed_ink(spectrum(t), bed_luma_budget(fg)), cov),
            cov,
        )
    }

    // -- the black gaps (2026-09-08): chroma, not luminance ------------------

    #[test]
    fn the_bed_s_dark_stops_arrive_at_full_chroma_not_walked_grey() {
        // THE OWNER, 2026-09-08: "there are gaps black gaps in the rainbow a
        // few characters back from the cursor" — measured on Nord as indigo
        // composited `(82, 60, 136)`, S 0.61, next to a blue at 210–230. At
        // the bar's own luminance indigo's hue owns `(127, 0, 219)`; the old
        // walk toward white spent that chroma for nothing the bar wanted.
        for (name, fg, bg) in [
            ("Nord", NORD_FG, NORD_BG),
            ("default", DEFAULT_FG, DEFAULT_BG),
        ] {
            let budget = bed_luma_budget(fg);
            let indigo = bed_ink(0x004B_0082, budget);
            assert!(
                sat(indigo) >= 0.95 && max_channel(indigo) >= 195,
                "{name}: indigo's bed ink is #{indigo:06X} (S {:.2}, peak {}) — a grey lavender, not a violet",
                sat(indigo),
                max_channel(indigo)
            );
            // …at the bar's weight, not over it.
            assert!(
                relative_luminance(indigo) < budget && budget - relative_luminance(indigo) < 0.002,
                "{name}: indigo sits at Y {:.4} against a budget of {budget:.4}",
                relative_luminance(indigo)
            );
            // The whole arc, composited at the cap: no stop is walked greyer
            // than blue's own unavoidable share of white (blue's full value
            // is under the bar, so it alone takes white: S 0.81 on Nord).
            let (mut lo, mut lo_t) = (f32::INFINITY, 0.0f32);
            for i in 0..=512u32 {
                let t = i as f32 / 512.0;
                let s = sat(bed_at_cap(t, fg, bg));
                if s < lo {
                    lo = s;
                    lo_t = t;
                }
            }
            assert!(
                lo >= 0.78,
                "{name}: the bed's least saturated composite is S {lo:.3} at t {lo_t:.3} — the walk toward white is back"
            );
        }
        // The controls: the stops the bar itself prices are byte-identical to
        // the recipe this replaced (scale-down is scale-down).
        let budget = bed_luma_budget(DEFAULT_FG);
        for (stop, name) in [
            (0x00FF_FF00, "yellow"),
            (0x0000_FF00, "green"),
            (0x00FF_0000, "red"),
        ] {
            let old = solve_for_luma(|k| scale_rgb(stop, k), budget);
            assert_eq!(
                bed_ink(stop, budget),
                old,
                "{name} is priced by the bar alone and must not move"
            );
        }
        // Blue's full value is already blue: its white share is what it was.
        let old_blue = solve_for_luma(|w| toward_white(0x0000_00FF, w), budget);
        assert_eq!(
            bed_ink(0x0000_00FF, budget),
            old_blue,
            "blue was already at full value"
        );
    }

    #[test]
    fn the_crossing_composites_no_greyer_than_its_flanks() {
        // The other "gap": the green→blue crossing carried the arc's authored
        // S 0.53 (`SPECTRUM_CROSSING_ROOF`, the retired cyan census's bound)
        // and composited `(45, 92, 93)` on Nord — the greyest cell on a typed
        // line. `BED_SAT_FLOOR` gives the seam its neighbours' chroma back at
        // the bed's own read; the arc's hue pacing is untouched.
        use crate::spectrum::{spectrum_crossing_position, spectrum_crossing_width};
        let mid = spectrum_crossing_position();
        let half = spectrum_crossing_width();
        for (name, fg, bg) in [
            ("Nord", NORD_FG, NORD_BG),
            ("default", DEFAULT_FG, DEFAULT_BG),
        ] {
            let green = sat(bed_at_cap(mid - 2.0 * half, fg, bg));
            let blue = sat(bed_at_cap(mid + 2.0 * half, fg, bg));
            let mut worst = f32::INFINITY;
            let mut at = 0.0f32;
            let mut px = 0u32;
            for i in 0..=64u32 {
                let t = mid - half + 2.0 * half * i as f32 / 64.0;
                let c = bed_at_cap(t, fg, bg);
                if sat(c) < worst {
                    worst = sat(c);
                    at = t;
                    px = c;
                }
            }
            assert!(
                worst >= green.min(blue) - 0.02,
                "{name}: the crossing composites #{px:06X} (S {worst:.3}) at t {at:.3}, greyer than its flanks (green S {green:.3}, blue S {blue:.3})"
            );
            assert!(
                worst >= 0.90,
                "{name}: the crossing's least saturated composite is S {worst:.3} (#{px:06X}) — the roof's S 0.53 is showing through the bed"
            );
            // And the arc's hue through the seam is the table's own: the
            // floor moves S, never H.
            for i in 0..=16u32 {
                let t = mid - half + 2.0 * half * i as f32 / 16.0;
                let (h_arc, _, _) = crate::spectrum::spectrum_hsv(spectrum(t));
                let (h_ink, _, _) =
                    crate::spectrum::spectrum_hsv(bed_ink(spectrum(t), bed_luma_budget(fg)));
                assert!(
                    (h_arc - h_ink).abs() <= 2.5,
                    "{name}: at t {t:.3} the bed's hue is {h_ink:.1}° against the arc's {h_arc:.1}°"
                );
            }
        }
    }

    #[test]
    fn the_ground_never_lifts_the_bed_over_the_bar() {
        // A floor against the ground (`BED_GROUND_CONTRAST_MIN` 1.4:1,
        // 2026-09-09) was taken out: wherever it exceeded the bar it lifted
        // the bed over the luminance the text can bear. The budget is the
        // bar's answer alone, clamped to [`BED_LUMA_MIN`, `BED_LUMA_MAX`] —
        // on the themes the floor would have moved as much as on the ones it
        // would not.
        let bar = |fg: u32| {
            (relative_luminance(fg) + 0.05) / (BODY_CONTRAST_BAR + BODY_CONTRAST_GUARD) - 0.05
        };
        let one_dark = (0x00AB_B2BFu32, 0x0028_2C34u32);
        let solarized = (0x0083_9496u32, 0x0000_2B36u32);
        for (name, fg, bg) in [
            ("Nord", NORD_FG, NORD_BG),
            ("default", DEFAULT_FG, DEFAULT_BG),
            ("One Dark", one_dark.0, one_dark.1),
            ("One Dark on its alt ground", one_dark.0, 0x0032_3844),
            ("Solarized Dark", solarized.0, solarized.1),
            (
                "a #999999 foreground on Nord's ground",
                0x0099_9999,
                NORD_BG,
            ),
        ] {
            let budget = bed_luma_budget(fg);
            assert!(
                (budget - bar(fg).clamp(BED_LUMA_MIN, BED_LUMA_MAX)).abs() < 1e-6,
                "{name}: the budget {budget:.4} is not the bar's own answer {:.4}",
                bar(fg)
            );
            // The floor that was removed would have bound here: prove the
            // control — on One Dark and Solarized it would have lifted the
            // bed over the bar, and the text over it under 5.25:1.
            let floor = (relative_luminance(bg) + 0.05) * 1.4 - 0.05;
            if floor > budget {
                assert!(
                    worst_bed_contrast_at(fg, bg, floor) < BODY_CONTRAST_BAR,
                    "{name}: the control — a 1.4:1 ground floor ({floor:.4}) would not have broken the bar here"
                );
            }
            // …and the bar's own budget holds the bar wherever the bar is
            // obeyed at all: a foreground clamped up to `BED_LUMA_MIN`
            // (Solarized Dark's `#839496` solves to 0.012) is the documented
            // exception, and is under 5.25:1 by design.
            if budget > BED_LUMA_MIN {
                assert!(
                    worst_bed_contrast_at(fg, bg, budget) >= BODY_CONTRAST_BAR,
                    "{name}: the bar's own budget composites under the bar"
                );
            }
        }
        // One Dark's pair is the one the floor cost the most: 5.29:1 → 4.69:1
        // (4.74:1 on the sweep pin's own coarser grid).
        let (fg, bg) = one_dark;
        let floor = (relative_luminance(bg) + 0.05) * 1.4 - 0.05;
        assert!(
            worst_bed_contrast_at(fg, bg, floor) < 4.8,
            "the control: One Dark under the removed floor read {:.3}:1",
            worst_bed_contrast_at(fg, bg, floor)
        );
    }

    #[test]
    fn the_visibility_floor_costs_the_bar_nothing_on_nord() {
        // The chroma-first recipe changes what indigo, violet, the blue mids
        // and the crossing LOOK like; it must not change what the text over
        // them reads at — the bar is a function of Y alone and the solver
        // still answers from under it. Nord, every position, every level.
        let worst = worst_bed_contrast(NORD_FG, NORD_BG);
        assert!(
            worst >= BODY_CONTRAST_BAR,
            "Nord: the bed composited to {worst:.4}:1; the bar is {BODY_CONTRAST_BAR}:1"
        );
        assert!(
            worst - BODY_CONTRAST_BAR <= 0.10,
            "Nord: the bed's worst stop composites at {worst:.4}:1 — light left under the bar"
        );
        // …and the scanner's own colour floor (paint-conformance `SAT_MIN`
        // 60 / `SAT_SPREAD` 40) is cleared by every stop at the cap and at
        // the cold key's share of it.
        for cov in [UNDER_COV_CAP as u8, (UNDER_COV_CAP * BODY_COLD_SHARE) as u8] {
            for i in 0..=512u32 {
                let t = i as f32 / 512.0;
                let ink = bed_ink(spectrum(t), bed_luma_budget(NORD_FG));
                let lit = over_premul(NORD_BG, premul_rgb(ink, cov), cov);
                assert!(
                    max_channel(lit) >= 60 && max_channel(lit) - min_channel(lit) >= 40,
                    "Nord at cov {cov}, t {t:.3}: #{lit:06X} is under the scanner's colour floor"
                );
            }
        }
    }

    /// **THE INK TABLE** — not a pin: every anchor and the crossing's roof,
    /// the bed's ink before (the walk toward white from the stop itself) and
    /// after (`onto_luma` + `BED_SAT_FLOOR`) on Nord and the default theme,
    /// and the hot edge's ink the same way — the numbers the module doc and
    /// `RAINBOW-KITTY-V2.md` quote, printed from the code that makes them.
    ///
    /// `targo --unverified test -p aterm-effects --lib -- --ignored --nocapture the_bed_and_hot_edge_ink_table`
    #[test]
    #[ignore = "a table for the record, not a pin"]
    fn the_bed_and_hot_edge_ink_table() {
        use crate::spectrum::{SPECTRUM_ANCHORS, SPECTRUM_CROSSING_ROOF};
        let old_bed = |rgb: u32, budget: f32| {
            if relative_luminance(rgb) > budget {
                solve_for_luma(|k| scale_rgb(rgb, k), budget)
            } else {
                solve_for_luma(|w| toward_white(rgb, w), budget)
            }
        };
        let old_hot = |rgb: u32| {
            if relative_luminance(rgb) >= HOT_EDGE_LUMA_FLOOR {
                rgb
            } else {
                solve_for_luma(|w| toward_white(rgb, w), HOT_EDGE_LUMA_FLOOR)
            }
        };
        let rgb = |c: u32| {
            format!(
                "({:3},{:3},{:3})",
                (c >> 16) & 0xff,
                (c >> 8) & 0xff,
                c & 0xff
            )
        };
        let names = [
            "red", "orange", "yellow", "green", "blue", "indigo", "violet",
        ];
        let mut stops: Vec<(String, u32)> = names
            .iter()
            .zip(SPECTRUM_ANCHORS)
            .map(|(n, c)| ((*n).to_string(), c))
            .collect();
        for (i, &c) in SPECTRUM_CROSSING_ROOF.iter().enumerate() {
            stops.push((format!("roof[{i}]"), c));
        }
        println!();
        for (theme, fg, bg) in [
            ("Nord", NORD_FG, NORD_BG),
            ("default", DEFAULT_FG, DEFAULT_BG),
        ] {
            let budget = bed_luma_budget(fg);
            let cov = UNDER_COV_CAP as u8;
            println!("bed on {theme} (budget Y {budget:.4}):");
            println!(
                "  stop      arc                 old ink          S     @236 (peak)          new ink          S     @236 (peak)          Y new  fg/bed@236"
            );
            for (name, c) in &stops {
                let (o, n) = (old_bed(*c, budget), bed_ink(*c, budget));
                let (oc, nc) = (
                    over_premul(bg, premul_rgb(o, cov), cov),
                    over_premul(bg, premul_rgb(n, cov), cov),
                );
                println!(
                    "  {name:9} {} #{c:06X}  {} {:.2}  {} ({:3})  {} {:.2}  {} ({:3})  {:.4} {:.3}",
                    rgb(*c),
                    rgb(o),
                    sat(o),
                    rgb(oc),
                    max_channel(oc),
                    rgb(n),
                    sat(n),
                    rgb(nc),
                    max_channel(nc),
                    relative_luminance(n),
                    contrast(fg, nc)
                );
            }
        }
        println!("hot edge (floor Y {HOT_EDGE_LUMA_FLOOR:.2}):");
        println!("  stop      old ink   S     new ink   S");
        for (name, c) in &stops {
            let (o, n) = (old_hot(*c), hot_edge_ink(*c));
            println!(
                "  {name:9} #{o:06X}   {:.2}  #{n:06X}   {:.2}",
                sat(o),
                sat(n)
            );
        }
    }

    /// **THE WARM STOPS' FRONTIER** — not a pin: a table for the owner.
    ///
    /// Yellow, green and the crossing's cyan are at full chroma already; at
    /// the bar's luminance the sRGB transfer sets their peak channel (Nord:
    /// 90 / 102 / 98 ink, 87 / 98 / 95 composited). The only lever left is
    /// the bar, and that is a ruling. This prints, per stop and per step of
    /// the peak channel, what the foreground reads at over the composited
    /// bed at the body's cap and at the ledger's frame top, on Nord and the
    /// default theme, and names the pins each step would move.
    ///
    /// `targo --unverified test -p aterm-effects --lib -- --ignored --nocapture the_warm_stops_frontier_table`
    #[test]
    #[ignore = "a table for the owner's ruling, not a pin"]
    fn the_warm_stops_frontier_table() {
        type InkOf = fn(u32) -> u32;
        let stops: [(&str, InkOf); 3] = [
            ("yellow", |p| (p << 16) | (p << 8)),
            ("green", |p| p << 8),
            ("cyan", |p| (p << 8) | p),
        ];
        let themes = [
            ("Nord", NORD_FG, NORD_BG),
            ("default", DEFAULT_FG, DEFAULT_BG),
        ];
        println!();
        println!(
            "bar {BODY_CONTRAST_BAR}:1 (+{BODY_CONTRAST_GUARD} guard); fg over the composited bed, at cov {} (the body's cap) and {} (the frame top)",
            UNDER_COV_CAP as u32, BODY_FRAME_TOP as u32
        );
        for (name, ink_of) in stops {
            for (theme, fg, bg) in themes {
                let budget = bed_luma_budget(fg);
                let at_bar = match name {
                    "yellow" => bed_ink(0x00FF_FF00, budget),
                    "green" => bed_ink(0x0000_FF00, budget),
                    _ => bed_ink(0x0000_FFFF, budget),
                };
                let p0 = max_channel(at_bar);
                println!();
                println!(
                    "{name} on {theme}: the bar's own ink is #{at_bar:06X} (peak {p0}); one level per row for the first six, then five"
                );
                println!(
                    "  ink peak | composited@236 (peak) | fg/bed @236 | fg/bed @251 | Y ink  | pins that move"
                );
                let mut p = p0;
                while p <= p0 + 45 && p <= 255 {
                    let ink = ink_of(p);
                    let cap = UNDER_COV_CAP as u8;
                    let top = BODY_FRAME_TOP as u8;
                    let c236 = over_premul(bg, premul_rgb(ink, cap), cap);
                    let c251 = over_premul(bg, premul_rgb(ink, top), top);
                    let (r236, r251) = (contrast(fg, c236), contrast(fg, c251));
                    let mut moves = Vec::new();
                    if r251 < BODY_CONTRAST_BAR {
                        moves.push("letters_stay_legible_* (5.25:1 at the frame top)");
                    }
                    if r236 < BODY_CONTRAST_BAR {
                        moves.push("…and at the body's cap");
                    }
                    if theme == "default"
                        && name == "yellow"
                        && !(79..=81).contains(&max_channel(c236))
                    {
                        moves.push("the_dimmest_stop_… (79..=81)");
                    }
                    if relative_luminance(ink) - budget > 0.004 {
                        moves.push("every_stop_of_the_arc_composites_at_one_weight (±0.004 Y)");
                    }
                    println!(
                        "  {p:8} | #{c236:06X} ({:3})        | {r236:8.3}    | {r251:8.3}    | {:.4} | {}",
                        max_channel(c236),
                        relative_luminance(ink),
                        if moves.is_empty() {
                            "none".to_string()
                        } else {
                            moves.join("; ")
                        }
                    );
                    p += if p < p0 + 6 { 1 } else { 5 };
                }
            }
        }
        // THE GROUND. The bed's budget is the bar's answer against the
        // foreground; how far it sits over the theme's own page is not a
        // law (a 1.4:1 floor was tried on 2026-09-09 and removed — it
        // overrode the bar wherever it exceeded it). Per theme: the bar's
        // bed over the ground, and what lifting the bed to 1.4:1 over the
        // ground would cost the text over it.
        println!();
        println!(
            "the bed over its ground (no law — a ruling): bar's bed / ground, and the cost of a 1.4:1 lift"
        );
        println!(
            "  theme                     fg      bg      budget  bed/ground  fg/bed worst | lifted to  fg/bed worst"
        );
        let one_dark = (0x00AB_B2BFu32, 0x0028_2C34u32);
        for (theme, fg, bg) in [
            ("Nord", NORD_FG, NORD_BG),
            ("default", DEFAULT_FG, DEFAULT_BG),
            ("One Dark", one_dark.0, one_dark.1),
            ("One Dark (alt ground)", one_dark.0, 0x0032_3844),
            ("Solarized Dark", 0x0083_9496, 0x0000_2B36),
            ("Gruvbox Dark", 0x00EB_DBB2, 0x0028_2828),
            ("Dracula", 0x00F8_F8F2, 0x0028_2A36),
            ("Tokyo Night", 0x00C0_CAF5, 0x001A_1B26),
        ] {
            let budget = bed_luma_budget(fg);
            let over = (budget + 0.05) / (relative_luminance(bg) + 0.05);
            let lifted = ((relative_luminance(bg) + 0.05) * 1.4 - 0.05).max(budget);
            println!(
                "  {theme:24} #{fg:06X} #{bg:06X} {budget:.4}  {over:6.3}:1  {:8.3}:1 | {lifted:.4}     {:8.3}:1{}",
                worst_bed_contrast_at(fg, bg, budget),
                worst_bed_contrast_at(fg, bg, lifted),
                if lifted > budget + 1e-6 {
                    "  (lift would bind)"
                } else {
                    "  (bar already above 1.4:1)"
                }
            );
        }
    }

    // -- §18: the field index ---------------------------------------------

    #[test]
    fn the_field_index_answers_what_a_newest_first_scan_would() {
        // Two rows, driven through the public API only, across the frames in
        // which a cell is genuinely SHADOWED (a jump abandons a cohort and the
        // hand retypes over it while it is still draining), a row goes DARK
        // and is RELIT — the lane prune and re-mint in `FieldIndex::begin`
        // that a one-row, one-frame pin never exercises.
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        let scan_matches = |rib: &Ribbon, ms: u64| {
            for row in 0..6u16 {
                for col in 0..30u16 {
                    let scan = rib
                        .cells()
                        .iter()
                        .rev()
                        .find(|l| l.row == row && l.col == col)
                        .map(|l| l.t);
                    let idx = rib.field_at(row, col);
                    match (scan, idx) {
                        (Some(a), Some(b)) => assert!(
                            (a - b).abs() < 1e-6,
                            "+{ms} ms: index says {b} at ({row},{col}); a newest-first scan says {a}"
                        ),
                        (None, None) => {}
                        (a, b) => {
                            panic!(
                                "+{ms} ms: index and scan disagree at ({row},{col}): {a:?} vs {b:?}"
                            )
                        }
                    }
                }
            }
        };
        let mut script: Vec<(u64, (u16, u16), Event)> = Vec::new();
        // Row 2, cols 10..17.
        for i in 0..8u16 {
            script.push((u64::from(i) * 60, (2, 11 + i), typed()));
        }
        // A jump away and back abandons that cohort; retyping cols 13 and 14
        // lays a NEW cohort over cells that are still in the pool, draining.
        script.push((500, (2, 40), nav((2, 18), (2, 40))));
        script.push((500, (2, 14), nav((2, 40), (2, 14))));
        script.push((500, (2, 14), typed()));
        script.push((560, (2, 15), typed()));
        // A jump to row 3 abandons row 2 entirely; row 3 is laid.
        script.push((900, (3, 10), nav((2, 15), (3, 10))));
        for i in 0..6u16 {
            script.push((1000 + u64::from(i) * 60, (3, 11 + i), typed()));
        }
        // Row 2 has gone dark by now; relight it.
        script.push((1600, (2, 21), nav((3, 16), (2, 21))));
        for i in 0..3u16 {
            script.push((1600 + u64::from(i) * 60, (2, 21 + i), typed()));
        }
        let mut caret = (2u16, 10u16);
        let (mut saw_shadow, mut saw_dark_row_2, mut saw_relit_row_2) = (false, false, false);
        for ms in (0..=2400u64).step_by(20) {
            let now = at(t0, ms);
            for (_, c_at, ev) in script.iter().filter(|(t, _, _)| *t == ms) {
                caret = *c_at;
                let cx = ctx(now, &c, caret, 0.6);
                rib.on_event(ev, now, &cx);
            }
            let cx = ctx(now, &c, caret, 0.6);
            rib.plan(&cx);
            scan_matches(&rib, ms);
            let cells = rib.cells();
            saw_shadow |= cells.iter().filter(|l| (l.row, l.col) == (2, 13)).count() >= 2;
            let row_2_dark = !cells.is_empty() && cells.iter().all(|l| l.row != 2);
            saw_dark_row_2 |= row_2_dark;
            saw_relit_row_2 |= saw_dark_row_2 && cells.iter().any(|l| l.row == 2);
        }
        assert!(
            saw_shadow,
            "the script must put a genuinely shadowed cell in the pool"
        );
        assert!(saw_dark_row_2, "the script must take row 2 dark");
        assert!(saw_relit_row_2, "…and relight it");
    }

    // -- §4 / C2: the classic walk -----------------------------------------

    #[test]
    fn the_walk_lays_a_sixteenth_per_cell_for_sixteen_cells_then_a_thirty_sixth() {
        assert!(walk_t(0.0).abs() < 1e-6);
        assert!((walk_t(1.0) - 1.0 / WALK_FAST_CELLS).abs() < 1e-6);
        assert!((walk_t(16.0) - 1.0).abs() < 1e-6);
        assert!((walk_t(17.0) - walk_t(16.0) - WALK_LAY_RATE).abs() < 1e-6);
        assert!((walk_t(52.0) - 2.0).abs() < 1e-6);
        assert!(
            (walk_t(-2.0) + 2.0 / WALK_FAST_CELLS).abs() < 1e-6,
            "a reach cell continues the fast phase backwards"
        );
    }

    #[test]
    fn the_walk_is_a_function_of_position_so_a_retyped_cell_takes_back_its_stop() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        // Cols 2..41; the caret ends at 42; the cohort's origin is col 2.
        type_run(&mut rib, t0, 2, 40, &c, 0.7);
        let mut ms = 40 * 60;
        for caret in [41u16, 40, 39] {
            erase_at(&mut rib, at(t0, ms), (2, caret), &c);
            ms += 60;
        }
        // §4's retract is a SUFFIX: every cell at or right of the caret is
        // leaving, nothing left of it is, and nothing in the middle is gone.
        for cell in rib.cells() {
            assert_eq!(
                cell.retract_at.is_some(),
                cell.col >= 39,
                "col {} is on the wrong side of the backspace",
                cell.col
            );
        }
        assert!(
            (2..39u16).all(|col| rib.field_at(2, col).is_some()),
            "the lit run must stay contiguous from the caret backward"
        );
        // Retype the three, then one more in the MIDDLE of the word.
        for caret in [40u16, 41, 42, 21] {
            let now = at(t0, ms);
            let cx = ctx(now, &c, (2, caret), 0.7);
            rib.on_event(&typed(), now, &cx);
            rib.plan(&cx);
            ms += 60;
        }
        for cell in rib.cells().iter().filter(|l| l.retract_at.is_none()) {
            let want = walk_t(f32::from(cell.col) - 2.0);
            assert!(
                (cell.t - want).abs() < 1e-5,
                "col {} carries t = {} but the walk at its column is {want}",
                cell.col,
                cell.t
            );
        }
        for col in [20u16, 39, 40, 41] {
            let owners: Vec<&Cell> = rib.cells().iter().filter(|l| l.col == col).collect();
            assert_eq!(
                owners.len(),
                1,
                "col {col} must have exactly one owner after a retype"
            );
            assert!(owners[0].retract_at.is_none(), "…and it is live again");
        }
    }

    #[test]
    fn the_reach_extends_its_own_cohort_and_continues_its_own_walk() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        // A: cols 10..17. B: cols 40..41, two cells short of four letters, so
        // at its second reach beat it wants cols 39 and 38 — with A still on
        // the row to its left.
        type_run(&mut rib, t0, 10, 8, &c, 0.8);
        let keys = Keys {
            g: geom(),
            row: 2,
            col0: 40,
            n: 2,
            period_ms: 60,
            disp: 0.8,
        };
        type_keys(&mut rib, at(t0, 1000), keys, &c);
        // Two reach beats in: the rest (0.90 s at the floor) plus 0.10 s.
        let now = at(t0, 1060 + (PHRASE_REST_MIN_S * 1000.0) as u64 + 100);
        let cx = ctx(now, &c, (2, 42), 0.0);
        rib.plan(&cx);
        assert!(
            rib.cells().iter().any(|l| l.row == 2 && l.col < 18),
            "A must still be on the row for the pin to mean anything"
        );
        let b_id = rib
            .cells()
            .iter()
            .find(|l| l.col == 40)
            .expect("B's first cell")
            .cohort;
        let t40 = rib.field_at(2, 40).expect("B's field");
        for (col, back) in [(39u16, 1.0f32), (38, 2.0)] {
            let cell = rib
                .cells()
                .iter()
                .find(|l| l.row == 2 && l.col == col)
                .unwrap_or_else(|| panic!("the reach must have laid col {col}"));
            assert_eq!(
                cell.cohort, b_id,
                "the reach cell at {col} joined the wrong cohort"
            );
            assert!(!cell.typing, "a reach cell is not typing light");
            assert!(
                (cell.t - (t40 - back / WALK_FAST_CELLS)).abs() < 1e-5,
                "the reach at {col} must continue B's own walk backwards"
            );
        }
    }

    #[test]
    fn the_wave_breathes_a_tenth_of_a_cell_at_full_momentum_inside_the_body_s_own_rows() {
        // THE OWNER, 2026-09-08: "make this rainbow theme truly magical and
        // special and dynamic and beautiful" — the live-momentum wave was
        // v1's `0.055 ch · disp`, at most 1.5 px at retina; it is doubled so
        // the ribbon visibly breathes with speed, still inside the rows the
        // body already owns: the top edge never rises above `TALL_UP_CH` and
        // the bottom never falls past `DN_TOP_CH`, at any phase.
        // RE-PINNED 2026-09-13 for the COMET: the body's bottom at full
        // momentum is the comet's lobe under the hand (`COMET_DN_HAND_CH`
        // 0.55), settling to the floor behind it; the flat spelling keeps
        // the 0.36 it had. The top bound and the swing are unchanged.
        for (c, dn_ch) in [
            (cfg(true, true), COMET_DN_HAND_CH),
            (flat(true, true), DN_TOP_CH),
        ] {
            let t0 = Instant::now();
            let mut rib = Ribbon::new();
            type_run(&mut rib, t0, 10, 24, &c, 1.0);
            let g = geom();
            let ch = g.ch as f32;
            let rest = f32::from(g.origin_y) + 3.0 * ch;
            let mut deepest = 0.0f32;
            for k in 0..16u32 {
                let mut cx = ctx(at(t0, 23 * 60 + 8), &c, (2, 34), 1.0);
                cx.phase = k as f32 / 16.0;
                rib.plan(&cx);
                for col in 10..34u16 {
                    let band = rib.band(2, col).expect("a laid cell has a band");
                    deepest = deepest.max((band.spine - rest).abs());
                    assert!(
                        band.top >= rest - TALL_UP_CH * ch - 0.5,
                        "the wave lifted the body's top past its own reach at col {col}, phase {k}/16 ({})",
                        band.top
                    );
                    assert!(
                        band.bottom <= rest + dn_ch * ch + 0.5,
                        "the wave dropped the body's bottom past its own reach at col {col}, phase {k}/16 ({}; flat {})",
                        band.bottom,
                        c.ribbon_flat
                    );
                }
            }
            assert!(
                deepest >= 0.10 * ch,
                "the wave's deepest swing at full momentum is {deepest:.2} px ({:.3} ch); the owner asked for a ribbon that visibly breathes with speed — `WAVE_AMP_CELLS` 0.110",
                deepest / ch
            );
        }
    }

    // -- §4.1: the hot edge ------------------------------------------------

    #[test]
    fn the_hot_edge_lives_only_at_the_head_on_a_dark_ground() {
        let dark = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 10, 8, &dark, 0.9);
        let g = geom();
        let caret_top = f32::from(g.origin_y) + 2.0 * g.ch as f32;
        let caret_bot = caret_top + g.ch as f32;
        let reach_y = HOT_EDGE_CARET_REACH_CH * g.ch as f32;
        let check = |out: &[GlowQuad], head_col: u16, label: &str| {
            assert!(!out.is_empty(), "{label}: a hot hand must leave a hot edge");
            let head_x = f32::from(g.origin_x) + f32::from(head_col) * g.cw as f32;
            for q in out {
                assert!(
                    u32::from(q.alpha) == 0,
                    "{label}: the hot edge is ADDITIVE light; it may not carry a source-over alpha"
                );
                let x = f32::from(q.x);
                assert!(
                    x <= head_x + 1.0,
                    "{label}: the hot edge ran PAST the head cell (x = {x}, head = {head_x})"
                );
                assert!(
                    head_x - x <= HOT_EDGE_CELLS_MAX * g.cw as f32 + f32::from(q.w) + 1.0,
                    "{label}: the hot edge reached further than {HOT_EDGE_CELLS_MAX} cells behind the head"
                );
                let y = f32::from(q.y);
                assert!(
                    (caret_top - reach_y..=caret_bot + reach_y).contains(&y),
                    "{label}: the hot edge left the caret's own row band (y = {y})"
                );
            }
            // The peak request is `HOT_EDGE_COV_MAX`; the beam's transverse AA
            // can only lower it, and the lifted stop's brightest channel is
            // 255, so its premultiplied peak is the coverage itself.
            assert!(
                peak_channel(out) as f32 <= HOT_EDGE_COV_MAX + 1.0,
                "{label}: hot-edge coverage is over the {HOT_EDGE_COV_MAX} request ceiling"
            );
        };
        let mut sink = Sink::default();
        let cx = ctx(at(t0, 8 * 60), &dark, (2, 18), 0.9);
        rib.plan(&cx);
        {
            let mut f = sink.frame();
            rib.emit(&cx, &mut f);
        }
        check(&sink.out, 18, "typing");
        // A BACKSPACE FRAME (§4.1 "never lags the head", §20.1 "ends at the
        // head cell every frame"): three erased cells are still draining to
        // the right of the caret; the edge ends at the caret's own cell.
        let mut ms = 8 * 60;
        for caret in [17u16, 16, 15] {
            erase_at(&mut rib, at(t0, ms), (2, caret), &dark);
            ms += 60;
        }
        let cx = ctx(at(t0, ms), &dark, (2, 15), 0.9);
        rib.plan(&cx);
        {
            let mut f = sink.frame();
            rib.emit(&cx, &mut f);
        }
        assert!(
            rib.cells().iter().any(|l| l.col >= 15),
            "the erased cells must still be draining for this frame to test anything"
        );
        check(&sink.out, 15, "backspace");
        // …and once the whole word is erased nothing at the hand is wet.
        for caret in [14u16, 13, 12, 11, 10] {
            erase_at(&mut rib, at(t0, ms), (2, caret), &dark);
            ms += 60;
        }
        let cx = ctx(at(t0, ms), &dark, (2, 10), 0.9);
        rib.plan(&cx);
        {
            let mut f = sink.frame();
            rib.emit(&cx, &mut f);
        }
        assert!(
            !rib.cells().is_empty() && !sink.under.is_empty(),
            "the draining cells must still be on glass for this frame to test anything"
        );
        assert!(
            sink.out.is_empty(),
            "a word that is entirely draining has no wet head; the hot edge sat on erased cells"
        );
    }

    /// **THE SURGE** (§23's addendum "Flow state", 2026-09-09) — a key born
    /// on a CHANGE OF SPEED wears a LONGER crisp edge, and an open theme
    /// raises the floor to the same ceiling. The price first
    /// ([`edge_cells`]), then the pixels: the hairline of a surged head
    /// reaches further behind the hand than a steady head's, and neither
    /// reaches past [`HOT_EDGE_CELLS_MAX`].
    ///
    /// The alpha law and the transient cap are untouched by it — the same
    /// normalised falloff over a longer `d`, so a surged edge is LONGER and
    /// never brighter: the peak coverage is `HOT_EDGE_COV_MAX` either way.
    #[test]
    fn the_surge_stretches_the_edge_on_a_change_of_speed() {
        // -- 1. the price: `3 + 4·Δ/0.5`, capped at 5 -----------------------
        assert!(
            (edge_cells(0.0, 0.0) - HOT_EDGE_CELLS).abs() < 1e-6,
            "no surge and no flow is the constant the theme had before flow"
        );
        // The spine hands the rise over normalised by `SURGE_FULL_RISE`; the
        // gate rise is 0.15 of it, and `3 + 4·0.15/0.5 = 4.2`.
        let gate = SURGE_MIN_RISE / SURGE_FULL_RISE;
        assert!(
            (edge_cells(gate, 0.0) - (HOT_EDGE_CELLS + 4.0 * SURGE_MIN_RISE / 0.5)).abs() < 1e-5,
            "the gate rise buys {} cells, want 3 + 4·0.15/0.5",
            edge_cells(gate, 0.0)
        );
        assert!(
            (edge_cells(1.0, 0.0) - HOT_EDGE_CELLS_MAX).abs() < 1e-6,
            "the full rise buys the whole stretch"
        );
        assert!(
            (edge_cells(0.0, 1.0) - HOT_EDGE_CELLS_MAX).abs() < 1e-6,
            "an OPEN theme raises the floor from 3 to 5 with no surge at all"
        );
        assert!(
            edge_cells(0.0, 0.5) > HOT_EDGE_CELLS && edge_cells(0.0, 0.5) < HOT_EDGE_CELLS_MAX,
            "the floor LERPS as the theme opens; it does not snap"
        );

        // -- 2. the pixels: how far the hairline actually reaches -----------
        let dark = cfg(true, true);
        let g = geom();
        // The furthest a hot-edge quad sits behind the head, in cells, for a
        // ten-key run whose LAST key was born with `surge`.
        let reach_of = |surge: f32, heat: f32| -> f32 {
            let t0 = Instant::now();
            let mut rib = Ribbon::new();
            for i in 0..10u16 {
                let now = at(t0, u64::from(i) * 60);
                let mut cx = ctx_in(now, &dark, (2, 9 + i), 0.9, g);
                if i == 9 {
                    cx.surge = surge;
                    cx.flow.heat = heat;
                }
                rib.on_event(&typed(), now, &cx);
                rib.plan(&cx);
            }
            let cx = ctx_in(at(t0, 9 * 60), &dark, (2, 18), 0.9, g);
            rib.plan(&cx);
            let mut sink = Sink::default();
            {
                let mut f = sink.frame();
                rib.emit(&cx, &mut f);
            }
            assert!(!sink.out.is_empty(), "a hot hand must leave a hot edge");
            let head_x = f32::from(g.origin_x) + 18.0 * g.cw as f32;
            let far = sink
                .out
                .iter()
                .map(|q| head_x - f32::from(q.x))
                .fold(0.0f32, f32::max);
            far / g.cw as f32
        };
        let steady = reach_of(0.0, 0.0);
        let surged = reach_of(1.0, 0.0);
        let flowing = reach_of(0.0, 1.0);
        assert!(
            surged > steady + 0.9,
            "a surged head reaches {surged:.2} cells behind the hand, a steady one {steady:.2} — the stretch is not on the glass"
        );
        assert!(
            (flowing - surged).abs() < 0.01,
            "an open theme reaches the same ceiling as a full surge ({flowing:.2} vs {surged:.2})"
        );
        assert!(
            surged <= HOT_EDGE_CELLS_MAX,
            "the stretched edge ran past its own ceiling ({surged:.2} cells)"
        );
        assert!(
            steady <= HOT_EDGE_CELLS,
            "a steady head must reach exactly what it always did ({steady:.2} cells)"
        );
    }

    /// A cold hand carries the hot edge at its FLOOR and a hot one the whole
    /// of it (2026-09-14: the edge's existence no longer waits for momentum —
    /// see [`HOT_EDGE_GAIN_FLOOR`]); a light ground carries none at all.
    #[test]
    fn a_cold_hand_carries_the_edge_s_floor_and_a_light_ground_carries_no_hot_edge() {
        let dark = cfg(true, true);
        let light = cfg(false, true);
        let cold = Ribbon::hot_edge_gain(&dark, 0.0, true);
        assert!(
            (cold - HOT_EDGE_GAIN_FLOOR).abs() < 1e-6,
            "the first key of a word gets the attach: {cold} against the floor {HOT_EDGE_GAIN_FLOOR}"
        );
        // The floor's VALUE, not only its arithmetic: with the floor at
        // 0.0 every line above still holds (review, 2026-09-14).
        assert!(
            HOT_EDGE_GAIN_FLOOR > 0.0 && cold > 0.0,
            "the edge EXISTS on a cold hand: floor {HOT_EDGE_GAIN_FLOOR}, cold gain {cold}"
        );
        assert!(
            (Ribbon::hot_edge_gain(&dark, HOT_EDGE_DISP_MIN - 0.01, true) - HOT_EDGE_GAIN_FLOOR)
                .abs()
                < 1e-6,
            "under the disp floor the gain is the floor and nothing more"
        );
        assert!(Ribbon::hot_edge_gain(&dark, 0.9, true) > HOT_EDGE_GAIN_FLOOR + 0.4);
        assert!((Ribbon::hot_edge_gain(&dark, 1.0, true) - 1.0).abs() < 1e-6);
        // A WAKE's head is no key: no floor, the ramp it always had.
        assert!(
            Ribbon::hot_edge_gain(&dark, 0.0, false) <= 0.0,
            "a cold arrow's wake carries no hairline"
        );
        assert!((Ribbon::hot_edge_gain(&dark, 1.0, false) - 1.0).abs() < 1e-6);
        assert!(
            Ribbon::hot_edge_gain(&light, 1.0, true) <= 0.0,
            "additive white on a paper ground is exactly what L6 forbids"
        );
    }

    #[test]
    fn the_hot_edge_is_the_stop_under_it_lifted_hot_and_brighter_than_the_bed() {
        // THE OWNER, 2026-09-08: "I want the meteor to have rainbow! be a
        // bigger more special rainbow impact!" — "make this rainbow theme
        // truly magical and special and dynamic and beautiful". The hairline
        // at the hand was `#FFFFFF`, "a third white idiom". It is now the
        // spectrum: the stop under each vertex, lifted toward white to
        // `HOT_EDGE_LUMA_FLOOR` (`hot_edge_ink`) so it out-shines any bed it
        // can sit on, at `HOT_EDGE_COV_MAX` 118 — the transient cap (was 38).
        use aterm_render::add_sat;
        let dark = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 10, 8, &dark, 0.9);
        let cx = ctx(at(t0, 8 * 60), &dark, (2, 18), 0.9);
        rib.plan(&cx);
        let mut sink = Sink::default();
        {
            let mut f = sink.frame();
            rib.emit(&cx, &mut f);
        }
        let head = sink
            .out
            .iter()
            .max_by_key(|q| max_channel(q.color))
            .expect("a hot hand leaves a hot edge");
        assert!(
            max_channel(head.color) - min_channel(head.color) >= 8,
            "the hot edge is white (#{:06X}) — the owner asked for the rainbow at the hand",
            head.color
        );
        let dominant = |c: u32| {
            let (r, g, b) = ((c >> 16) & 0xff, (c >> 8) & 0xff, c & 0xff);
            if r >= g && r >= b {
                'r'
            } else if g >= b {
                'g'
            } else {
                'b'
            }
        };
        let t = rib.field_at(2, 17).expect("the head cell is laid");
        let stop = spectrum(clamp01(tri(t)));
        assert_eq!(
            dominant(head.color),
            dominant(stop),
            "the hairline's hue (#{:06X}) is not the head's stop (#{stop:06X})",
            head.color
        );
        // BRIGHTER THAN THE BED it rides: the head's stop at the bed's
        // ceiling, plus the hairline's premultiplied light on top.
        let cov = UNDER_COV_CAP as u8;
        let bed = over_premul(
            DEFAULT_BG,
            premul_rgb(bed_ink(stop, bed_luma_budget(DEFAULT_FG)), cov),
            cov,
        );
        let lit = add_sat(bed, head.color);
        assert!(
            relative_luminance(lit) >= 1.5 * relative_luminance(bed),
            "the hot edge over its bed (#{lit:06X}, Y {:.3}) is not brighter than the bed (#{bed:06X}, Y {:.3})",
            relative_luminance(lit),
            relative_luminance(bed)
        );
        // …and priced for EMPHASIS: past the 38 the white hairline was held
        // to. The hairline's 1-px anti-aliasing puts two thirds of a request
        // on its brighter row at this fixture (25 of 38 before), so the pin
        // is on the emitted peak, past the old ceiling.
        let peak = peak_channel(&sink.out);
        assert!(
            peak > 39,
            "the hairline's brightest emitted level is {peak}; it was capped at 38 until 2026-09-08 and the owner asked for more"
        );
    }

    // -- the measured "keeps brightening" defect ---------------------------

    #[test]
    fn the_body_never_brightens_after_the_last_keystroke() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 10, 10, &c, 0.9);
        let last = at(t0, 9 * 60);
        let mut sink = Sink::default();
        let mut peak_prev = 255u8;
        let mut edge_first: Option<u32> = None;
        // Start past the head cell's ONE sanctioned ramp-in (T3's 18 ms
        // `edge-in`); everything after it must only ever spend light. The
        // spine is handed a value that keeps CLIMBING, which is exactly the
        // condition that produced the measured 660 ms of after-glow.
        //
        // The body's law is stated on the REQUEST — the coverage the ribbon
        // asks for at each cell boundary — because that is what "the body's
        // light" means. The rasterized peak still moves by a level as the
        // wave's sub-pixel phase settles, and forbidding THAT would forbid the
        // wave; what may not move is the light the mark was priced at. The hot
        // edge's law is stated on what it EMITS, since it is additive and has
        // no request of its own on the plan.
        for ms in (20..1400).step_by(8) {
            let now = at(last, ms);
            let disp = (0.4 + 0.6 * (ms as f32 / 700.0)).min(1.0);
            let cx = ctx(now, &c, (2, 20), disp);
            rib.plan(&cx);
            let mut f = sink.frame();
            rib.emit(&cx, &mut f);
            let peak = plan_peak(&rib);
            assert!(
                peak <= peak_prev,
                "the body brightened at +{ms} ms with no keystroke behind it ({peak_prev} → {peak})"
            );
            peak_prev = peak;
            let lit = sink.under.iter().map(|q| q.alpha).max().unwrap_or(0);
            assert!(
                f32::from(lit) <= BODY_FRAME_TOP,
                "the composited body passed the ledger's frame top at +{ms} ms ({lit})"
            );
            // The head vertex is pinned (`WAVE_HEAD_PIN`), so the hairline's
            // brightest slab is byte-static; the vertex one cell behind rides
            // a quarter of the wave's ~1 px, which the AA can move by a few
            // levels. A gain read off the live spine climbs by a factor of
            // ten here, not by four levels.
            let edge = peak_channel(&sink.out);
            let first = *edge_first.get_or_insert(edge);
            assert!(
                edge <= first + 4,
                "the hot edge brightened at +{ms} ms with no keystroke behind it ({first} → {edge})"
            );
        }
        assert!(
            edge_first.is_some_and(|e| e > 0),
            "the hot edge must have been lit for its law to have been tested"
        );
    }

    // -- §4: the exit swoosh -----------------------------------------------

    #[test]
    fn the_exit_swoosh_retracts_toward_the_caret_and_reaches_exactly_zero() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 10, 8, &c, 0.8);
        let last = at(t0, 7 * 60);
        let caret = (2u16, 18u16);
        let mut sink = Sink::default();
        let mut left_prev = f32::NEG_INFINITY;
        let mut edge_prev = u32::MAX;
        let mut edge_first: Option<u32> = None;
        let retract_start = RETRACT_START_S * 1000.0;
        let total = (SWOOSH_TOTAL_S * 1000.0).round() as u64;
        for ms in ((retract_start as u64)..total + 20).step_by(10) {
            let now = at(last, ms);
            let cx = ctx(now, &c, caret, 0.0);
            rib.plan(&cx);
            let mut f = sink.frame();
            rib.emit(&cx, &mut f);
            // THE HOT EDGE GOES OUT WITH ITS BODY. It is the head's property
            // and takes the head cell's envelope, so through the retract it
            // may only fall — the slab-mid sampling of a collapsing segment
            // can move the rasterized peak by a level, and no more — and it
            // is at exactly zero before the cells retire. A hairline priced
            // from `birth_disp` alone held at 38 over a spent body here and
            // snapped off with the pool at 1.54 s.
            let edge = peak_channel(&sink.out);
            edge_first.get_or_insert(edge);
            assert!(
                edge <= edge_prev.saturating_add(2),
                "the hot edge brightened inside the swoosh at +{ms} ms ({edge_prev} → {edge})"
            );
            edge_prev = edge;
            if ms == total - 90 {
                let first = edge_first.unwrap_or(0);
                assert!(
                    !sink.under.is_empty() && edge * 4 < first,
                    "mid-fade the hot edge must be well under way with its body ({first} → {edge})"
                );
            }
            if ms == total - 10 {
                assert!(
                    !rib.cells().is_empty(),
                    "the cells must still be in the pool for the pin to mean anything"
                );
                assert_eq!(
                    edge, 0,
                    "10 ms before the pool clears the hot edge must already be at exactly zero"
                );
            }
            let Some(left) = sink.under.iter().map(|q| f32::from(q.x)).reduce(f32::min) else {
                continue;
            };
            assert!(
                left + 1.0 >= left_prev,
                "the retract moved AWAY from the caret at +{ms} ms ({left_prev} → {left})"
            );
            left_prev = left;
        }
        assert!(
            edge_first.is_some_and(|e| e > 0),
            "the hot edge must have been lit at the retract's start for its law to have been tested"
        );
        let now = at(last, (SWOOSH_TOTAL_S * 1000.0) as u64 + 40);
        let cx = ctx(now, &c, caret, 0.0);
        rib.plan(&cx);
        let mut f = sink.frame();
        rib.emit(&cx, &mut f);
        assert!(
            sink.under.is_empty() && sink.out.is_empty(),
            "the swoosh must reach EXACTLY zero, not a residue"
        );
        assert!(rib.at_rest(), "and the pools must be empty with it");
    }

    #[test]
    fn the_retract_drains_the_tail_before_the_head() {
        // §4's drain "picks cells farthest-from-the-head first", which is what
        // makes the ending read as the mark being drawn back INTO the caret.
        // Reading the mark the other way round is the same arithmetic and
        // looks like the ribbon walking away from the hand.
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 10, 8, &c, 0.8);
        let last = at(t0, 7 * 60);
        let start = RETRACT_START_S;
        let now = at(last, ((start + RETRACT_DUR_S * 0.5) * 1000.0) as u64);
        let cx = ctx(now, &c, (2, 18), 0.0);
        rib.plan(&cx);
        let plan = rib.plan_segments();
        let slabs = rib.slabs_per_cell();
        let n = plan.len();
        assert!(n > 2 * slabs, "the mark must still be planned");
        let tail = plan[slabs].cov;
        let head = plan[n - 1 - slabs].cov;
        assert!(
            tail < head,
            "the retract drained the HEAD first (tail {tail}, head {head})"
        );
    }

    #[test]
    fn a_cohort_still_being_typed_keeps_its_boundaries_while_its_row_mate_retracts() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        // A: cols 10..17. A real jump on the same row abandons it into its
        // retract (§4); the hand then lays B beside it while A is moving.
        // A PTY-licensed jump (no credit): it abandons and lays no wake, so
        // A is a retracting row-mate and B a cohort of its own — a Nav jump
        // would take A over into its wake and B would join that (R5).
        type_run(&mut rib, t0, 10, 8, &c, 0.8);
        let now = at(t0, 1000);
        let cx = ctx(now, &c, (2, 40), 0.8);
        rib.on_event(&pty((2, 18), (2, 40)), now, &cx);
        rib.plan(&cx);
        let keys = Keys {
            g: geom(),
            row: 2,
            col0: 40,
            n: 6,
            period_ms: 60,
            disp: 0.8,
        };
        type_keys(&mut rib, at(t0, 1000), keys, &c);
        let now = at(t0, 1320);
        let cx = ctx(now, &c, (2, 46), 0.8);
        rib.plan(&cx);
        let cw = geom().cw as f32;
        let (a, b): (Vec<&Segment>, Vec<&Segment>) =
            rib.plan_segments().iter().partition(|s| s.x < 30.0 * cw);
        assert!(
            !a.is_empty() && !b.is_empty(),
            "both cohorts must be planned"
        );
        let a_left = a.iter().map(|s| s.x).fold(f32::INFINITY, f32::min);
        assert!(
            a_left > 10.0 * cw + 0.5,
            "the abandoned cohort must be moving toward the caret (left edge {a_left})"
        );
        let b_left = b.iter().map(|s| s.x).fold(f32::INFINITY, f32::min);
        let b_right = b.iter().map(|s| s.x).fold(f32::NEG_INFINITY, f32::max);
        // The six typed cells end at 46 cw; the ATTACH (2026-09-14) carries
        // the head's light one cell on under the caret at 46, to 47 cw.
        assert!(
            (b_left - 40.0 * cw).abs() < 0.5 && (b_right - 47.0 * cw).abs() < 0.5,
            "the cohort under the hand was squashed with its row-mate: [{b_left}, {b_right}]"
        );
    }

    /// **THE INSERT'S REWRITE** ([`Event::Rewrite`], 2026-09-10) retracts the
    /// row's suffix from the GIVEN column — not from `ctx.caret`, the mirror
    /// the host has not moved yet — farthest-first inside `12·n + 240` ms,
    /// the kill's law verbatim, and moves the ribbon's caret there. Cells
    /// left of the column are untouched.
    ///
    /// RED-PROOF (2026-09-10, the variant stubbed inert): fails at the first
    /// assert — no cell carries a `retract_at`.
    #[test]
    fn a_rewrite_retracts_the_suffix_from_the_given_column_not_the_mirror() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 10, 20, &c, 0.8);
        let now = at(t0, 1300);
        // The mirror still stands at the old end (2, 30).
        let cx = ctx(now, &c, (2, 30), 0.8);
        rib.on_event(
            &Event::Rewrite {
                row: 2,
                col: 18,
                cells: 12,
            },
            now,
            &cx,
        );
        let retract_at = |col: u16| {
            rib.cells()
                .iter()
                .find(|cell| cell.row == 2 && cell.col == col)
                .unwrap_or_else(|| panic!("no cell at (2, {col})"))
                .retract_at
        };
        assert!(
            (18..30u16).all(|col| retract_at(col).is_some()),
            "every cell at or right of the rewrite's caret retracts"
        );
        assert!(
            (10..18u16).all(|col| retract_at(col).is_none()),
            "the cells left of it are untouched"
        );
        let span = kill_span_s(12);
        let far = retract_at(29).unwrap();
        let near = retract_at(18).unwrap();
        assert!(far < near, "farthest from the caret goes first");
        let near_s = near.saturating_duration_since(now).as_secs_f32();
        assert!(
            (near_s - span * 11.0 / 12.0).abs() < 1e-3,
            "the near cell goes last, at the kill's 12·n + 240 stagger: {near_s}"
        );
        assert_eq!(
            rib.caret(),
            Some((2, 18)),
            "the ribbon's caret moves to the rewrite"
        );
    }

    #[test]
    fn under_reduced_motion_the_exit_swoosh_moves_nothing_and_only_fades() {
        let mut c = cfg(true, true);
        c.reduced_motion = true;
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 10, 8, &c, 0.8);
        let last = at(t0, 7 * 60);
        let cw = geom().cw as f32;
        let mut sink = Sink::default();
        let mut frames = 0u32;
        let mut peak_prev = u8::MAX;
        let total = (SWOOSH_TOTAL_S * 1000.0).round() as u64;
        for ms in ((PHRASE_REST_MIN_S * 1000.0) as u64..total + 20).step_by(10) {
            let now = at(last, ms);
            let cx = ctx(now, &c, (2, 18), 0.0);
            rib.plan(&cx);
            let mut f = sink.frame();
            rib.emit(&cx, &mut f);
            if sink.under.is_empty() {
                continue;
            }
            frames += 1;
            let left = sink
                .under
                .iter()
                .map(|q| f32::from(q.x))
                .fold(f32::INFINITY, f32::min);
            let right = sink
                .under
                .iter()
                .map(|q| f32::from(q.x) + f32::from(q.w))
                .fold(f32::NEG_INFINITY, f32::max);
            // Eight typed cells, 10..18 cw, plus the ATTACH under the caret
            // at 18 (2026-09-14) — static on every frame like the rest.
            assert!(
                (left - 10.0 * cw).abs() < 0.5 && (right - 19.0 * cw).abs() < 0.5,
                "a static mark moved at +{ms} ms: [{left}, {right}]"
            );
            let peak = sink.under.iter().map(|q| q.alpha).max().unwrap_or(0);
            assert!(
                peak <= peak_prev,
                "the one linear fade may only fall (+{ms} ms: {peak_prev} → {peak})"
            );
            peak_prev = peak;
        }
        assert!(
            frames > 40,
            "the mark must be visible through most of the swoosh window ({frames})"
        );
        assert!(rib.at_rest(), "…and be gone at its end");
    }

    #[test]
    fn a_jump_takes_the_band_out_from_the_light_it_has_never_by_a_step() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        // Forty cells over 2.4 s: the oldest are past the 70 % of their life
        // where a life clamp would have stepped the melt.
        type_run(&mut rib, t0, 2, 40, &c, 0.9);
        let last = at(t0, 39 * 60);
        let cx = ctx(at(last, 8), &c, (2, 42), 0.9);
        rib.plan(&cx);
        let before = band_sum(&rib, 42);
        let segs = rib.plan_segments().len() as u32;
        assert!(
            plan_peak(&rib) > 200,
            "the mark must be hot for a step to be measurable"
        );
        let now = at(last, 16);
        let cx = ctx(now, &c, (2, 80), 0.9);
        // A PTY-licensed jump: the abandon alone (no credit, no wake — R5's
        // wake is pinned on its own below).
        rib.on_event(&pty((2, 42), (2, 80)), now, &cx);
        rib.plan(&cx);
        let after = band_sum(&rib, 42);
        assert!(
            after + 2 * segs >= before,
            "the jump stepped the band down ({before} → {after} over {segs} boundaries) instead of retracting it from where it was"
        );
        // …and the band then leaves through the retract + fade it would have
        // taken anyway, farthest-first, and is gone at their end.
        let cx = ctx(at(last, 16 + 340), &c, (2, 80), 0.9);
        rib.plan(&cx);
        let mid = plan_sum(&rib);
        assert!(
            mid * 10 < before * 8,
            "the abandoned band must be well into its drain by mid-retract ({before} → {mid})"
        );
        let cx = ctx(at(last, 16 + 660), &c, (2, 80), 0.9);
        rib.plan(&cx);
        assert!(
            rib.at_rest(),
            "the abandoned band must be out {RETRACT_DUR_S} + {RETRACT_FADE_S} s after the jump"
        );
    }

    // -- R5: the wake ------------------------------------------------------

    /// Cells on `row` as `(col, typing, life_s, born)`, column-sorted.
    fn row_cells(rib: &Ribbon, row: u16) -> Vec<(u16, bool, f32, Instant)> {
        let mut v: Vec<_> = rib
            .cells()
            .iter()
            .filter(|c| c.row == row)
            .map(|c| (c.col, c.typing, c.life_s, c.born))
            .collect();
        v.sort_by_key(|c| c.0);
        v
    }

    #[test]
    fn a_jump_lays_its_wake_counted_back_from_the_landing_and_never_the_whole_row() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        // A cold Ctrl-A across 108 columns: WAKE_MAX_CELLS beside the caret,
        // the landing included, each born on the flight clock — cell `j`
        // from the origin at `T · j / n`, `T = flight_ms(108)` = 120 ms — with
        // the fixed life — never the four-letter chain law's.
        let cx = ctx(t0, &c, (2, 2), 0.0);
        rib.on_event(&nav((2, 110), (2, 2)), t0, &cx);
        let cells = row_cells(&rib, 2);
        let cols: Vec<u16> = cells.iter().map(|c| c.0).collect();
        assert_eq!(
            cols,
            (2..2 + WAKE_MAX_CELLS).collect::<Vec<_>>(),
            "a leftward jump's wake runs from the landing rightward, capped"
        );
        let flight = flight_ms(108.0) / 1000.0;
        assert!((flight - 0.12).abs() < 1e-6, "the clamp: 120 ms");
        for &(col, typing, life, born) in &cells {
            assert!(!typing, "col {col}: a wake cell is never typing");
            assert!(
                (life - WAKE_LIFE_S).abs() < 1e-6,
                "col {col}: fixed life, got {life}"
            );
            let j = f32::from(110 - col);
            let want = t0 + Duration::from_secs_f32(flight * j / 108.0);
            let off = born
                .saturating_duration_since(want)
                .max(want.saturating_duration_since(born));
            assert!(
                off <= Duration::from_millis(1),
                "col {col}: born on the flight clock at T·{j}/108 (off by {off:?})"
            );
        }
        let (first, last) = (cells[cells.len() - 1].3, cells[0].3);
        assert!(
            first < last,
            "the cell nearest the origin is born first, the landing last"
        );
        assert_eq!(rib.cohorts().len(), 1, "one cohort, the wake's own");
        // A cold Ctrl-E the other way, once the first wake is long gone: the
        // wake runs from the landing LEFTWARD, and the origin cell at col 2
        // is beyond the cap, so it is not painted.
        let t1 = at(t0, 3000);
        let cx = ctx(t1, &c, (2, 110), 0.0);
        rib.plan(&cx);
        assert!(rib.at_rest(), "the first wake is out by +3 s");
        rib.on_event(&nav((2, 2), (2, 110)), t1, &cx);
        let cols: Vec<u16> = row_cells(&rib, 2).iter().map(|c| c.0).collect();
        assert_eq!(
            cols,
            (111 - WAKE_MAX_CELLS..111).collect::<Vec<_>>(),
            "a rightward jump's wake ends AT the landing, capped"
        );
        // The landing is the run's HEAD (newest cell): the hot edge sits by
        // the caret, not at the far end.
        let newest = rib
            .cells()
            .iter()
            .max_by_key(|c| (c.born, c.col))
            .map(|c| c.col);
        assert_eq!(newest, Some(110));
    }

    /// **THE HEAD OF A LEFTWARD WAKE IS ITS LANDING** (2026-09-10, the
    /// stream round; the owner: the trail "needs more edge case handling
    /// for when the cursor is jumping around"). After a Ctrl-A the standing
    /// hot edge must come up WHERE THE CARET IS — over the landing cell and
    /// reaching rightward over the corridor — not where the caret left.
    ///
    /// FAILING BEFORE (measured on `9c67b4769`): `wake()` lays the far end
    /// first and the landing last, every NEW cell with the one
    /// `born_new = at + WAKE_BORN_LAG_S`, and the taken-over typed cells keep
    /// their older births; `head_col`'s own-cell branch fails at the landing
    /// (`caret.col − 1` underflows at col 0) and falls to `max_by(born)`,
    /// which returns the LAST equal maximum of a column-ascending run — the
    /// HIGHEST-column new cell, i.e. the far end of the corridor the jump
    /// crossed. On this fixture (a 40-cell word at cols 2..41, Ctrl-A to
    /// col 0) the new cells are cols 0 and 1, so the head resolved to col 1
    /// and the hairline lay at `x ∈ [0, 2·cw]` — BEHIND the caret, never
    /// over the corridor; in the deletion script's 20 → 0 jump the new
    /// cells span the whole corridor and the edge lit its FAR end, 20 cells
    /// from the hand. `wake()`'s own comment ("the landing is the newest
    /// cell, so it is the run's head") stated the intent the code missed.
    ///
    /// Now `head_col` has a WAKE clause: the caret standing ON the first cell
    /// of a `Cohort::wake` run makes that cell the head and the run's
    /// `stream_dir` +1, and the hot edge is measured from the landing's LEFT
    /// edge rightward. The typed-cohort rule (`Run::head`: "the head is always
    /// the run's right-hand side") is untouched — a Backspace cannot reach
    /// the clause (typed cohorts are `wake == false`). The rightward mirror
    /// (a Ctrl-E) is unchanged: its head is the caret's own cell, and the
    /// edge ends at the caret's left edge as it did.
    #[test]
    fn a_leftward_wake_s_hot_edge_sits_at_the_landing_not_where_the_caret_left() {
        let c = cfg(true, true);
        let g = geom();
        let cw = g.cw as f32;
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 2, 40, &c, 0.9);
        let last = at(t0, 39 * 60);
        let jump = at(last, 8);
        let cx = ctx(jump, &c, (2, 0), 0.9);
        rib.on_event(&nav((2, 42), (2, 0)), jump, &cx);
        rib.plan(&cx);
        let mut sink = Sink::default();
        let now = at(jump, 150);
        let cx = ctx(now, &c, (2, 0), 0.9);
        rib.plan(&cx);
        {
            let mut f = sink.frame();
            rib.emit(&cx, &mut f);
        }
        assert!(
            !sink.out.is_empty(),
            "a hot hand's wake must carry a hot edge once its cells are born"
        );
        let landing_x = f32::from(g.origin_x);
        let (mut lo, mut hi) = (f32::INFINITY, f32::NEG_INFINITY);
        for q in &sink.out {
            let x = f32::from(q.x);
            let x1 = x + f32::from(q.w);
            lo = lo.min(x);
            hi = hi.max(x1);
            assert!(
                x >= landing_x - 1.0,
                "the hot edge lies BEHIND the landing after a Ctrl-A (x = {x})"
            );
            assert!(
                x <= landing_x + HOT_EDGE_CELLS_MAX * cw + f32::from(q.w) + 1.0,
                "the hot edge reached further than {HOT_EDGE_CELLS_MAX} cells from the landing (x = {x})"
            );
        }
        println!("leftward wake hot edge: x ∈ [{lo}, {hi}] (landing {landing_x}, cw {cw})");
        // It REACHES over the corridor: past the landing cell's own right
        // edge and at least the base reach, so the eye finds it at the hand.
        assert!(
            hi >= landing_x + HOT_EDGE_CELLS * cw - 1.0,
            "the hot edge does not reach rightward from the landing (right end {hi}, want ≥ {})",
            landing_x + HOT_EDGE_CELLS * cw - 1.0
        );
        assert_eq!(
            rib.runs.first().map(|r| (r.head_col, r.stream_dir)),
            Some((0, 1)),
            "the landing is the head and the stream runs rightward"
        );
        // THE MIRROR: a cold Ctrl-E lays its wake ending AT the landing; the
        // caret stands ON the landing cell, so the head is the caret's own
        // cell (`caret.col − 1`) and the edge ends at the caret's left edge —
        // within five cells left of col 43's left edge, unchanged today.
        let mut rib = Ribbon::new();
        let cx = ctx(t0, &c, (2, 42), 0.9);
        rib.on_event(&nav((2, 2), (2, 42)), t0, &cx);
        let now = at(t0, 150);
        let cx = ctx(now, &c, (2, 42), 0.9);
        rib.plan(&cx);
        {
            let mut f = sink.frame();
            rib.emit(&cx, &mut f);
        }
        assert!(
            !sink.out.is_empty(),
            "the Ctrl-E's wake must carry a hot edge"
        );
        let edge = 43.0 * cw;
        for q in &sink.out {
            let x = f32::from(q.x);
            assert!(
                x <= edge + 1.0 && edge - x <= HOT_EDGE_CELLS_MAX * cw + f32::from(q.w) + 1.0,
                "a rightward wake's hot edge left the five cells before the landing's right edge (x = {x})"
            );
        }
        assert_eq!(
            rib.runs.first().map(|r| (r.head_col, r.stream_dir)),
            Some((41, -1)),
            "a rightward wake keeps the own-cell head and a leftward stream"
        );
    }

    #[test]
    fn a_wake_cell_is_dark_until_it_is_born_then_takes_the_one_attack() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        let cx = ctx(t0, &c, (2, 2), 0.0);
        rib.on_event(&nav((2, 60), (2, 2)), t0, &cx);
        // THE FLIGHT CLOCK (K3): 58 cells fly `T = flight_ms(58)` = 108 ms.
        // The corridor is capped at 32 cells back from the landing, so the
        // first cell laid is col 33 — `j = 27` from the origin — born at
        // `T · 27/58` = 50 ms; the landing (col 2, `j = 58`) is born at `T`.
        let flight_ms_ = flight_ms(58.0) as u64;
        let first_ms = (flight_ms(58.0) * 27.0 / 58.0) as u64;
        let edge_ms = (EDGE_IN_S * 1000.0) as u64;
        for ms in [0u64, first_ms / 2, first_ms - 1] {
            let cx = ctx(at(t0, ms), &c, (2, 2), 0.0);
            rib.plan(&cx);
            assert_eq!(
                plan_peak(&rib),
                0,
                "+{ms} ms: a wake cell is dark before its birth"
            );
            assert!(
                !rib.at_rest(),
                "+{ms} ms: …but it is laid, so the host keeps ticking"
            );
        }
        // Half-way through the flight the corridor is lit behind the head
        // and dark ahead of it: the landing's own cell is still unborn.
        let cx = ctx(at(t0, flight_ms_ / 2 + 10), &c, (2, 2), 0.0);
        rib.plan(&cx);
        assert!(plan_peak(&rib) > 0, "the corridor is lit behind the head");
        let landing = rib
            .cells()
            .iter()
            .find(|l| l.row == 2 && l.col == 2)
            .expect("the landing cell is laid");
        assert_eq!(
            rib.env_of(&cx, landing),
            0.0,
            "…and dark ahead of it: the landing is unborn until T"
        );
        let cx = ctx(at(t0, flight_ms_ + edge_ms + 2), &c, (2, 2), 0.0);
        rib.plan(&cx);
        assert!(
            plan_peak(&rib) >= (UNDER_COV_CAP * BODY_COLD_SHARE) as u8 - 2,
            "one attack past the landing's birth the wake is at its cold ceiling ({})",
            plan_peak(&rib)
        );
        assert!(
            rib.env_of(
                &cx,
                rib.cells().iter().find(|l| l.col == 2).expect("landing")
            ) > 0.99,
            "the landing cell is at full one edge-in past T"
        );
        // …and it is out one life after the last birth, through its own melt.
        let cx = ctx(
            at(t0, flight_ms_ + (WAKE_LIFE_S * 1000.0) as u64 + 20),
            &c,
            (2, 2),
            0.0,
        );
        rib.plan(&cx);
        assert!(
            rib.at_rest(),
            "the wake is gone one WAKE_LIFE_S after its last birth"
        );
    }

    #[test]
    fn a_jump_lays_its_wake_over_the_live_band_and_leaves_the_band_s_cells_untouched() {
        // 2026-09-13 (K1, M1): this pin was `a_jump_hands_the_live_band_to_
        // its_wake_with_the_stops_it_had_and_no_step` — the takeover. Under
        // the overlay the band's cells are BYTE-IDENTICAL after the jump,
        // the corridor lies over them on `Over` at the band's own level with
        // the same stops, and the eye still sees no step.
        //
        // RESTATED 2026-09-14 at the scrub merge, because its PREMISE moved
        // and not its law. v3 drove this over a band at cols 2..42 with a
        // Ctrl-A from 42 — a corridor lit end to end by the hand's own text,
        // which `Ribbon::scrub` now answers instead: held, nothing laid,
        // nothing abandoned (`a_home_and_an_end_inside_the_band_abandon_
        // nothing_and_take_nothing_over`, which is that exact script). So
        // the band starts at column 12 and the jump crosses ten dark cells
        // before it reaches the band: the move LEAVES the run, which is the
        // event v3 §2.7's overlay is about, and every assertion below is
        // v3's own, now read over a corridor with a dark half and a lit one.
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        // Forty cells at cols 12..52 over 2.4 s, then a Ctrl-A to column 2:
        // the corridor crosses ten dark cells before the band, so the jump
        // LEAVES the run (a Ctrl-A onto the band's own first cell is a scrub
        // since 2026-09-14 — `a_home_and_an_end_inside_the_band_abandon_nothing_and_take_nothing_over`)
        // and the band under the corridor is handed to the wake. It keeps
        // every stop it had (C2), loses no light on the jump frame, has ONE
        // owner per cell, and then outlives the jump on the wake's clock
        // instead of the abandon's 0.64 s retract. (A jump AWAY from the
        // band — Alt-F past its end — leaves the band behind its origin,
        // outside the corridor: that light still goes out through the
        // abandon, pinned above under the PTY licence.)
        type_keys(
            &mut rib,
            t0,
            Keys {
                g: geom(),
                row: 2,
                col0: 12,
                n: 40,
                period_ms: 60,
                disp: 0.9,
            },
            &c,
        );
        let last = at(t0, 39 * 60);
        let cx = ctx(at(last, 8), &c, (2, 52), 0.9);
        rib.plan(&cx);
        // THE BAND'S OWN LIGHT, NOT THE CARET'S — main's law at the 0.86 RC
        // merge, taken at this script's own columns. The notch round makes
        // the band continue one cell under the caret block, so at the
        // `before` frame (caret on col 52) the plan carries an attach at col
        // 52 that is the CARET's light and leaves with it on the jump; main
        // answered that by reading `band_sum` instead of `plan_sum` here.
        // Main's column, 42, was the one its own abandon test uses and is
        // ten cells inside this script's band (cols 12..52), which would
        // have thrown away a quarter of what the three assertions below
        // compare, so the boundary is read at the band's own right edge, 51.
        // The same measure is taken on every later frame, so `before`,
        // `after`, `landed` and `held` are one quantity read four times —
        // main's test compared a `band_sum` against three `plan_sum`s. The
        // corridor's DARK HALF beyond this window (cols 2..12) is asserted
        // on its own below.
        let band_light = |rib: &Ribbon| band_sum(rib, 51);
        let before = band_light(&rib);
        let before_cells: Vec<Cell> = rib.cells().to_vec();
        let segs = rib.plan_segments().len() as u32;
        let stops: Vec<Option<f32>> = (12..52u16).map(|col| rib.field_at(2, col)).collect();
        assert!(
            stops.iter().all(Option::is_some),
            "the band is laid under cols 12..52"
        );
        let now = at(last, 16);
        let cx = ctx(now, &c, (2, 2), 0.9);
        // The band's envelope AT THE JUMP INSTANT — what the corridor's
        // cells are born from.
        let envs_before: Vec<(u16, f32)> = before_cells
            .iter()
            .map(|l| (l.col, rib.env_of(&cx, l)))
            .collect();
        rib.on_event(&nav((2, 52), (2, 2)), now, &cx);
        rib.plan(&cx);
        for (col, want) in (12..52u16).zip(&stops) {
            let got = rib.field_at(2, col);
            assert!(
                matches!((got, want), (Some(g), Some(w)) if (g - w).abs() < 1e-5),
                "col {col}: the wake repainted a stop the eye had read ({want:?} → {got:?})"
            );
        }
        let wake = rib
            .cohorts()
            .iter()
            .map(|c| c.id)
            .max()
            .expect("the wake cohort");
        let band = rib.cohorts()[0];
        assert_ne!(band.id, wake);
        assert!(band.abandoned, "the band is abandoned beneath the corridor");
        assert_eq!(
            (band.retract_col, band.drain_right_first),
            (Some(2), true),
            "…toward the landing, draining from the origin end first (the drain follows the meteor)"
        );
        // THE CORRIDOR'S DARK HALF: the ten cells before the band, which are
        // what makes this move a jump and not a scrub. They carry the
        // corridor and nothing else — no band cell was ever laid there.
        let flight = flight_ms(50.0) / 1000.0;
        for col in 2..12u16 {
            assert!(
                !rib.cells()
                    .iter()
                    .any(|c| c.row == 2 && c.col == col && c.layer == Layer::Base),
                "col {col}: dark ground carries no band cell"
            );
            let over: Vec<&Cell> = rib
                .cells()
                .iter()
                .filter(|c| c.row == 2 && c.col == col && c.layer == Layer::Over)
                .collect();
            assert_eq!(over.len(), 1, "col {col}: the corridor lit the dark ground");
            assert!(over[0].rearm.is_none(), "col {col}: nothing to re-arm from");
            let j = f32::from(52 - col);
            let want = now + Duration::from_secs_f32(flight * j / 50.0);
            assert!(
                over[0].born.saturating_duration_since(want) <= Duration::from_millis(1)
                    && want.saturating_duration_since(over[0].born) <= Duration::from_millis(1),
                "col {col}: born on the flight clock"
            );
        }
        for col in 12..52u16 {
            let base: Vec<&Cell> = rib
                .cells()
                .iter()
                .filter(|c| c.row == 2 && c.col == col && c.layer == Layer::Base)
                .collect();
            let over: Vec<&Cell> = rib
                .cells()
                .iter()
                .filter(|c| c.row == 2 && c.col == col && c.layer == Layer::Over)
                .collect();
            assert_eq!(
                base.len(),
                1,
                "col {col}: the band's own cell is still there"
            );
            assert_eq!(over.len(), 1, "col {col}: one wake cell over it");
            let (b, o) = (base[0], over[0]);
            let was = before_cells
                .iter()
                .find(|c| c.col == col)
                .expect("was laid");
            assert!(
                b.cohort == was.cohort
                    && b.born == was.born
                    && b.cov0 == was.cov0
                    && b.life_s == was.life_s,
                "col {col}: the wake touched the band's cell (K1)"
            );
            assert_eq!(o.cohort, wake, "col {col}: …in the wake cohort");
            assert!(!o.typing, "col {col}: a wake cell is never typing");
            assert!(
                o.cov0 >= b.cov0 - 1e-6,
                "col {col}: the corridor is never dimmer than the band it covers"
            );
            let (re_at, level) = o.rearm.expect("born at the band's level");
            assert_eq!(re_at, o.born);
            // The band's coverage share AT THE JUMP: `cov0 · env` — the
            // newest typed cell is still inside its 18 ms attack at +16 ms.
            let env_before = envs_before
                .iter()
                .find(|&&(c, _)| c == col)
                .map_or(0.0, |&(_, e)| e);
            assert!(
                env_before > 0.9 && (level * o.cov0 - b.cov0 * env_before).abs() < 1e-5,
                "col {col}: the level is the band's own coverage share (level {level}, env {env_before})"
            );
            let j = f32::from(52 - col);
            let want = now + Duration::from_secs_f32(flight * j / 50.0);
            assert!(
                o.born.saturating_duration_since(want) <= Duration::from_millis(1)
                    && want.saturating_duration_since(o.born) <= Duration::from_millis(1),
                "col {col}: born on the flight clock"
            );
        }
        // The origin cell (52) is unlit and beyond the cap counted back from
        // the landing, so it stays dark: the cap bounds NEW light only — the
        // ten dark cells before the band are inside it and are laid, and the
        // band's forty are covered however far back they run.
        assert!(rib.field_at(2, 52).is_none());
        assert_eq!(
            rib.cells().len(),
            90,
            "the band's 40 cells and the corridor's 50 over them"
        );
        // NO STEP on the jump frame (the corridor is unborn, the band is
        // whole) and none once the corridor has landed.
        let after = band_light(&rib);
        assert!(
            after + 2 * segs >= before,
            "the jump frame stepped the band down ({before} → {after} over {segs} boundaries)"
        );
        let cx = ctx(at(now, (flight * 1000.0) as u64 + 20), &c, (2, 2), 0.9);
        rib.plan(&cx);
        let landed = band_light(&rib);
        assert!(
            landed + 2 * segs >= before,
            "the landed corridor is dimmer than the band it covers ({before} → {landed})"
        );
        // Where the abandon's retract would have drained it to nothing, the
        // wake still holds the corridor (the design's ~1.1 s after-effect)…
        let cx = ctx(at(last, 16 + 660), &c, (2, 80), 0.9);
        rib.plan(&cx);
        let held = band_light(&rib);
        assert!(
            held * 2 > before,
            "0.66 s after the jump the wake must still hold the corridor ({before} → {held})"
        );
        // …and it is out one wake life past the last birth.
        let cx = ctx(at(last, 16 + 1250), &c, (2, 80), 0.9);
        rib.plan(&cx);
        assert!(rib.at_rest(), "the wake is out by +1.25 s");
    }

    #[test]
    fn a_wake_is_never_typing_and_holds_no_other_cohort() {
        // THE FLAG PIN. `typing: false` is what keeps "a jump builds no
        // momentum" (mod.rs) true and keeps a wake from lifting another
        // mark's finger: a later flip to `true` would break both silently.
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        // A band on row 3, then a hop on row 2 (no abandon): row 3's clock is
        // untouched, and every row-2 cell is a non-typing wake cell.
        let keys = Keys {
            g: geom(),
            row: 3,
            col0: 10,
            n: 6,
            period_ms: 60,
            disp: 0.8,
        };
        type_keys(&mut rib, t0, keys, &c);
        let row3_alive = rib
            .cohorts()
            .iter()
            .find(|c| c.row == 3)
            .map(|c| c.alive_at);
        let now = at(t0, 400);
        let cx = ctx(now, &c, (2, 21), 0.8);
        rib.on_event(&nav((2, 20), (2, 21)), now, &cx);
        assert!(rib.cells().iter().filter(|c| c.row == 2).all(|c| !c.typing));
        assert_eq!(
            rib.cohorts()
                .iter()
                .find(|c| c.row == 3)
                .map(|c| c.alive_at),
            row3_alive,
            "a hop's wake holds only its own cohort"
        );
        // A jump's wake: not typing either, and `field_at_caret`'s fallback
        // still reads REAL TYPING — the jump wrote no hand colour.
        let now = at(t0, 800);
        let cx = ctx(now, &c, (2, 2), 0.8);
        rib.on_event(&nav((2, 60), (2, 2)), now, &cx);
        assert!(rib.cells().iter().filter(|c| c.row == 2).all(|c| !c.typing));
        assert!(rib.cells().iter().any(|c| c.row == 3 && c.typing));
    }

    #[test]
    fn a_ping_pong_at_speed_does_not_accumulate_and_empties_one_wake_life_after_its_last_key() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        // Ctrl-A / Ctrl-E across 58 cells every 200 ms for 3 s, planned at
        // 120 Hz. Each jump abandons the last wake and lays its own, so the
        // pool plateaus (bounded by WAKE_LIFE_S / period wakes plus the
        // abandoned ones' 0.64 s drains) instead of growing with the count.
        let mut caret = (2u16, 2u16);
        let mut peak_by_cycle = Vec::new();
        let mut peak = 0usize;
        // K2 / K4 (2026-09-13): from the second corridor's landing on, the
        // lit cells on the row are ONE interval holding both endpoints on
        // EVERY frame, and the row holds exactly one live wake cohort after
        // every jump — a reversal re-owns the corridor it overlaps instead of
        // abandoning it and laying a second one beside its drain.
        let landed = 200 + flight_ms(58.0) as u64 + (EDGE_IN_S * 1000.0) as u64 + 8;
        let mut interval_frames = 0u32;
        for ms in (0..=3000u64).step_by(8) {
            let now = at(t0, ms);
            if ms % 200 == 0 {
                let to = if caret.1 == 2 { (2, 60) } else { (2, 2) };
                let cx = ctx(now, &c, to, 0.0);
                rib.on_event(&nav(caret, to), now, &cx);
                caret = to;
                if ms > 0 {
                    peak_by_cycle.push(peak);
                    peak = 0;
                }
            }
            let cx = ctx(now, &c, caret, 0.0);
            rib.plan(&cx);
            peak = peak.max(rib.cells().len());
            if ms % 200 == 0 {
                let wakes = rib
                    .cohorts()
                    .iter()
                    .filter(|k| k.wake && rib.cells().iter().any(|l| l.cohort == k.id))
                    .count();
                assert_eq!(wakes, 1, "+{ms} ms: one live wake cohort on the row (K4)");
            }
            if ms >= landed {
                // A column is lit when any cell at it composites at or over
                // the status floor — the position's owner is the brightest
                // born cell (`build`'s rank), so "any" is what the glass shows.
                let lit: Vec<u16> = (2..=60u16)
                    .filter(|&col| {
                        rib.cells().iter().any(|l| {
                            l.row == 2
                                && l.col == col
                                && UNDER_COV_CAP * l.cov0 * rib.env_of(&cx, l)
                                    >= f32::from(STATUS_LIT_COV)
                        })
                    })
                    .collect();
                assert!(
                    lit.first() == Some(&2) && lit.last() == Some(&60),
                    "+{ms} ms: the lit set holds both endpoints ({lit:?})"
                );
                assert!(
                    lit.windows(2).all(|w| w[1] - w[0] <= 1),
                    "+{ms} ms: the lit set is one interval, no hole (K2): {lit:?}"
                );
                interval_frames += 1;
            }
        }
        assert!(
            interval_frames > 300,
            "the interval law was checked on {interval_frames} frames"
        );
        let bound = usize::from(WAKE_MAX_CELLS + 1) * (WAKE_LIFE_S / 0.2).ceil() as usize
            + 59 * (RETRACT_START_S / 0.2).ceil() as usize;
        let worst = *peak_by_cycle.iter().max().expect("cycles");
        assert!(
            worst <= bound,
            "the pool grew past its bound: {worst} > {bound} ({peak_by_cycle:?})"
        );
        let (early, late) = peak_by_cycle.split_at(peak_by_cycle.len() / 2);
        assert!(
            late.iter().max() <= early.iter().max(),
            "the second half of the ping-pong holds more than the first: {peak_by_cycle:?}"
        );
        let cx = ctx(at(t0, 3000 + 1250), &c, caret, 0.0);
        rib.plan(&cx);
        assert!(
            rib.at_rest(),
            "everything is out one wake life after the last jump"
        );
    }

    /// **A PARTIAL REVERSAL RE-OWNS THE CORRIDOR INSTEAD OF ABANDONING IT**
    /// (K4's "mints no second wake cohort on that row"; 2026-09-14). A jump
    /// back that stops SHORT of the corridor it is reversing leaves the far
    /// part of the old corridor uncovered. NOTE WHICH GESTURES REACH HERE: a
    /// hop back over text that is still lit is a SCRUB ([`Ribbon::scrub`]) and
    /// never lays a corridor at all, so the gestures this law answers are the
    /// ones that JUMP — a reversal landing inside a corridor laid over ground
    /// the hand's own light has already left. Until
    /// [`Ribbon::own_wake_cohort`] that part belonged to a cohort the jump's
    /// own `abandon` then sent straight into its retract, so the row carried
    /// two clocks: the covered half on the new jump's, the uncovered half
    /// draining at once. The lit set stopped being one interval well before
    /// the wake's own drain, which is exactly what K2 forbids.
    #[test]
    fn a_partial_reversal_keeps_the_whole_corridor_on_one_clock() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        // A 43-cell run left to go out entirely, so the corridor is laid over
        // NOTHING: over a live band cell a corridor is laid without bound
        // (§2.10) and neither the cap nor the ownership shows.
        type_run(&mut rib, t0, 2, 43, &c, 0.9);
        let cold = at(t0, 6000);
        rib.plan(&ctx(cold, &c, (2, 43), 0.0));
        assert!(
            rib.at_rest(),
            "the typed band is gone before the first jump"
        );
        // Ctrl-A: 43 → 0, the whole row.
        let a = at(cold, 100);
        rib.on_event(&nav((2, 43), (2, 0)), a, &ctx(a, &c, (2, 0), 0.0));
        rib.plan(&ctx(at(a, 130), &c, (2, 0), 0.0));
        let first = rib
            .cohorts()
            .iter()
            .find(|k| k.wake)
            .map(|k| k.id)
            .expect("the first corridor");
        // …then a PARTIAL reversal: 0 → 20, half of it.
        let b = at(a, 300);
        rib.on_event(&nav((2, 0), (2, 20)), b, &ctx(b, &c, (2, 20), 0.0));
        rib.plan(&ctx(at(b, 130), &c, (2, 20), 0.0));
        let live: Vec<(u32, bool)> = rib
            .cohorts()
            .iter()
            .filter(|k| k.wake && rib.cells().iter().any(|l| l.cohort == k.id))
            .map(|k| (k.id, k.abandoned))
            .collect();
        assert_eq!(
            live,
            vec![(first, false)],
            "the reversal re-owns the corridor it overlaps: one live wake cohort, \
             not abandoned ({live:?})"
        );
        // ONE CLOCK over the whole corridor — the covered half and the far
        // half the reversal never reached.
        let cx = ctx(at(b, 130), &c, (2, 20), 0.0);
        let clocks: Vec<Instant> = rib
            .cells()
            .iter()
            .filter(|l| l.layer == Layer::Over)
            .map(|l| rib.live_since(l))
            .collect();
        assert!(clocks.len() > 30, "the corridor is laid ({})", clocks.len());
        assert_eq!(
            clocks.iter().min(),
            clocks.iter().max(),
            "every corridor cell reads one clock, covered or not"
        );
        // …and it stays one interval for the rest of the wake's life.
        let mut frames = 0u32;
        for ms in (0..1000u64).step_by(8) {
            let now = at(b, 130 + ms);
            let cx = ctx(now, &c, (2, 20), 0.0);
            rib.plan(&cx);
            let live: Vec<u16> = rib
                .cells()
                .iter()
                .filter(|l| l.layer == Layer::Over && rib.env_of(&cx, l) > 0.0)
                .map(|l| l.col)
                .collect();
            if live.is_empty() {
                break;
            }
            let (lo, hi) = (
                *live.iter().min().expect("min"),
                *live.iter().max().expect("max"),
            );
            assert_eq!(
                (lo..=hi).filter(|col| live.contains(col)).count(),
                usize::from(hi - lo + 1),
                "+{ms} ms: the corridor is one interval ({lo}..={hi})"
            );
            frames += 1;
        }
        let _ = cx;
        assert!(
            frames > 60,
            "the interval law was checked on {frames} frames"
        );
    }

    #[test]
    fn a_row_change_a_pty_jump_and_a_typed_move_lay_no_wake() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        let cx = ctx(t0, &c, (3, 5), 0.0);
        rib.on_event(&nav((2, 5), (3, 5)), t0, &cx);
        assert!(rib.at_rest(), "an Up-arrow recall paints no line");
        let cx = ctx(t0, &c, (2, 2), 0.0);
        rib.on_event(&pty((2, 60), (2, 2)), t0, &cx);
        assert!(rib.at_rest(), "a PTY cascade earns no wake");
        rib.on_event(
            &Event::Move {
                from: (2, 60),
                to: (2, 2),
                licence: Licence::Typed,
                dir: Dir::Right,
            },
            t0,
            &cx,
        );
        assert!(rib.at_rest(), "a typed echo's own motion lays nothing");
        rib.on_event(
            &Event::Move {
                from: (2, 60),
                to: (2, 2),
                licence: Licence::Return,
                dir: Dir::Right,
            },
            t0,
            &cx,
        );
        assert!(rib.at_rest(), "a return-licensed move lays nothing");
    }

    #[test]
    fn an_arrow_hop_lays_a_short_wake_behind_the_caret_and_leaves_live_cells_alone() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        // A word at cols 10..17, the caret at 18; Right from 18 to 19
        // crosses the caret's empty cell 18 — blank ground beside the word,
        // so it is a hop and not a scrub — and lays 18 and 19 in a WAKE
        // cohort of its own on the word's walk, never in the word's cohort,
        // whose clock, phase and bounds an arrow over blank ground must not
        // touch: an arrow BESIDE the word you just typed does not restart
        // its grace. (An arrow ONTO the word — Left from 18 to 17 — is a
        // scrub since 2026-09-14: it lays nothing and holds the word;
        // `a_scrub_inside_the_band_keeps_every_cell_s_light_and_holds_the_row`.)
        type_run(&mut rib, t0, 10, 8, &c, 0.8);
        let word = rib.cohorts()[0];
        assert!(!word.wake);
        let now = at(t0, 600);
        let cx = ctx(now, &c, (2, 19), 0.8);
        let t17 = rib.field_at(2, 17);
        rib.on_event(&nav((2, 18), (2, 19)), now, &cx);
        rib.plan(&cx);
        assert_eq!(
            rib.field_at(2, 17),
            t17,
            "the live cell beside the hop is untouched"
        );
        let laid: Vec<&Cell> = rib
            .cells()
            .iter()
            .filter(|c| c.row == 2 && c.col == 18)
            .collect();
        assert_eq!(laid.len(), 1);
        assert_ne!(
            laid[0].cohort, word.id,
            "the hop's cell is in a cohort of its own, not the word's"
        );
        let hop = rib
            .cohorts()
            .iter()
            .find(|k| k.id == laid[0].cohort)
            .expect("the hop's cohort");
        assert!(hop.wake, "…a wake cohort");
        assert!(
            (laid[0].t - word.t_at(18)).abs() < 1e-6,
            "…on the word's own walk ({} vs {})",
            laid[0].t,
            word.t_at(18)
        );
        assert!(!laid[0].typing);
        assert!((laid[0].life_s - WAKE_LIFE_S * WAKE_HOP_LIFE_SHARE).abs() < 1e-6);
        assert!(
            rib.cells()
                .iter()
                .filter(|c| c.row == 2 && c.col == 17)
                .count()
                == 1
        );
        let word_now = rib
            .cohorts()
            .iter()
            .find(|k| k.id == word.id)
            .expect("the word");
        assert_eq!(
            (word_now.alive_at, word_now.col0, word_now.col1),
            (word.alive_at, word.col0, word.col1),
            "the hop touched the word's clock or bounds"
        );
        assert_eq!(
            word_now.phase,
            Phase::Grace,
            "180 ms after its last key the word is in its grace — a hop that refreshed it would read Laying"
        );
        // …so the word swooshes exactly when it would have without the arrow:
        // SWOOSH_TOTAL_S after ITS last key (t0 + 420 ms), not after the hop.
        let cx = ctx(
            at(t0, 420 + (SWOOSH_TOTAL_S * 1000.0) as u64 + 20),
            &c,
            (2, 19),
            0.0,
        );
        rib.plan(&cx);
        assert!(
            rib.cells().iter().all(|c| c.cohort != word.id),
            "the word must be out {SWOOSH_TOTAL_S} s after its own last key; the arrow lifted its finger"
        );
        // Right held down over cold ground: a trail behind the caret, one
        // cohort, gone WAKE_LIFE_S / 2 after the last step.
        let mut rib = Ribbon::new();
        for (k, col) in (40u16..46).enumerate() {
            let now = at(t0, 40 * k as u64);
            let cx = ctx(now, &c, (2, col + 1), 0.0);
            rib.on_event(&nav((2, col), (2, col + 1)), now, &cx);
            rib.plan(&cx);
        }
        let cols: Vec<u16> = row_cells(&rib, 2).iter().map(|c| c.0).collect();
        assert_eq!(
            cols,
            (40..=46).collect::<Vec<_>>(),
            "the arrow painted behind itself"
        );
        assert_eq!(rib.cohorts().len(), 1, "one walk under the held arrow");
        let cx = ctx(at(t0, 200 + 100 + 560), &c, (2, 46), 0.0);
        rib.plan(&cx);
        assert!(
            rib.at_rest(),
            "the arrow's trail is out half a wake life after its last step"
        );
    }

    /// **A JUMP INTO A BAND ALREADY LEAVING** (2026-09-13, K1 / M1). The
    /// reviewer's reproduction of 2026-09-09 — forty cells, a Ctrl-A inside
    /// the swoosh window and past it — re-pinned under the overlay. This was
    /// `…_lays_nothing_under_it_and_lights_only_what_has_gone_out`, the
    /// clause that refused to lay a wake cell under a leaving cell because a
    /// TAKEOVER froze a half-drained cell at its level (R6's ramp). Nothing is
    /// taken over now: the band's cells and clock are byte-identical after
    /// the jump, the corridor is laid OVER every column — from the level the
    /// band has there, rising through `edge-in`, never a step — and the
    /// corridor's width no longer depends on how far the band has drained:
    /// that dependence was the refresh's drain tear.
    #[test]
    fn a_jump_into_a_band_already_leaving_lays_its_wake_over_it_from_the_level_it_has() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let cov0_new = BODY_COLD_SHARE + (1.0 - BODY_COLD_SHARE) * 0.9;
        let cw = geom().cw as f32;
        // MAIN'S CLAUSE, KEPT AT THE 0.86 MERGE (2026-09-14): the band's own
        // boundaries end at the head cell's right edge (42 cw), and the
        // ATTACH under the caret cell is the CARET's light — it leaves with
        // the caret on the jump frame, so the no-lurch comparison below must
        // not read it. Without this filter the attach at col 42 vanishes when
        // the caret lands on col 2 and the band reads as having moved, which
        // is the opposite of what the assertion is for. v3 hoisted this
        // closure out of the loop; the clause rides with it.
        let lit_xs = |rib: &Ribbon| -> Vec<(i32, u8)> {
            rib.plan_segments()
                .iter()
                .filter(|s| s.cov > 0 && s.x <= 42.0 * cw + 0.5)
                .map(|s| ((s.x * 16.0).round() as i32, s.cov))
                .collect()
        };
        let rs = (RETRACT_START_S * 1000.0).round() as u64;
        let total = (SWOOSH_TOTAL_S * 1000.0).round() as u64;
        let flight = flight_ms(40.0) / 1000.0;
        let mut laid = Vec::new();
        for idle_ms in [rs + 100, rs + 350, rs + 500, total + 60] {
            let mut rib = Ribbon::new();
            type_run(&mut rib, t0, 2, 40, &c, 0.9);
            let last = at(t0, 39 * 60);
            let now = at(last, idle_ms);
            let cx = ctx(now, &c, (2, 42), 0.9);
            rib.plan(&cx);
            let band = rib.cohorts().first().copied();
            let before: Vec<Cell> = rib.cells().to_vec();
            // v3's own reading: the LEVEL of every band cell at the jump
            // instant, not merely which columns are lit. The overlay is born
            // from that level (`rearm`), so the corridor's assertions below
            // need the number, and `> 0.0` on it is still main's lit/empty
            // split — the two are one list now.
            let level_before: Vec<(u16, f32)> =
                before.iter().map(|l| (l.col, rib.env_of(&cx, l))).collect();
            let xs_before = lit_xs(&rib);
            let cx = ctx(now, &c, (2, 2), 0.9);
            rib.on_event(&nav((2, 42), (2, 2)), now, &cx);
            rib.plan(&cx);
            // NO LURCH: every boundary the leaving band had is exactly where
            // it was on the jump frame — the corridor is unborn, and the
            // band's retract keeps pulling toward the caret it began under.
            assert_eq!(
                xs_before,
                lit_xs(&rib),
                "idle {idle_ms}: the leaving band moved on the jump frame"
            );
            let band_id = band.map(|b| b.id);
            // K1: every band cell is byte-identical, and its clock untouched.
            for was in &before {
                let now_cell = rib
                    .cells()
                    .iter()
                    .find(|l| l.layer == Layer::Base && l.col == was.col)
                    .unwrap_or_else(|| {
                        panic!("idle {idle_ms}: the band's cell at {} is gone", was.col)
                    });
                assert!(
                    now_cell.cohort == was.cohort
                        && now_cell.born == was.born
                        && now_cell.cov0 == was.cov0
                        && now_cell.retract_at == was.retract_at,
                    "idle {idle_ms}: the wake touched the band's cell at col {}",
                    was.col
                );
            }
            if let Some(b) = band {
                let b_now = rib
                    .cohorts()
                    .iter()
                    .find(|k| k.id == b.id)
                    .expect("the band stays in the pool");
                assert_eq!(
                    b_now.alive_at, b.alive_at,
                    "idle {idle_ms}: the band keeps its clock"
                );
                assert!(b_now.phase.is_retracting() || idle_ms < rs);
            }
            // THE CORRIDOR: one wake cohort on `Over`, over every column
            // within the cap and over every lit column beyond it; from the
            // band's level where the band is lit, from dark where it is not.
            let wake: Vec<Cell> = rib
                .cells()
                .iter()
                .filter(|l| l.layer == Layer::Over)
                .copied()
                .collect();
            let wake_id = wake.first().map(|l| l.cohort);
            assert!(wake_id.is_some() && wake_id != band_id);
            let mut wake_cols: Vec<u16> = wake.iter().map(|l| l.col).collect();
            wake_cols.sort_unstable();
            let want: Vec<u16> = (2..42u16)
                .filter(|col| {
                    col - 2 < WAKE_MAX_CELLS
                        || level_before.iter().any(|&(c, e)| c == *col && e > 0.0)
                })
                .collect();
            assert_eq!(
                wake_cols, want,
                "idle {idle_ms}: the corridor covers the cap and every lit cell beyond it"
            );
            for w in &wake {
                assert!(Some(w.cohort) == wake_id, "idle {idle_ms}: one wake cohort");
                assert!(!w.typing);
                let j = f32::from(42 - w.col);
                let want = now + Duration::from_secs_f32(flight * j / 40.0);
                assert!(
                    w.born.saturating_duration_since(want) <= Duration::from_millis(1)
                        && want.saturating_duration_since(w.born) <= Duration::from_millis(1),
                    "idle {idle_ms}: col {} is born on the flight clock",
                    w.col
                );
                let under = before.iter().find(|l| l.col == w.col);
                let env = level_before
                    .iter()
                    .find(|&&(c, _)| c == w.col)
                    .map_or(0.0, |&(_, e)| e);
                match (under, env > 0.0) {
                    (Some(u), true) => {
                        let (re_at, level) = w.rearm.unwrap_or_else(|| {
                            panic!("idle {idle_ms}: col {} over lit light is re-armed", w.col)
                        });
                        assert_eq!(re_at, w.born);
                        let start = u.cov0 * env;
                        assert!(
                            (w.cov0 * level - start).abs() < 1e-5 && w.cov0 >= cov0_new - 1e-6,
                            "idle {idle_ms}: col {} starts at the band's coverage {start} (cov0 {} level {level})",
                            w.col,
                            w.cov0
                        );
                    }
                    _ => {
                        assert!(
                            w.rearm.is_none() && (w.cov0 - cov0_new).abs() < 1e-6,
                            "idle {idle_ms}: col {} over nothing is a cold-priced cell from dark",
                            w.col
                        );
                    }
                }
            }
            laid.push(wake.len());
            // M1 at one covered column: the light the eye sees there does not
            // step when the corridor cell is born over the draining band —
            // sampled one frame either side of its birth.
            if let Some(w) = wake.iter().find(|l| l.col == 20)
                && level_before.iter().any(|&(c, e)| c == 20 && e > 0.3)
            {
                let cov_at = |rib: &mut Ribbon, t: Instant| -> u8 {
                    let cx = ctx(t, &c, (2, 2), 0.9);
                    rib.plan(&cx);
                    cell_cov(rib, geom(), 20)
                };
                let pre = cov_at(&mut rib, w.born - Duration::from_millis(1));
                let post = cov_at(&mut rib, w.born + Duration::from_millis(1));
                assert!(
                    post + 4 >= pre,
                    "idle {idle_ms}: col 20 stepped down at the corridor's birth ({pre} → {post})"
                );
            }
            // Through the wake's life the pool holds only the two kinds — the
            // band's cells draining on their own clock and the wake's — and
            // no band cell remains once its swoosh is over.
            for dt in [50u64, 150, 400, 800] {
                let cx = ctx(at(now, dt), &c, (2, 2), 0.9);
                rib.plan(&cx);
                for l in rib.cells() {
                    assert!(
                        Some(l.cohort) == band_id || Some(l.cohort) == wake_id,
                        "idle {idle_ms} +{dt}: a cell of a third cohort at col {}",
                        l.col
                    );
                }
                if idle_ms + dt > total {
                    assert!(
                        rib.cells().iter().all(|l| Some(l.cohort) != band_id),
                        "idle {idle_ms} +{dt}: the band must be out {SWOOSH_TOTAL_S} s after its last key"
                    );
                }
            }
            let cx = ctx(at(now, 1300), &c, (2, 2), 0.9);
            rib.plan(&cx);
            assert!(
                rib.at_rest(),
                "idle {idle_ms}: everything is out 1.3 s after the jump"
            );
        }
        // The corridor's width does not depend on how far the band has
        // drained: the cap plus whatever the band still lights beyond it —
        // 40 while any of cols 34..42 are lit, the bare cap once it is gone.
        assert_eq!(laid[0], 40, "inside the retract the whole band is covered");
        assert!(
            laid.windows(2).all(|w| w[0] >= w[1]),
            "non-increasing in idle: {laid:?}"
        );
        assert_eq!(
            laid[3],
            usize::from(WAKE_MAX_CELLS),
            "past the swoosh the cold Ctrl-A lays the cap"
        );
    }

    // -- 2026-09-14: scrubbing never damages the band -----------------------
    //
    // The owner, on the shipped v0.85.0: "the painting system for scrubbing
    // back and forth with back word and forward word doesn't work correctly
    // and leaves gaps."

    /// One column of a [`census`]: `(col, owner, planned coverage)`, the
    /// owner as `(cohort, born, typing, cov0 bits)` or `None` where no live
    /// cell owns the column.
    type CensusColumn = (u16, Option<(u32, Instant, bool, u32)>, u8);

    /// The per-cell CENSUS of `row`'s cells `col0..col1` on this frame — the
    /// identity of the cell that owns each column (`cohort`, `born`,
    /// `typing`, `cov0`), and the coverage the plan puts inside that column
    /// (the max `cov` of every boundary and slab whose `x` falls in the
    /// column; a retract that pulls a boundary out of its column moves its
    /// light with it, which is the gap the eye sees). Byte for byte, so
    /// "the scrub damaged nothing" is an equality and not a tolerance.
    fn census(rib: &Ribbon, row: u16, col0: u16, col1: u16) -> Vec<CensusColumn> {
        let cw = geom().cw as f32;
        (col0..col1)
            .map(|col| {
                let owner = rib
                    .cells()
                    .iter()
                    .rposition(|c| c.row == row && c.col == col && !c.leaving())
                    .map(|i| {
                        let c = rib.cells()[i];
                        (c.cohort, c.born, c.typing, c.cov0.to_bits())
                    });
                let cov = rib
                    .plan_segments()
                    .iter()
                    .filter(|s| (s.x / cw).floor() as i64 == i64::from(col))
                    .map(|s| s.cov)
                    .max()
                    .unwrap_or(0);
                (col, owner, cov)
            })
            .collect()
    }

    /// The columns of `census` whose light is gone or is another cell's —
    /// the gaps, as `(col, cov)`.
    fn gaps(before: &[CensusColumn], now: &[CensusColumn]) -> Vec<(u16, u8)> {
        before
            .iter()
            .zip(now)
            .filter(|(b, n)| b != n)
            .map(|(_, n)| (n.0, n.2))
            .collect()
    }

    /// The owner's line, "the quick brown fox jumps over the lazy dogs", typed
    /// at 12 cps on row 2 from column 0 (44 cells, the caret at 44), planned
    /// after every key. Returns the last key's clock.
    fn type_the_owner_s_line(rib: &mut Ribbon, t0: Instant, c: &Config) -> Instant {
        let keys = Keys {
            g: geom(),
            row: 2,
            col0: 0,
            n: 44,
            period_ms: 83,
            disp: 0.8,
        };
        type_keys(rib, t0, keys, c);
        at(t0, 43 * 83)
    }

    /// **SCRUBBING NEVER DAMAGES THE BAND** (2026-09-14). Option+Left ×4,
    /// Option+Right ×4, Option+Left ×2 over the words of the line the hand
    /// just typed, 150 ms apart, starting 150 ms after the last key — the
    /// owner's scrub. Every hop stays inside the run the hand typed, so it
    /// is a SCRUB: it abandons nothing, takes nothing over, lays nothing
    /// and dims nothing — the census of the band's 44 cells is byte-identical
    /// after every hop to the census before the first — and it HOLDS THE ROW
    /// exactly as an erase does (`hold_row`): the band's clock is renewed by
    /// the hop, so it leaves through its swoosh `SWOOSH_TOTAL_S` after the
    /// hand's LAST hop, not after its last key.
    ///
    /// RED before (the 2026-09-09 law "a hop must not touch the word's
    /// clock"): the swoosh ran on the last KEY's clock under the scrubbing
    /// hand — at the seventh hop (+1050 ms) the band was mid-retract, its
    /// tail drained and its boundaries pulled toward the column the retract
    /// began under, and the hops through it laid cold half-life stubs only
    /// where the drain had already emptied a cell: the gaps the owner saw.
    #[test]
    fn a_scrub_inside_the_band_keeps_every_cell_s_light_and_holds_the_row() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        let last = type_the_owner_s_line(&mut rib, t0, &c);
        let band = rib.cohorts()[0];
        // The census just before the first hop.
        let cx = ctx(at(last, 150), &c, (2, 44), 0.8);
        rib.plan(&cx);
        let before = census(&rib, 2, 0, 44);
        assert!(
            before
                .iter()
                .all(|(_, owner, cov)| owner.is_some() && *cov > 100),
            "the band is whole and hot under the line before the scrub"
        );
        // The words' starts: 40 (dogs), 35 (lazy), 31 (the), 26 (over) — and
        // back to the end.
        let stops = [40u16, 35, 31, 26, 31, 35, 40, 44, 40, 35];
        let mut caret = (2u16, 44u16);
        let mut last_hop = last;
        for (k, &col) in stops.iter().enumerate() {
            let now = at(last, 150 * (k as u64 + 1));
            let to = (2, col);
            let cx = ctx(now, &c, to, 0.8);
            rib.on_event(&nav(caret, to), now, &cx);
            rib.plan(&cx);
            caret = to;
            last_hop = now;
            let now_census = census(&rib, 2, 0, 44);
            let g = gaps(&before, &now_census);
            assert!(
                g.is_empty(),
                "hop {} ({}→{}, +{} ms): the scrub damaged the band at {} of 44 cells: {:?}",
                k + 1,
                if k == 0 { 44 } else { stops[k - 1] },
                col,
                150 * (k + 1),
                g.len(),
                g
            );
            assert!(
                rib.cohorts().iter().all(|k| !k.abandoned),
                "hop {}: a scrub abandons nothing",
                k + 1
            );
            assert!(
                rib.cells().iter().all(|c| !c.leaving()),
                "hop {}: a scrub sends no cell on its way out",
                k + 1
            );
        }
        // The scrubbing hand HELD the row: the band's clock is the last hop's.
        let held = rib
            .cohorts()
            .iter()
            .find(|k| k.id == band.id)
            .expect("the typed band is still the band");
        assert_eq!(
            held.alive_at, last_hop,
            "the scrubbing hand holds the row — the band's clock is the hop's"
        );
        assert_eq!(held.phase, Phase::Laying);
        // The band is whole at +0.7 s after the last hop (its grace)…
        let cx = ctx(at(last_hop, 700), &c, caret, 0.8);
        rib.plan(&cx);
        assert!(
            gaps(&before, &census(&rib, 2, 0, 44)).is_empty(),
            "0.7 s after the last hop the band is still whole: the hops renewed its clock"
        );
        // …well into its retract SWOOSH_TOTAL_S − 0.2 s after it, on the
        // hop's clock and not the key's (which would have had it out long
        // ago)…
        let cx = ctx(at(last_hop, 1400), &c, caret, 0.8);
        rib.plan(&cx);
        assert!(
            !rib.at_rest() && plan_sum(&rib) > 0,
            "1.4 s after the last hop the band is leaving on the hop's clock"
        );
        // …and out one swoosh after the hand's last act on the row.
        let cx = ctx(
            at(last_hop, (SWOOSH_TOTAL_S * 1000.0) as u64 + 20),
            &c,
            caret,
            0.8,
        );
        rib.plan(&cx);
        assert!(
            rib.at_rest(),
            "the band leaves through its swoosh {SWOOSH_TOTAL_S} s after the last hop"
        );
    }

    /// **A HOME AND AN END INSIDE THE BAND ARE SCRUBS TOO** (2026-09-14) —
    /// the jump arm. Forty cells at cols 2..42, then Ctrl-A (42 → 2, forty
    /// cells: the meteor flies), Ctrl-E (2 → 42) and Ctrl-A again, 150 ms
    /// apart. The corridor is the band, so the jump abandons nothing and
    /// hands nothing over: every cell keeps its own cohort, birth, price and
    /// `typing`, one owner per cell, the census byte-identical, the row
    /// held — and the band leaves one swoosh after the last jump.
    ///
    /// Discards, for a jump INSIDE the run, the 2026-09-09 law "a real jump
    /// on the same row abandons the band into its swoosh" (`Ribbon::abandon`,
    /// d23b4f935 → 4ca68c467) and its takeover: they moved the band under
    /// the hand onto the wake's 1.1 s clock and drained everything the
    /// corridor did not cover, and the Option+Right back through the
    /// draining part found cells mid-retract that the wake may not take
    /// over — gaps. A jump that LEAVES the run keeps that law (pinned below).
    #[test]
    fn a_home_and_an_end_inside_the_band_abandon_nothing_and_take_nothing_over() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 2, 40, &c, 0.9);
        let last = at(t0, 39 * 60);
        let band = rib.cohorts()[0];
        let cx = ctx(at(last, 150), &c, (2, 42), 0.9);
        rib.plan(&cx);
        let before = census(&rib, 2, 2, 42);
        let mut caret = (2u16, 42u16);
        let mut last_jump = last;
        for (k, &col) in [2u16, 42, 2].iter().enumerate() {
            let now = at(last, 150 * (k as u64 + 1));
            let to = (2, col);
            let cx = ctx(now, &c, to, 0.9);
            rib.on_event(&nav(caret, to), now, &cx);
            rib.plan(&cx);
            caret = to;
            last_jump = now;
            let g = gaps(&before, &census(&rib, 2, 2, 42));
            assert!(
                g.is_empty(),
                "jump {} (→{col}): the jump damaged the band at {} of 40 cells: {:?}",
                k + 1,
                g.len(),
                g
            );
            assert_eq!(rib.cohorts().len(), 1, "jump {}: no wake was minted", k + 1);
            assert_eq!(rib.cells().len(), 40, "jump {}: nothing was laid", k + 1);
            for cell in rib.cells() {
                assert!(
                    cell.cohort == band.id && cell.typing && !cell.leaving(),
                    "jump {}: col {} was taken over or sent away",
                    k + 1,
                    cell.col
                );
            }
            let held = rib.cohorts()[0];
            assert!(!held.abandoned, "jump {}: the band is not abandoned", k + 1);
            assert_eq!(held.alive_at, now, "jump {}: the hand holds the row", k + 1);
        }
        let cx = ctx(at(last_jump, 660), &c, caret, 0.9);
        rib.plan(&cx);
        assert!(
            gaps(&before, &census(&rib, 2, 2, 42)).is_empty(),
            "0.66 s after the last jump — where the abandon's retract would have drained it — the band is whole"
        );
        let cx = ctx(
            at(last_jump, (SWOOSH_TOTAL_S * 1000.0) as u64 + 20),
            &c,
            caret,
            0.9,
        );
        rib.plan(&cx);
        assert!(rib.at_rest(), "…and out one swoosh after the last jump");
    }

    /// **A JUMP THAT LEAVES THE RUN KEEPS THE ABANDON LAW** (2026-09-14, the
    /// control for the scrub). (a) Past the band's OLDEST cell: forty cells
    /// at cols 10..50 and a Ctrl-A to column 0 — the corridor crosses ten
    /// dark cells before it reaches the band, so it is a jump: the wake lays
    /// its corridor and the band goes into its retract beneath it, as
    /// d23b4f935 ruled. (b) Off the row: an Up-arrow abandons the band into
    /// its retract and lays nothing. (c) Past the band's end: a Right ×6
    /// from the caret's cell over blank ground is a hop that leaves the
    /// band's clock alone — the band leaves one swoosh after ITS last key,
    /// and the hop's cold wake sits behind the caret.
    ///
    /// RESTATED 2026-09-14 at the Rainbow Path v3 merge, for (a) only and
    /// for the MECHANISM only, not the law. d23b4f935's abandon said the
    /// corridor TOOK the band's cells over onto the wake's clock; v3 §2.7
    /// (K1) says the corridor is an OVERLAY that never touches them — the
    /// band's cells stay on `Base`, byte-identical, and drain under the
    /// corridor on their own abandon. The law under test is unchanged and is
    /// the whole point of this pin: a jump that leaves the run ABANDONS the
    /// band, where a scrub inside the run holds it.
    #[test]
    fn a_jump_past_the_band_s_oldest_cell_or_off_its_row_still_abandons_it() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        // (a)
        let mut rib = Ribbon::new();
        type_keys(
            &mut rib,
            t0,
            Keys {
                g: geom(),
                row: 2,
                col0: 10,
                n: 40,
                period_ms: 60,
                disp: 0.9,
            },
            &c,
        );
        let last = at(t0, 39 * 60);
        let band = rib.cohorts()[0];
        let now = at(last, 150);
        let cx = ctx(now, &c, (2, 0), 0.9);
        rib.on_event(&nav((2, 50), (2, 0)), now, &cx);
        rib.plan(&cx);
        let wake = rib
            .cohorts()
            .iter()
            .find(|k| k.wake)
            .expect("(a): the jump past the band's oldest cell lays its wake");
        assert!(
            (10..50u16).all(|col| rib
                .cells()
                .iter()
                .any(|c| c.col == col && c.cohort == wake.id && c.layer == Layer::Over)),
            "(a): the corridor lies over every cell of the band"
        );
        assert_eq!(
            rib.cells()
                .iter()
                .filter(|c| c.cohort == band.id && c.layer == Layer::Base)
                .count(),
            40,
            "(a): …and the band's own forty cells are still under it (K1)"
        );
        let leaving = rib
            .cohorts()
            .iter()
            .find(|k| k.id == band.id)
            .expect("(a): the band is still in the pool");
        assert!(
            leaving.abandoned,
            "(a): a jump PAST the band's oldest cell abandons it — this is the law"
        );
        assert!(
            (0..10u16).all(|col| rib
                .cells()
                .iter()
                .any(|c| c.col == col && c.cohort == wake.id)),
            "(a): the dark cells before the band are laid"
        );
        // …and it is gone one abandon later, where a scrub would have held
        // it one swoosh past the hand's last act.
        let cx = ctx(at(now, 660), &c, (2, 0), 0.9);
        rib.plan(&cx);
        assert!(
            rib.cells().iter().all(|c| c.cohort != band.id),
            "(a): the abandoned band is out 0.66 s later"
        );
        // (b)
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 2, 40, &c, 0.9);
        let band = rib.cohorts()[0];
        let cx = ctx(now, &c, (1, 42), 0.9);
        rib.on_event(&nav((2, 42), (1, 42)), now, &cx);
        rib.plan(&cx);
        let gone = rib
            .cohorts()
            .iter()
            .find(|k| k.id == band.id)
            .expect("(b): the band is still in the pool");
        assert!(gone.abandoned, "(b): a row change abandons the band");
        assert_eq!(rib.cohorts().len(), 1, "(b): …and lays no wake");
        let cx = ctx(at(now, 660), &c, (1, 42), 0.9);
        rib.plan(&cx);
        assert!(rib.at_rest(), "(b): the abandoned band is out 0.64 s later");
        // (c)
        let mut rib = Ribbon::new();
        let last = type_the_owner_s_line(&mut rib, t0, &c);
        let band = rib.cohorts()[0];
        let now = at(last, 150);
        let cx = ctx(now, &c, (2, 50), 0.8);
        rib.on_event(&nav((2, 44), (2, 50)), now, &cx);
        rib.plan(&cx);
        let held = rib
            .cohorts()
            .iter()
            .find(|k| k.id == band.id)
            .expect("(c): the band");
        assert_eq!(
            held.alive_at, last,
            "(c): a hop off the band's end does not hold the row"
        );
        assert!(
            rib.cohorts().iter().any(|k| k.wake && k.id != band.id),
            "(c): the hop lays its cold wake behind the caret"
        );
        let cx = ctx(
            at(last, (SWOOSH_TOTAL_S * 1000.0) as u64 + 20),
            &c,
            (2, 50),
            0.8,
        );
        rib.plan(&cx);
        assert!(
            rib.cells().iter().all(|c| c.cohort != band.id),
            "(c): the band leaves one swoosh after ITS last key"
        );
    }

    /// **A SCRUB HOLDS ONLY THE COHORTS THAT OWN ITS CORRIDOR** (2026-09-14,
    /// the fix-up's first guard) — the reviewer's counter-example. Type
    /// `hello world` (eleven cells at cols 0..11); +200 ms a Ctrl-E-shaped
    /// jump 11 → 31 (the wake W1 lays the dark corridor, the band A is
    /// abandoned into its retract); +350 ms a Ctrl-A 31 → 0 (A's drain has
    /// emptied its far cells, so the jump's wake W2 takes W1 over and fills
    /// A's holes with fresh cells — A's drained cells stay RESIDENT under
    /// them, not `leaving()`, until its swoosh ends); +200 ms a Right ×2
    /// 0 → 2 over W2's live cells: a scrub.
    ///
    /// The scrub holds W2 — and NOTHING ELSE. A, shadowed under the
    /// corridor, keeps its clock, its phase, its abandon and every cell's
    /// coverage exactly as a twin that never saw the scrub has them, and is
    /// pruned when its swoosh ends. RED before: the hold loop renewed every
    /// cohort with any non-leaving cell in the corridor, so A went Fading →
    /// Laying with the hop's clock and its drained cells came back from
    /// `0,0,0,0,0,0,0,111` to `153…158` in one frame — light no key bought,
    /// the anti-stray law's own complaint, and the inverse of what
    /// `hold_row` and `place`'s one-finger hold do (both skip `abandoned`).
    #[test]
    fn a_scrub_over_holes_a_hop_filled_leaves_an_abandoned_cohort_exactly_as_it_was() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        // The scrubbed ribbon and its twin: identical up to the scrub, which
        // only `rib` receives; both planned at the same instant with the
        // same caret.
        let mut rib = Ribbon::new();
        let mut twin = Ribbon::new();
        for r in [&mut rib, &mut twin] {
            type_run(r, t0, 0, 11, &c, 0.9);
        }
        let last = at(t0, 10 * 60);
        let band = rib.cohorts()[0];
        for (from, to, ms) in [((2u16, 11u16), (2, 31), 200u64), ((2, 31), (2, 0), 550)] {
            let now = at(last, ms);
            for r in [&mut rib, &mut twin] {
                let cx = ctx(now, &c, to, 0.9);
                r.on_event(&nav(from, to), now, &cx);
                r.plan(&cx);
            }
        }
        let a_before = *rib
            .cohorts()
            .iter()
            .find(|k| k.id == band.id)
            .expect("A is resident until its swoosh ends");
        assert!(
            a_before.abandoned && a_before.phase.is_retracting(),
            "the setup: A is abandoned and inside its swoosh ({:?})",
            a_before.phase
        );
        let w2 = *rib
            .cohorts()
            .iter()
            .filter(|k| k.wake && !k.abandoned)
            .max_by_key(|k| k.alive_at)
            .expect("W2 owns the corridor");
        assert!(
            rib.cells()
                .iter()
                .any(|l| l.cohort == band.id && (0..2).contains(&l.col) && !l.leaving()),
            "the setup: A's drained cells at cols 0..2 are still resident under W2's"
        );
        // The frame before the scrub, on both.
        let scrub_at = at(last, 750);
        let pre = at(scrub_at, 0)
            .checked_sub(Duration::from_millis(16))
            .expect("clock");
        let cx = ctx(pre, &c, (2, 0), 0.9);
        rib.plan(&cx);
        let cov_pre = census(&rib, 2, 0, 32);
        // The scrub: Right ×2 over W2's live cells.
        let cx = ctx(scrub_at, &c, (2, 2), 0.9);
        rib.on_event(&nav((2, 0), (2, 2)), scrub_at, &cx);
        rib.plan(&cx);
        twin.plan(&cx);
        let held = rib
            .cohorts()
            .iter()
            .find(|k| k.id == w2.id)
            .expect("W2 is still in the pool");
        assert_eq!(
            held.alive_at, scrub_at,
            "the scrub holds W2, the cohort that owns the corridor"
        );
        let a_after = *rib
            .cohorts()
            .iter()
            .find(|k| k.id == band.id)
            .expect("A is still resident on the scrub frame");
        assert!(
            a_after.abandoned
                && a_after.alive_at == a_before.alive_at
                && a_after.retract_col == a_before.retract_col
                && a_after.phase.is_retracting(),
            "the scrub left A's clock, abandon and retract target exactly as they were, still leaving: {a_before:?} → {a_after:?}"
        );
        // A's phase moves only with the clock: it is the twin's, planned at
        // the same instant (Retracting → Fading between the jump and the
        // scrub, on A's own clock).
        let a_twin = twin
            .cohorts()
            .iter()
            .find(|k| k.id == band.id)
            .expect("the twin's A");
        assert!(
            a_twin.alive_at == a_after.alive_at
                && a_twin.phase == a_after.phase
                && a_twin.retract_col == a_after.retract_col,
            "A on the scrubbed ribbon is A on the twin: {a_twin:?} vs {a_after:?}"
        );
        // A's own columns (its cells are the newest owners at cols 2..11 —
        // W2 filled only the holes at 0..2): byte-identical to the twin's,
        // and no column brighter than the frame before.
        let cov_now = census(&rib, 2, 0, 32);
        let cov_twin = census(&twin, 2, 0, 32);
        let a_cols: Vec<u16> = rib
            .cells()
            .iter()
            .filter(|l| l.cohort == band.id && !l.leaving())
            .map(|l| l.col)
            .collect();
        assert!(!a_cols.is_empty(), "A still has resident cells");
        for &col in &a_cols {
            let i = usize::from(col);
            assert_eq!(
                cov_now[i], cov_twin[i],
                "col {col}: A's light after the scrub is the twin's"
            );
            assert!(
                cov_now[i].2 <= cov_pre[i].2,
                "col {col}: A's light did not come back ({} → {})",
                cov_pre[i].2,
                cov_now[i].2
            );
        }
        // A is pruned when ITS swoosh ends, not one swoosh after the hop.
        let cx = ctx(at(last, 950), &c, (2, 2), 0.9);
        rib.plan(&cx);
        assert!(
            rib.cohorts().iter().all(|k| k.id != band.id),
            "A is gone when its own swoosh ends — the scrub did not renew it"
        );
    }

    /// **A SCRUB INTO A CORRIDOR DOES NOT MOVE ITS SOFT END**
    /// (2026-09-14, at the merge of the notch round with the scrub round).
    /// The tail-ease law puts a run's one-cell feather at the end away from
    /// its head, and [`Ribbon::head_col`] resolves the head from the CARET:
    /// type `hello world`, Ctrl-E to col 31 (a rightward hop, its wake over
    /// blank ground), Ctrl-A back to col 0 (a leftward corridor whose
    /// landing is col 0 and whose own cells fill the holes the band's drain
    /// left at cols 0..2), then Right Right — a scrub over the corridor's
    /// own live cells. The caret at col 0 took the corridor's WAKE head
    /// (`stream_dir` +1, the feather at its far end); the caret at col 2
    /// took the own-cell head at `caret − 1` (`stream_dir` −1, the feather
    /// at its near end), so the feather crossed the run in ONE frame.
    ///
    /// The boundary is ONE expression on every run now — the left side owns
    /// it, the right enters at the ease — so a run's soft end is its left
    /// one whichever way its light streams, and no caret move can exchange
    /// the two. (Keying it instead to a `Cohort::landing_col` stamped where
    /// the jump put the hand was tried and REJECTED in a second review
    /// round: a run pouring rightward feathers the end the hand types at,
    /// and typing after any leftward jump put the block on 20 of 214
    /// levels, with the swoosh's own reach re-arming the predicate 0.75 s
    /// after the hand lifted. The trade this one makes instead is stated
    /// in `docs/measured/cursor-notch-2026-09-14.md`.) RED before: the corridor's far edge
    /// went 23 → 233 in one frame, a 210-level rise inside a settled band,
    /// and its near end went dark by the same step. It is the same class of
    /// pop the boundary law itself was written to remove, and
    /// `scrub_gaps.rs`'s
    /// `a_scrub_over_holes_a_hop_filled_leaves_an_abandoned_band_alone_at_the_host_seam`
    /// caught it at the host seam.
    ///
    /// RESTATED AT THE 0.86 RC MERGE (2026-09-14) — THE LAW IS MAIN'S, THE
    /// COLUMNS ARE v3's. Main wrote this over a FOUR-COLUMN window (0..4)
    /// because on main the Ctrl-A's wake laid new light only where the
    /// band's drain had already emptied a cell, so the corridor's own run
    /// was cols 0..2 and its right-edge vertex was col 3. Rainbow Path v3
    /// §2.7 makes the wake an OVERLAY: the corridor is laid on `Layer::Over`
    /// over every column it crosses, lit or not, out to [`WAKE_MAX_CELLS`] —
    /// so here it is one run at cols 0..=31 with its vertex at col 32, and
    /// cols 0..4 are all interior to it. On a window that short, "whole to
    /// its right edge" is read three cells in from the left and says nothing
    /// about the right edge at all; that is how this pin would go quietly
    /// vacuous. The window is read off the corridor's OWN run now — its
    /// left cell through the vertex one past its last — and the whole-ness
    /// clause is read over every column of it rather than two. MEASURED on
    /// the merged tree: col 0 at 163 (the left feather, the trade the second
    /// review round chose and stated), cols 1..=32 flat at 233 through the
    /// right-edge vertex, identical before and after the scrub, while
    /// `stream_dir` still flips +1 → −1 across it — the flip the soft end
    /// must not follow. REFUTED to prove it can still fail: swap the
    /// boundary to `lb.max(la * RUN_TAIL_EASE)` — the right side owning it —
    /// and col 0 rises to 233 while the vertex at col 32 drops to 23.
    #[test]
    fn a_scrub_into_a_leftward_corridor_does_not_move_its_soft_end() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 0, 11, &c, 0.9);
        let last = at(t0, 10 * 60);
        for (from, to, ms) in [((2u16, 11u16), (2, 31), 200u64), ((2, 31), (2, 0), 550)] {
            let now = at(last, ms);
            let cx = ctx(now, &c, to, 0.9);
            rib.on_event(&nav(from, to), now, &cx);
            rib.plan(&cx);
        }
        let cx = ctx(at(last, 734), &c, (2, 0), 0.9);
        rib.plan(&cx);
        // THE CORRIDOR'S OWN EXTENT, read off its run rather than assumed:
        // the overlay reaches as far as the jump did, capped at
        // `WAKE_MAX_CELLS`. The census window is its landing through the
        // vertex one past its far cell.
        let corridor = |r: &Ribbon| {
            r.runs
                .iter()
                .find(|u| u.row == 2 && u.col0 == 0)
                .map(|u| (u.col0, u.col1, u.stream_dir))
                .expect("the corridor's own run")
        };
        let (c0, c1, dir_before) = corridor(&rib);
        assert!(
            c1 >= c0 + 4,
            "the setup: the corridor must be long enough to have a far end away from its landing (cols {c0}..={c1})"
        );
        let before = census(&rib, 2, c0, c1 + 2);
        let cohorts_before = rib.cohorts().len();
        let scrub_at = at(last, 750);
        let cx = ctx(scrub_at, &c, (2, 2), 0.9);
        rib.on_event(&nav((2, 0), (2, 2)), scrub_at, &cx);
        rib.plan(&cx);
        let after = census(&rib, 2, c0, c1 + 2);
        let (a0, a1, dir_after) = corridor(&rib);
        let show = |cen: &[CensusColumn]| cen.iter().map(|k| (k.0, k.2)).collect::<Vec<_>>();
        // THE PRECONDITION: the two Rights are a scrub (nothing is minted)
        // and the head DOES still flip across them — the run's light streams
        // one way with the caret on the landing and the other with it two
        // cells in. Without this the pin could go quietly vacuous.
        assert_eq!(
            cohorts_before,
            rib.cohorts().len(),
            "the setup: the two Rights are a scrub over lit cells and mint nothing"
        );
        assert_eq!(
            (a0, a1),
            (c0, c1),
            "the scrub moved the corridor's own bounds, so the two censuses read different columns"
        );
        assert_ne!(
            dir_before, dir_after,
            "the setup: the caret move still flips the corridor run's stream, which is what the soft end must no longer follow"
        );
        // The soft end is the corridor's LEFT one, where the band opens,
        // and it is whole from there through to its right-edge vertex. On
        // both frames: the caret move does not exchange the two. Read over
        // EVERY column of the run and not a three-column peek, so a census
        // that went dark past the window cannot pass the equalities.
        for (name, cen) in [("before the scrub", &before), ("after it", &after)] {
            let whole = cen[1].2;
            assert!(
                cen[0].2 < whole && whole > 100 && cen[1..].iter().all(|k| k.2 == whole),
                "{name}: the corridor opens soft at its left and is whole to its right edge: {:?}",
                show(cen)
            );
        }
        for (b, a) in before.iter().zip(after.iter()) {
            assert!(
                i32::from(a.2) - i32::from(b.2) <= 2,
                "col {}: the scrub raised the corridor's light {} -> {} in one frame ({:?} -> {:?})",
                b.0,
                b.2,
                a.2,
                show(&before),
                show(&after)
            );
        }
    }

    /// **A SCRUB NEVER RAISES A CELL'S LIGHT** (2026-09-14, the fix-up's
    /// second guard) — the reviewer's counter-example. Type `the quick`
    /// (nine cells at cols 0..9); +150 ms a Right ×6 9 → 15 over blank
    /// ground (a hop: its cold half-life wake at cols 9..15, 0.55 s); +600 ms
    /// a Left ×5 15 → 10 back over the stub. The stub is 0.5 s into its
    /// 0.55 s life — deep inside its expiry melt (which opens at 0.70 of
    /// life) at ~0.09 of body — while its cohort is still in its grace.
    ///
    /// A cell past its melt opening is a cell already going out: the move is
    /// NOT a scrub, it takes the hop arm, lays nothing over the stub (a live
    /// cell is left as it is) and the stub finishes melting to zero as it
    /// did on main — no column is brighter on the landing frame than on the
    /// frame before. RED before: the scrub held the stub's cohort, `spent`
    /// restarted at zero and the stub snapped from 21 to 157 levels in one
    /// frame, a ×7.5 step with no attack. The typed band is never caught
    /// by this guard: its melt opens at 0.70 × 1.70 = 1.19 s, past the
    /// 0.90 s retract the phase guard already refuses.
    ///
    /// The control: the same Left ×5 at +200 ms, the stub at 0.1 s of life
    /// (un-melted), IS a scrub — it holds the stub's cohort and no column
    /// moves a level.
    #[test]
    fn a_scrub_over_a_melting_wake_stub_raises_no_cell_s_coverage() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let hop_at = |last: Instant| at(last, 150);
        // (a) The melting stub.
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 0, 9, &c, 0.9);
        let last = at(t0, 8 * 60);
        let band = rib.cohorts()[0];
        let cx = ctx(hop_at(last), &c, (2, 15), 0.9);
        rib.on_event(&nav((2, 9), (2, 15)), hop_at(last), &cx);
        rib.plan(&cx);
        let stub = *rib
            .cohorts()
            .iter()
            .find(|k| k.wake)
            .expect("the hop lays its cold wake");
        // 2026-09-14, the v3 merge: the corridor is born ON THE FLIGHT CLOCK
        // now (cell `j` of `n` at `T·j/n`, `T` = `FLIGHT_MIN_MS` 60 ms for a
        // hop) where it used to be born one flat `WAKE_BORN_LAG_S` 100 ms
        // after the key, so every stub cell is ~0.1 s older at a given
        // instant and the whole stub is OUT by +600 ms. The probe moves with
        // the births, not the law: at +500 ms every cell of cols 10..15 is
        // past its melt opening (0.70 × 0.55 s = 0.385 s of life; the
        // youngest, col 15, is 0.44 s old) and every one is still lit (the
        // oldest, col 10, is 0.49 s of 0.55 s) — which is the setup the two
        // asserts below read.
        let back_at = at(hop_at(last), 500);
        let pre = back_at
            .checked_sub(Duration::from_millis(16))
            .expect("clock");
        let cx = ctx(pre, &c, (2, 15), 0.9);
        rib.plan(&cx);
        let cov_pre = census(&rib, 2, 0, 16);
        // THE SETUP, read off the engine and not off a coverage threshold:
        // every cell of the corridor the move is about to cross is still LIT
        // and is already PAST ITS MELT OPENING. (Before the flight clock every
        // stub cell shared one birth, so one census bound said this; now they
        // are staggered 10 ms apart across the hop's 60 ms flight and sit at
        // 0.77–0.86 of their life — 62…182 of the body's 233 at this
        // instant — all of them melting, which is what the guard reads.)
        let melting: Vec<(u16, f32, f32)> = (10..16u16)
            .filter_map(|col| {
                rib.cells().iter().find(|l| l.col == col).map(|l| {
                    (
                        col,
                        rib.env_of(&cx, l),
                        pre.saturating_duration_since(live_since(rib.cohorts(), l))
                            .as_secs_f32()
                            - l.life_s * (1.0 - EXPIRY_MELT_SHARE),
                    )
                })
            })
            .collect();
        assert!(
            melting.len() == 6
                && melting
                    .iter()
                    .all(|&(_, env, past)| env > 0.0 && past > 0.0),
            "the setup: every cell of cols 10..16 is lit and past its melt opening              (col, env, seconds past the opening) {melting:?}, coverage {:?}",
            cov_pre.iter().map(|k| k.2).collect::<Vec<_>>()
        );
        let cx = ctx(back_at, &c, (2, 10), 0.9);
        rib.on_event(&nav((2, 15), (2, 10)), back_at, &cx);
        rib.plan(&cx);
        let cov_now = census(&rib, 2, 0, 16);
        for col in 0..16usize {
            assert!(
                cov_now[col].2 <= cov_pre[col].2,
                "col {col}: the move over a melting stub raised its light ({} → {})",
                cov_pre[col].2,
                cov_now[col].2
            );
        }
        let stub_now = rib
            .cohorts()
            .iter()
            .find(|k| k.id == stub.id)
            .expect("the stub's cohort is still resident");
        assert_eq!(
            stub_now.alive_at, stub.alive_at,
            "a melting stub's clock is not renewed"
        );
        assert_eq!(
            rib.cohorts()
                .iter()
                .find(|k| k.id == band.id)
                .map(|k| k.alive_at),
            Some(last),
            "the typed band beside the corridor is left on its own clock"
        );
        // …and the stub is out within the 0.11 s its youngest cell had left
        // (col 9 holds the band's own right boundary, sampled at 9·cw).
        let cx = ctx(at(back_at, 130), &c, (2, 10), 0.9);
        rib.plan(&cx);
        let cov_out = census(&rib, 2, 0, 16);
        assert!(
            (10..16).all(|col| cov_out[col].2 == 0),
            "the stub finished melting under the hand: {:?}",
            cov_out.iter().map(|k| k.2).collect::<Vec<_>>()
        );
        // (b) The control: over an un-melted stub the same move is a scrub.
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 0, 9, &c, 0.9);
        let cx = ctx(hop_at(last), &c, (2, 15), 0.9);
        rib.on_event(&nav((2, 9), (2, 15)), hop_at(last), &cx);
        rib.plan(&cx);
        let stub = *rib.cohorts().iter().find(|k| k.wake).expect("the stub");
        let back_at = at(hop_at(last), 200);
        let cx = ctx(back_at, &c, (2, 15), 0.9);
        rib.plan(&cx);
        let cov_pre = census(&rib, 2, 0, 16);
        let cx = ctx(back_at, &c, (2, 10), 0.9);
        rib.on_event(&nav((2, 15), (2, 10)), back_at, &cx);
        rib.plan(&cx);
        let cov_now = census(&rib, 2, 0, 16);
        assert_eq!(
            cov_now.iter().map(|k| k.2).collect::<Vec<_>>(),
            cov_pre.iter().map(|k| k.2).collect::<Vec<_>>(),
            "(b): a scrub over a live stub moves no column a level"
        );
        assert_eq!(
            rib.cohorts()
                .iter()
                .find(|k| k.id == stub.id)
                .map(|k| k.alive_at),
            Some(back_at),
            "(b): …and holds the stub's cohort, which owns the corridor"
        );
        assert_eq!(rib.cells().len(), 9 + 7, "(b): nothing was laid");
    }

    #[test]
    fn a_wake_continues_the_band_s_own_walk_across_the_kink_whichever_way_it_jumps() {
        // `walk_t` is `d/16` for sixteen cells and `1/36` a cell after, so a
        // wake anchored at the LANDING reproduces the band's `t_at` only when
        // the landing is the band's origin (a Ctrl-A) and repaints every stop
        // past the kink on a jump into the band's middle or a Ctrl-E across
        // it. The wake shares the band's origin instead (`wake_origin`).
        assert!(
            (walk_t(39.0) - walk_t(18.0) - walk_t(21.0)).abs() > 0.1,
            "the kink this pin is about"
        );
        let c = cfg(true, true);
        let t0 = Instant::now();
        let last = at(t0, 39 * 60);
        let now = at(last, 16);
        // (a) Into the band's middle from beyond its end, 60 → 20 (a jump
        // that leaves the run: cells 42..60 are dark — from 42 it would be
        // a scrub, 2026-09-14): cells 20..42 are taken over on the stops
        // they had; cells 2..20 stay the band's, on theirs.
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 2, 40, &c, 0.9);
        let cx = ctx(at(last, 8), &c, (2, 42), 0.9);
        rib.plan(&cx);
        let stops: Vec<f32> = (2..42u16)
            .map(|col| rib.field_at(2, col).expect("laid"))
            .collect();
        let cx = ctx(now, &c, (2, 20), 0.9);
        rib.on_event(&nav((2, 60), (2, 20)), now, &cx);
        rib.plan(&cx);
        for (col, want) in (2..42u16).zip(&stops) {
            let got = rib.field_at(2, col).expect("still lit at +0");
            assert!(
                (got - want).abs() < 1e-5,
                "col {col}: the wake repainted {want} → {got}"
            );
        }
        let wake = rib.cohorts().iter().max_by_key(|k| k.id).expect("the wake");
        assert_eq!(
            (wake.anchor_col, wake.t0),
            (rib.cohorts()[0].anchor_col, rib.cohorts()[0].t0),
            "the wake shares the band's origin"
        );
        // (b) Past its end, 42 → 60: the new cells continue the band's walk
        // at the lay rate — no seam at 41/42, and none where a landing-
        // anchored walk would have re-entered the fast sixteen.
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 2, 40, &c, 0.9);
        let band = rib.cohorts()[0];
        let cx = ctx(now, &c, (2, 60), 0.9);
        rib.on_event(&nav((2, 42), (2, 60)), now, &cx);
        let cx = ctx(at(now, 150), &c, (2, 60), 0.9);
        rib.plan(&cx);
        for col in 42..=60u16 {
            let got = rib.field_at(2, col).expect("the corridor is laid");
            let want = band.t_at(col);
            assert!((got - want).abs() < 1e-5, "col {col}: {want} → {got}");
        }
    }

    #[test]
    fn a_wrapped_paragraph_is_one_phrase_and_each_row_keeps_its_own_clock() {
        // MERGED 2026-09-14 — the row ruling and the phrase, each doing its
        // own work.
        //
        // 2026-09-12's ruling (the abandoned band) is ROW-SCOPED renewal
        // (`Ribbon::place`), model-checked by `aterm-spec`'s
        // `ribbon_row_hold_model` (`OnlyOwnerRenews`, whose named falsifier
        // is a global hold): row 2's cohort stops being renewed the moment
        // the hand is on row 3, keeps exactly the clock it had, and leaves
        // through its own swoosh after ITS last key — which is how the
        // owner's 0.83.0 screenshot stopped holding a flat full band on a
        // wrapped line's first row while the caret typed on its second.
        //
        // Rainbow Path v3 §2.3 (S6) adds the PHRASE on top of it, and the
        // phrase is what the merge keeps: a wrap fold's new-row cohort is
        // minted while the phrase is open, so both rows carry ONE phrase id
        // and one grace (the hand's rest), and a cohort of a CLOSED phrase
        // on the hand's own row is no longer renewed by it — the row law
        // renewed that one. The phrase narrows the hold; it does not widen
        // it back across rows.
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        let row = |r: u16, col0: u16, n: u16| Keys {
            g: geom(),
            row: r,
            col0,
            n,
            period_ms: 100,
            disp: 0.9,
        };
        // Twenty keys on row 2 (the last at +1900 ms), then the hand is on
        // row 3 from +2000 ms — 100 ms later, inside the phrase's rest.
        type_keys(&mut rib, t0, row(2, 0, 20), &c);
        let left_row_2 = at(t0, 1900);
        type_keys(&mut rib, at(t0, 2000), row(3, 0, 5), &c);
        let open = rib
            .phrase_open()
            .expect("the phrase is open under the hand");
        let row2 = rib
            .cohorts()
            .iter()
            .find(|c| c.row == 2)
            .expect("row 2 is still resident inside its own grace at +2.4 s");
        let row3 = rib
            .cohorts()
            .iter()
            .find(|c| c.row == 3)
            .expect("row 3 cohort");
        // THE PHRASE: one breath, one id, one grace.
        assert_eq!(row2.phrase, open, "the fold's first row is the phrase's");
        assert_eq!(row3.phrase, open, "…and so is the row the fold opened");
        assert_eq!(
            (row2.grace_s, row3.grace_s),
            (rib.rest_s(), rib.rest_s()),
            "both rows keep the rest the hand is playing"
        );
        // THE ROW: each keeps its own clock.
        assert_eq!(
            row2.alive_at, left_row_2,
            "a key on row 3 must not renew row 2's clock"
        );
        assert_eq!(
            row3.alive_at,
            at(t0, 2400),
            "…and the key holds its own row"
        );
        assert!(
            rib.cells().iter().any(|l| l.row == 2),
            "row 2 keeps the life it had: lit through its own grace"
        );
        // Fifteen more keys on row 3, +2500 … +3900 ms.
        type_keys(&mut rib, at(t0, 2500), row(3, 5, 15), &c);
        assert!(
            !rib.cells().iter().any(|l| l.row == 2),
            "row 2 left one swoosh after ITS last key, under a hand still typing on row 3"
        );
        assert!(
            rib.cells().iter().any(|l| l.row == 3),
            "…while row 3 is still lit under the hand"
        );
        // …and row 3 leaves one swoosh after its own last key — measured
        // against the PHRASE's own total, not the old 1.54 s literal.
        let total = (SWOOSH_TOTAL_S * 1000.0).round() as u64;
        let cx = ctx(at(t0, 3900 + total - 40), &c, (3, 20), 0.0);
        rib.plan(&cx);
        assert!(
            rib.cells().iter().any(|l| l.row == 3),
            "row 3 is on glass 40 ms before its swoosh ends"
        );
        let cx = ctx(at(t0, 3900 + total + 40), &c, (3, 20), 0.0);
        rib.plan(&cx);
        assert!(
            rib.at_rest(),
            "…and leaves {SWOOSH_TOTAL_S} s after its own last key"
        );
    }

    /// **S6, the other half: a phrase holds only its own.** A cohort of a
    /// CLOSED phrase on the same row — typed, then a rest — is not renewed by
    /// a new phrase's keys elsewhere on the row and goes out on its own
    /// clock; a lay ADJACENT to it (the hand resuming the line) re-assigns it
    /// to the open phrase, and it is held again.
    #[test]
    fn a_new_phrase_holds_none_of_a_closed_one_s_cohorts_until_the_hand_resumes_it() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        let keys = |col0: u16, n: u16| Keys {
            g: geom(),
            row: 2,
            col0,
            n,
            period_ms: 100,
            disp: 0.9,
        };
        // "abc" at cols 2..5, the last key at +200 ms; a 1.2 s rest closes
        // the phrase (the floor is 0.90 s).
        type_keys(&mut rib, t0, keys(2, 3), &c);
        let first = rib.phrase_open().expect("open");
        let abc = rib.cohorts()[0];
        assert_eq!(abc.phrase, first);
        // A new phrase at cols 20.., not adjacent: the old cohort is left
        // with the clock it had.
        type_keys(&mut rib, at(t0, 1400), keys(20, 3), &c);
        let second = rib.phrase_open().expect("open");
        assert_ne!(second, first, "a rest closed the first phrase");
        let abc_now = rib
            .cohorts()
            .iter()
            .find(|k| k.id == abc.id)
            .expect("abc is still resident inside its swoosh");
        assert_eq!(
            abc_now.phrase, first,
            "the closed phrase's cohort is not re-assigned"
        );
        assert_eq!(
            abc_now.alive_at,
            at(t0, 200),
            "…and not renewed by the new phrase's keys"
        );
        assert_ne!(abc_now.phase, Phase::Laying, "it is on its way out");
        // The hand resumes the old line at col 5, adjacent to "abc", at
        // +1700 ms: still inside the chain window, so the lay joins the old
        // cohort — which re-joins the open phrase and is held from here.
        let now = at(t0, 1700);
        let cx = ctx(now, &c, (2, 6), 0.9);
        rib.on_event(&typed(), now, &cx);
        rib.plan(&cx);
        let abc_now = rib
            .cohorts()
            .iter()
            .find(|k| k.id == abc.id)
            .expect("the resumed cohort");
        assert_eq!(
            abc_now.phrase, second,
            "resuming a line re-joins the open phrase"
        );
        assert_eq!(abc_now.alive_at, now);
        assert_eq!(abc_now.phase, Phase::Laying);
    }

    /// **The after-glow's other complaint (audit #9, s10): a Backspace run
    /// started the swoosh.** An erase holds the open phrase — the hand is on
    /// it — so through a run of eight Backspaces at 120 ms the band's LEFT
    /// end stays lit and its clock follows the erases; the swoosh begins one
    /// rest after the LAST erase. An erase after the rest has passed holds
    /// nothing: the phrase is closed and the band is already leaving.
    #[test]
    fn a_backspace_run_holds_the_phrase_so_the_left_end_stays_lit_through_it() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 2, 20, &c, 0.9);
        let last = at(t0, 19 * 60);
        let coh = rib.cohorts()[0];
        // Eight Backspaces from +100 ms, 120 ms apart: caret 22 → 14.
        let mut caret = 22u16;
        for i in 0..8u64 {
            caret -= 1;
            let now = at(last, 100 + i * 120);
            erase_at(&mut rib, now, (2, caret), &c);
            let k = rib
                .cohorts()
                .iter()
                .find(|k| k.id == coh.id)
                .expect("the band is held");
            assert_eq!(
                k.alive_at, now,
                "erase {i}: the phrase's clock follows the hand"
            );
            assert_eq!(k.phase, Phase::Laying);
            let cx = ctx(now, &c, (2, caret), 0.9);
            let left = rib
                .cells()
                .iter()
                .find(|l| l.row == 2 && l.col == 2)
                .expect("the left end is in the pool");
            assert!(
                rib.env_of(&cx, left) > 0.99,
                "erase {i} (+{} ms after the last key): the left end is at full ({})",
                100 + i * 120,
                rib.env_of(&cx, left)
            );
        }
        let last_erase = at(last, 100 + 7 * 120);
        // Under the old law the swoosh had started at +0.75 s after the last
        // KEY, and the left end was retreating by +541 ms on glass. Here
        // the grace runs from the last ERASE: still laying at +0.80 s after
        // it, reaching at +0.95 s.
        let cx = ctx(at(last_erase, 800), &c, (2, 14), 0.0);
        rib.plan(&cx);
        let k = rib.cohorts().iter().find(|k| k.id == coh.id).expect("held");
        assert_eq!(k.phase, Phase::Grace);
        let cx = ctx(at(last_erase, 950), &c, (2, 14), 0.0);
        rib.plan(&cx);
        let k = rib.cohorts().iter().find(|k| k.id == coh.id).expect("held");
        assert_eq!(
            k.phase,
            Phase::Reaching,
            "the rest is measured from the last erase"
        );
        // …and an erase past the rest MINTS a phrase and takes the row back
        // (2026-09-14; the arm this replaces asserted the opposite and had
        // deleted the shipped revival — see `hold_phrase`).
        let before = rib.phrase_open().expect("open under the run");
        let late = at(last_erase, 1200);
        erase_at(&mut rib, late, (2, 13), &c);
        let k = rib
            .cohorts()
            .iter()
            .find(|k| k.id == coh.id)
            .expect("resident");
        assert_eq!(
            k.alive_at, late,
            "an erase after the rest takes the row's band back"
        );
        assert_eq!(k.phase, Phase::Laying);
        let now = rib.phrase_open().expect("a fresh phrase is open");
        assert_ne!(now, before, "it is a NEW phrase, not the one that rested");
        assert_eq!(
            rib.cohorts()
                .iter()
                .find(|k| k.id == coh.id)
                .expect("resident")
                .phrase,
            now,
            "…and the row's cohort was adopted into it"
        );
    }

    /// **THE HAND COMES BACK: a Backspace run begun AFTER the rest revives
    /// the band** (2026-09-14). The gesture `hold_row` was added for, and
    /// the one the first phrase spelling deleted: on v0.85.0 erasing into a
    /// band that had started its after-glow snapped it back to full and the
    /// band then outlived the last erase by a whole swoosh; on the branch as
    /// merged there was no snap at all and the tail was zero, because the
    /// first erase CLOSED the phrase and every later erase of the run then
    /// held nothing.
    ///
    /// The revival is a NEW phrase over the caret row's cohorts — the row
    /// scope is untouched (`ribbon_row_hold_model`, `OnlyOwnerRenews`), so
    /// this is not the global hold coming back.
    #[test]
    fn a_backspace_run_begun_after_the_rest_snaps_the_band_back_and_runs_the_tail_from_it() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        // Twenty keys at 10 cps on row 2, cols 2..22; last key at +1900 ms.
        type_keys(
            &mut rib,
            t0,
            Keys {
                g: geom(),
                row: 2,
                col0: 2,
                n: 20,
                period_ms: 100,
                disp: 0.9,
            },
            &c,
        );
        let id = rib.cohorts()[0].id;
        // A SECOND ROW under the SAME hand, one key later and never touched
        // again: the control that says the revival renews the caret's row
        // and nothing else (`OnlyOwnerRenews`).
        type_keys(
            &mut rib,
            at(t0, 2000),
            Keys {
                g: geom(),
                row: 5,
                col0: 2,
                n: 1,
                period_ms: 100,
                disp: 0.9,
            },
            &c,
        );
        let last = at(t0, 2000);
        let other = rib
            .cohorts()
            .iter()
            .find(|k| k.row == 5)
            .expect("row 5")
            .alive_at;
        // +1.40 s past the hand's last key: the rest (0.90 s) is spent, row
        // 2's after-glow is under way, and both rows are still resident.
        let glow = at(last, 1400);
        let cx = ctx(glow, &c, (2, 22), 0.0);
        rib.plan(&cx);
        let leaving = rib
            .cohorts()
            .iter()
            .find(|k| k.id == id)
            .expect("row 2 is still resident");
        assert!(
            leaving.phase != Phase::Laying && leaving.phase != Phase::Grace,
            "the fixture must erase into a band that is already leaving ({:?})",
            leaving.phase
        );
        let dim = lit_row(&rib, &cx, 2, 1.0).len();
        let full_env = rib
            .cells()
            .iter()
            .filter(|l| l.row == 2 && rib.env_of(&cx, l) > 0.99)
            .count();
        // THE SNAP: one Backspace, and the row is laying again at full.
        let bs0 = at(glow, 0);
        erase_at(&mut rib, bs0, (2, 21), &c);
        // Read at the erase's OWN instant — the same frame `dim` was read
        // on, so the two numbers are a before and an after of one gesture.
        let cx = ctx(bs0, &c, (2, 21), 0.0);
        let k = rib
            .cohorts()
            .iter()
            .find(|k| k.id == id)
            .expect("the band is taken back");
        assert_eq!(k.alive_at, bs0, "the clock follows the hand that came back");
        assert_eq!(k.phase, Phase::Laying);
        let back = lit_row(&rib, &cx, 2, 1.0).len();
        let back_env = rib
            .cells()
            .iter()
            .filter(|l| l.row == 2 && rib.env_of(&cx, l) > 0.99)
            .count();
        println!("revival: lit {dim} -> {back} cells, at full {full_env} -> {back_env}");
        assert!(
            back > dim && back_env > full_env,
            "the erase must SNAP the band back ({dim} -> {back} lit, {full_env} -> {back_env} at full)"
        );
        let left = rib
            .cells()
            .iter()
            .find(|l| l.row == 2 && l.col == 2)
            .expect("the left end is in the pool");
        assert!(
            rib.env_of(&cx, left) > 0.99,
            "…and the left end is back at full ({})",
            rib.env_of(&cx, left)
        );
        // THE ROW CONTROL: row 5 kept exactly the clock it had.
        assert_eq!(
            rib.cohorts()
                .iter()
                .find(|k| k.row == 5)
                .expect("row 5 is resident")
                .alive_at,
            other,
            "an erase on row 2 must not renew row 5"
        );
        // THE REST OF THE RUN, 120 ms apart, then the tail from the LAST one.
        let mut caret = 21u16;
        for i in 1..8u64 {
            caret -= 1;
            erase_at(&mut rib, at(glow, i * 120), (2, caret), &c);
        }
        let last_erase = at(glow, 7 * 120);
        let mut tail_ms = 0u64;
        for ms in (0..=2400).step_by(20) {
            let cx = ctx(at(last_erase, ms), &c, (2, caret), 0.0);
            rib.plan(&cx);
            if rib.cells().iter().any(|l| l.row == 2) {
                tail_ms = ms;
            } else {
                break;
            }
        }
        println!("revival: tail after the last erase {tail_ms} ms");
        assert!(
            tail_ms >= 1176,
            "the band must outlive the last erase by at least the 1175.8 ms v0.85.0 measured ({tail_ms} ms)"
        );
        let total = (SWOOSH_TOTAL_S * 1000.0).round() as u64;
        assert!(
            tail_ms <= total + 60,
            "…and by no more than its own swoosh ({tail_ms} ms against {total} ms)"
        );
    }

    /// **S5: the grace is the melody's rest, tempo-scaled** (D-6, k = 2.5).
    /// At 10 cps the rest is the 0.90 s floor; at 2 cps (ioi 500 ms) it is
    /// 1.25 s, so a slow hand's line is still in its grace at 1.0 s idle —
    /// where the floor would have had it reaching — and leaves 0.79 s after
    /// its own rest.
    #[test]
    fn a_slow_hand_s_grace_is_the_tempo_multiple_and_a_fast_hand_s_is_the_floor() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let fast = {
            let mut rib = Ribbon::new();
            type_run(&mut rib, t0, 2, 12, &c, 0.9);
            rib.cohorts()[0].grace_s
        };
        assert!(
            (fast - PHRASE_REST_MIN_S).abs() < 1e-6,
            "10 cps: the floor ({fast})"
        );
        let mut rib = Ribbon::new();
        let keys = Keys {
            g: geom(),
            row: 2,
            col0: 2,
            n: 12,
            period_ms: 500,
            disp: 0.6,
        };
        type_keys(&mut rib, t0, keys, &c);
        let last = at(t0, 11 * 500);
        let slow = rib.cohorts()[0].grace_s;
        assert!(
            (1.20..=1.25).contains(&slow),
            "2 cps: the EMA has the tempo near 500 ms, the rest near 1.25 s ({slow})"
        );
        let cx = ctx(at(last, 1000), &c, (2, 14), 0.0);
        rib.plan(&cx);
        assert_eq!(
            rib.cohorts()[0].phase,
            Phase::Grace,
            "at 1.0 s idle a 2 cps line is still in its grace"
        );
        let total = (swoosh_total_s(slow) * 1000.0) as u64;
        let cx = ctx(at(last, total - 40), &c, (2, 14), 0.0);
        rib.plan(&cx);
        assert!(!rib.at_rest(), "on glass 40 ms before its own swoosh ends");
        let cx = ctx(at(last, total + 40), &c, (2, 14), 0.0);
        rib.plan(&cx);
        assert!(rib.at_rest(), "…and gone 40 ms after");
    }

    /// **§2.6, the retract target and drain order per row.** A band on a
    /// row BELOW the caret (an Up-arrow recall left it) retracts toward its
    /// own LEFT end and drains right-first; one on a row ABOVE (the hand
    /// went down a line) retracts toward its right end — the fold — and
    /// drains left-first. Neither slides toward a column that belongs to
    /// another row (audit hi/s9; the refresh's s17).
    #[test]
    fn a_row_the_caret_is_not_on_retracts_into_its_own_nearest_end_in_logical_order() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let below = |to: (u16, u16)| {
            let mut rib = Ribbon::new();
            type_run(&mut rib, t0, 4, 20, &c, 0.9);
            let last = at(t0, 19 * 60);
            let now = at(last, 100);
            let cx = ctx(now, &c, to, 0.9);
            rib.on_event(&nav((2, 24), to), now, &cx);
            rib.plan(&cx);
            let k = rib.cohorts()[0];
            let env = |col: u16, ms: u64| {
                let cx = ctx(at(now, ms), &c, to, 0.0);
                let cell = rib
                    .cells()
                    .iter()
                    .find(|l| l.row == 2 && l.col == col)
                    .expect("cell");
                rib.env_of(&cx, cell)
            };
            (k, env(4, 200), env(23, 200))
        };
        // Up: the caret to row 1, col 24 — the band is now BELOW the hand.
        let (k, left, right) = below((1, 24));
        assert!(k.abandoned && k.phase.is_retracting());
        assert_eq!(
            k.retract_col,
            Some(4),
            "a row below the caret pulls into its left end"
        );
        assert!(k.drain_right_first, "…and drains from its right end");
        assert!(
            right < left,
            "200 ms into the retract the right end has spent more than the left ({right} vs {left})"
        );
        // Down: the caret to row 3, col 0 — the band is ABOVE the hand.
        let (k, left, right) = below((3, 0));
        assert_eq!(
            k.retract_col,
            Some(23),
            "a row above the caret pulls into its right end — the fold — not toward col 0 on another row"
        );
        assert!(!k.drain_right_first);
        assert!(
            left < right,
            "…and drains from its left end ({left} vs {right})"
        );
    }

    /// **A3: the after-glow of a fast line ends at the floor rest plus the
    /// swoosh** — 1.69 s after the last key, where the audit measured
    /// 931–1201 ms against a law that promised 1.54 and a grace of 0.75.
    #[test]
    fn the_after_glow_of_a_fast_line_ends_at_the_rest_plus_the_swoosh() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 2, 20, &c, 0.9);
        let last = at(t0, 19 * 60);
        let total = (SWOOSH_TOTAL_S * 1000.0).round() as u64;
        assert_eq!(total, 1690, "0.90 s rest + 0.79 s swoosh");
        let mut sink = Sink::default();
        let cx = ctx(at(last, total - 30), &c, (2, 22), 0.0);
        rib.plan(&cx);
        let mut f = sink.frame();
        rib.emit(&cx, &mut f);
        assert!(
            !sink.under.is_empty(),
            "light on glass 30 ms before the swoosh ends"
        );
        let cx = ctx(at(last, total + 30), &c, (2, 22), 0.0);
        rib.plan(&cx);
        let mut f = sink.frame();
        rib.emit(&cx, &mut f);
        assert!(
            sink.under.is_empty() && rib.at_rest(),
            "…and none 30 ms after"
        );
    }

    /// The brightest planned boundary strictly INSIDE cell `col` — its
    /// interior slabs, which interpolate the cell's own two edges and read
    /// nothing of the run's shape (a boundary's `cov` takes the brighter of
    /// the two cells' envelopes; the wave and wedge move `spine`/`up`/`dn`, never
    /// `cov`). The quads' per-row alpha is NOT this number: the strip rides a
    /// vertical profile the head's position bends, so the brightest device
    /// row of a cell can move by a few levels when the head moves — which a
    /// retirement does. `0` once the cell has no interior slab planned.
    fn cell_cov(rib: &Ribbon, g: Geom, col: u16) -> u8 {
        let x0 = f32::from(g.origin_x) + f32::from(col) * g.cw as f32;
        let x1 = x0 + g.cw as f32;
        rib.plan_segments()
            .iter()
            .filter(|s| s.x > x0 + 0.5 && s.x < x1 - 0.5)
            .map(|s| s.cov)
            .max()
            .unwrap_or(0)
    }

    #[test]
    fn a_retired_cell_melts_to_exactly_nothing_inside_150_ms_and_never_brightens() {
        // 2026-09-12, the abandoned band: `Ribbon::retire_cells` is the
        // engine's half of the host's content witness. Eight keys on row 2
        // (cells 10..=17); 40 ms after the last, the two HEAD cells are
        // reported overwritten. RED on main (no such call; the cells lived
        // on the cohort's clock), GREEN here.
        let c = cfg(true, true);
        let g = geom();
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 10, 8, &c, 0.9);
        let last = at(t0, 7 * 60);
        let fired = at(last, 40);
        let cx = ctx(fired, &c, (2, 18), 0.9);
        rib.plan(&cx);
        assert_eq!(
            rib.slabs_per_cell(),
            SLABS_PER_CELL,
            "interior slabs exist to read"
        );
        let before = (cell_cov(&rib, g, 16), cell_cov(&rib, g, 17));
        assert!(
            before.0 > 0 && before.1 > 0,
            "the head cells are lit: {before:?}"
        );
        let mut control = rib.clone();
        let born_of = |rib: &Ribbon, col: u16| {
            rib.cells()
                .iter()
                .find(|l| l.row == 2 && l.col == col)
                .map(|l| l.born)
                .expect("laid")
        };
        let (b16, b17) = (born_of(&rib, 16), born_of(&rib, 17));
        assert_eq!(
            rib.retire_cells(&[(2, 15, fired)], fired),
            0,
            "a cell at a laid position with ANOTHER birth is not it (identity, not position)"
        );
        assert_eq!(
            rib.retire_cells(
                &[(2, 16, b16), (2, 17, b17), (2, 99, fired), (5, 16, b16)],
                fired
            ),
            2,
            "two live cells stamped; a cell that is not laid is not counted"
        );
        assert_eq!(
            rib.retire_cells(&[(2, 16, b16)], at(fired, 8)),
            0,
            "a retired cell is not restamped"
        );
        let mut prev = before;
        let mut gone_by: Option<u64> = None;
        for k in 1..=20u64 {
            let ms = 8 * k;
            let now = at(fired, ms);
            let cx = ctx(now, &c, (2, 18), 0.9);
            rib.plan(&cx);
            control.plan(&cx);
            let here = (cell_cov(&rib, g, 16), cell_cov(&rib, g, 17));
            assert!(
                here.0 <= prev.0 && here.1 <= prev.1,
                "+{ms} ms: the melt is monotone, got {here:?} after {prev:?}"
            );
            assert!(
                here.0 <= before.0 && here.1 <= before.1,
                "+{ms} ms: never brighter than the cell was ({before:?}), got {here:?}"
            );
            prev = here;
            let resident = rib
                .cells()
                .iter()
                .any(|l| l.row == 2 && (l.col == 16 || l.col == 17));
            if here == (0, 0) && !resident && gone_by.is_none() {
                gone_by = Some(ms);
            }
            // Cells two or more columns from the retired pair share no
            // boundary with it: byte-identical to the control that never
            // retired anything.
            for col in [10u16, 11, 12, 13] {
                assert_eq!(
                    cell_cov(&rib, g, col),
                    cell_cov(&control, g, col),
                    "+{ms} ms: cell {col} is untouched by the retirement"
                );
            }
        }
        assert!(
            gone_by.is_some_and(|ms| ms <= 150),
            "off the glass and out of the pool inside 150 ms, got {gone_by:?}"
        );
        assert!(
            rib.cells().iter().any(|l| l.row == 2 && l.col == 15),
            "the un-retired band is still resident"
        );
    }

    /// **A RELEASED RUN LEAVES THROUGH THE SWOOSH'S RETRACT, UNSTAMPED**
    /// (2026-09-14, the new line's fade). Six typed cells, the last one
    /// backspaced (retracting on its own 0.24 s clock); the witness names
    /// all six as blanked. The five live cells are counted and not one of
    /// them is stamped: their cohort is abandoned into its retract — not
    /// rejoinable, the text is gone — and the retracting cell keeps the
    /// clock it has. The retract's own span later the row is out of the
    /// pool: its light was drawn into the hand over `RETRACT_DUR_S +
    /// RETRACT_FADE_S`, never cut on `RETIRE_MELT_S`.
    #[test]
    fn a_released_run_is_abandoned_into_its_retract_and_a_cell_already_leaving_keeps_its_clock() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 10, 6, &c, 0.9);
        let last = at(t0, 5 * 60);
        // Backspace: cell 15 retracts on its own 0.24 s clock.
        erase_at(&mut rib, at(last, 50), (2, 15), &c);
        let stamp_15 = rib
            .cells()
            .iter()
            .find(|l| l.row == 2 && l.col == 15)
            .and_then(|l| l.retract_at);
        assert!(stamp_15.is_some(), "the erased cell is retracting");
        let named: Vec<(u16, u16, Instant)> = rib
            .cells()
            .iter()
            .filter(|l| l.row == 2)
            .map(|l| (l.row, l.col, l.born))
            .collect();
        let released = rib.release_cells(&named, at(last, 60));
        assert_eq!(
            released, 5,
            "the five live cells; the retracting one keeps its clock"
        );
        assert!(
            rib.cells()
                .iter()
                .all(|l| l.row != 2 || l.retire_at.is_none()),
            "not one cell is stamped on the melt"
        );
        assert_eq!(
            rib.cells()
                .iter()
                .find(|l| l.row == 2 && l.col == 15)
                .and_then(|l| l.retract_at),
            stamp_15,
            "the retracting cell keeps its own clock"
        );
        let coh = rib
            .cohorts()
            .iter()
            .find(|k| k.row == 2)
            .copied()
            .expect("the row's cohort");
        assert!(coh.abandoned, "the cohort is abandoned");
        assert!(!coh.rejoinable, "…and not rejoinable: the text is gone");
        let cx = ctx(at(last, 76), &c, (2, 15), 0.9);
        rib.plan(&cx);
        assert!(
            rib.cohorts()
                .iter()
                .find(|k| k.row == 2)
                .is_some_and(|k| k.phase.is_retracting()),
            "on the next plan the cohort is in its retract"
        );
        assert!(
            rib.cells().iter().filter(|l| l.row == 2).count() >= 5,
            "…and the five cells are still resident, drawn in, not cut"
        );
        // A second release of the same cells restarts nothing: the clock
        // the cohort has is the clock it keeps.
        let alive_at = rib
            .cohorts()
            .iter()
            .find(|k| k.row == 2)
            .map(|k| k.alive_at);
        rib.release_cells(&named, at(last, 90));
        assert_eq!(
            rib.cohorts()
                .iter()
                .find(|k| k.row == 2)
                .map(|k| k.alive_at),
            alive_at,
            "a repeat release does not rewind the clock again"
        );
        let cx = ctx(
            at(
                last,
                60 + ((RETRACT_DUR_S + RETRACT_FADE_S) * 1000.0) as u64 + 20,
            ),
            &c,
            (2, 15),
            0.9,
        );
        rib.plan(&cx);
        assert!(
            !rib.cells().iter().any(|l| l.row == 2),
            "the released run is out of the pool the retract's own span later, \
             not {RETIRE_MELT_S} s later"
        );
    }

    #[test]
    fn a_relay_over_a_content_retired_cell_has_a_fresh_witness_identity() {
        use super::super::witness::{RowSample, Witness};

        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 10, 2, &c, 0.9);
        let born = |rib: &Ribbon, col| {
            rib.cells()
                .iter()
                .find(|cell| cell.row == 2 && cell.col == col)
                .expect("typed cell")
                .born
        };
        let old = born(&rib, 10);
        let neighbor = born(&rib, 11);
        let mut witness = Witness::new();
        let mut retired = Vec::new();
        let mut released = Vec::new();
        let original: Vec<char> = "          ab".chars().collect();
        witness.walk(
            rib.cells(),
            &[RowSample {
                row: 2,
                cols: &original,
            }],
            &mut retired,
            &mut released,
        );
        assert!(retired.is_empty());
        let fired = at(t0, 100);
        assert_eq!(rib.retire_cells(&[(2, 10, old)], fired), 1);
        let fresh = at(t0, 116);
        rib.on_event(&typed(), fresh, &ctx(fresh, &c, (2, 11), 0.9));
        assert_eq!(born(&rib, 10), fresh, "a retired identity cannot be re-wet");
        assert_eq!(born(&rib, 11), neighbor, "the neighboring key is untouched");
        assert_eq!(rib.retire_cells(&[(2, 10, old)], fresh), 0);

        // Even an old witness still resident at this position must arm the
        // new glyph under its new birth, not retire it as the old overwrite.
        let replacement: Vec<char> = "          Xb".chars().collect();
        witness.walk(
            rib.cells(),
            &[RowSample {
                row: 2,
                cols: &replacement,
            }],
            &mut retired,
            &mut released,
        );
        assert!(retired.is_empty(), "the fresh key arms its own record");
        let changed: Vec<char> = "          Yb".chars().collect();
        witness.walk(
            rib.cells(),
            &[RowSample {
                row: 2,
                cols: &changed,
            }],
            &mut retired,
            &mut released,
        );
        assert_eq!(
            retired,
            vec![(2, 10, fresh)],
            "later changes retire the new identity"
        );
    }

    #[test]
    fn a_relocation_pays_a_content_retired_destination_and_preserves_live_neighbors() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 10, 3, &c, 0.9);
        let before: Vec<_> = rib
            .cells()
            .iter()
            .map(|cell| (cell.col, cell.born))
            .collect();
        let old = before
            .iter()
            .find(|&&(col, _)| col == 11)
            .expect("middle")
            .1;
        let fired = at(t0, 160);
        assert_eq!(rib.retire_cells(&[(2, 11, old)], fired), 1);
        let fresh = at(t0, 176);
        rib.relay_word(2, 10, 13, fresh);
        for col in 10..13 {
            let cell = rib
                .cells()
                .iter()
                .find(|cell| cell.row == 2 && cell.col == col)
                .expect("relocated word");
            assert!(
                !cell.leaving(),
                "column {col} belongs to the relocated word"
            );
            let expected = if col == 11 {
                fresh
            } else {
                before.iter().find(|&&(c, _)| c == col).expect("neighbor").1
            };
            assert_eq!(
                cell.born, expected,
                "only the retired destination is paid again"
            );
        }
        assert_eq!(
            rib.retire_cells(&[(2, 11, old)], fresh),
            0,
            "stale identity has no authority over the relocation"
        );
    }

    #[test]
    fn a_live_retype_with_a_changed_glyph_does_not_inherit_the_old_witness() {
        use super::super::witness::{RowSample, Witness};

        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 10, 1, &c, 0.9);
        let mut witness = Witness::new();
        let mut retired = Vec::new();
        let mut released = Vec::new();
        let original: Vec<char> = "          a".chars().collect();
        witness.walk(
            rib.cells(),
            &[RowSample {
                row: 2,
                cols: &original,
            }],
            &mut retired,
            &mut released,
        );
        assert!(retired.is_empty());
        let before_ctx = ctx(at(t0, 99), &c, (2, 11), 0.9);
        rib.plan(&before_ctx);
        let before = cell_cov(&rib, geom(), 10);
        assert!(before > 0, "the old glyph is visibly lit");
        let old_attack = rib.cells()[0].attack_at;
        let fresh = at(t0, 100);
        rib.on_event(&typed(), fresh, &ctx(fresh, &c, (2, 11), 0.9));
        let after_ctx = ctx(at(t0, 101), &c, (2, 11), 0.9);
        rib.plan(&after_ctx);
        assert_eq!(rib.cells()[0].born, fresh, "fresh key, fresh identity");
        assert_eq!(rib.cells()[0].attack_at, old_attack);
        assert!(
            cell_cov(&rib, geom(), 10) >= before,
            "re-wetting cannot dim the old light"
        );
        assert!(
            !rib.brisk(after_ctx.now),
            "a settled attack is not restarted for scheduling either"
        );
        let replacement: Vec<char> = "          X".chars().collect();
        witness.walk(
            rib.cells(),
            &[RowSample {
                row: 2,
                cols: &replacement,
            }],
            &mut retired,
            &mut released,
        );
        assert!(
            retired.is_empty(),
            "a genuine retyped glyph must not be mistaken for an unlicensed overwrite: {retired:?}"
        );
    }

    #[test]
    fn focus_regained_inside_the_ember_comes_back_up_through_edge_in_not_a_snap() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 10, 6, &c, 0.8);
        let last = at(t0, 5 * 60);
        let cx = ctx(at(last, 100), &c, (2, 16), 0.8);
        rib.plan(&cx);
        let full = plan_peak(&rib);
        let full_sum = plan_sum(&rib);
        rib.on_event(&Event::Focus(false), at(last, 100), &cx);
        let cx = ctx(at(last, 250), &c, (2, 16), 0.8);
        rib.plan(&cx);
        let dimmed = plan_peak(&rib);
        let dim_sum = plan_sum(&rib);
        // RESTATED 2026-09-14 (the per-stop ember floor, and again the same
        // day when the floor became a CONTRAST). The ember dims each stop
        // into its OWN range, and that range is a fact about the theme, not
        // about the ember: the bed's bar and the colour floor are 236 and
        // 193 apart for a chartreuse stop on the default ground, so a band
        // of warm stops can only lose a sixth of its light before one of
        // them is off the glass. "Well under way" therefore has to be said
        // in units of the RANGE THE BAND HAS, which is measured here and not
        // assumed: a second ribbon laid by the same fixture, embered to its
        // last frame, is what this band's light is on its way to.
        let mut twin = Ribbon::new();
        type_run(&mut twin, t0, 10, 6, &c, 0.8);
        let blur_cx = ctx(at(last, 100), &c, (2, 16), 0.8);
        twin.on_event(&Event::Focus(false), at(last, 100), &blur_cx);
        let end_cx = ctx(at(last, 399), &c, (2, 16), 0.8);
        twin.plan(&end_cx);
        let floor_sum = plan_sum(&twin);
        let range = full_sum.saturating_sub(floor_sum);
        let burned = full_sum.saturating_sub(dim_sum);
        println!(
            "ember/regain: {full_sum} → {dim_sum} → {floor_sum} of light \
             (range {range}, burned {burned} = {:.0}% at u=0.5, spend says {:.0}%)",
            100.0 * burned as f32 / range.max(1) as f32,
            100.0 * (1.0 - spend(0.5))
        );
        assert!(
            dimmed < full,
            "the ember must be burning ({full} → {dimmed})"
        );
        assert!(
            range > 0 && floor_sum > 0,
            "the fixture must leave the band SOME range to spend and some light at the floor \
             ({full_sum} → {floor_sum})"
        );
        // `spend` is `(1 − u)²`, so at the halfway instant exactly three
        // quarters of whatever range the band has is already gone. Within a
        // twentieth, which is the rounding of a coverage byte per cell.
        assert!(
            burned as f32 >= range as f32 * (1.0 - spend(0.5) - 0.05),
            "the ember must be well under way: {burned} of {range} spendable \
             ({full_sum} → {dim_sum}, floor {floor_sum})"
        );
        rib.on_event(&Event::Focus(true), at(last, 250), &cx);
        let cx = ctx(at(last, 251), &c, (2, 16), 0.8);
        rib.plan(&cx);
        let back = plan_peak(&rib);
        assert!(
            back <= dimmed + 3,
            "focus regain snapped the ribbon from {dimmed} to {back} in one frame"
        );
        let cx = ctx(at(last, 290), &c, (2, 16), 0.8);
        rib.plan(&cx);
        let rearmed = plan_peak(&rib);
        assert!(
            rearmed + 3 >= full,
            "after the edge-in the ribbon must be back at its level ({full} vs {rearmed})"
        );
    }

    // -- THE FOCUS EMBER LEAVES AS ONE OBJECT (2026-09-14) -----------------

    /// **THE PAINT CENSUS'S OWN RULE, NOT THIS MODULE'S** — `ribscan`, the
    /// rig that measured this defect on glass, calls a pixel PAINT when
    /// `max(r, g, b) >= 60 && max - min >= 40`, and a cell LIT when a fifth
    /// of its bed pixels are paint. On a light theme the mark is a dark
    /// saturated pixel on bright paper, so the level half is read in the
    /// paper's polarity (`min <= 255 - 60`) and the chroma half — a property
    /// of the pixel alone — is unchanged.
    ///
    /// Stated in ABSOLUTE BYTES, and deliberately NOT in terms of [`seen`].
    /// The first spelling of this twin judged lit-ness with `seen` — the
    /// very predicate [`seen_floor`] inverts — so its pass was guaranteed by
    /// construction: it went on passing while the shipped binary opened
    /// 33-cell dark runs on the default theme. A pin must not be written in
    /// the thing it is pinning.
    fn census_paint(px: u32, light: bool) -> bool {
        let c = |sh: u32| f32::from(((px >> sh) & 0xff) as u8);
        let (r, g, b) = (c(16), c(8), c(0));
        let (mx, mn) = (r.max(g).max(b), r.min(g).min(b));
        let level = if light {
            mn <= 255.0 - 60.0
        } else {
            mx >= 60.0
        };
        level && mx - mn >= 40.0
    }

    /// Columns of `row` whose BED composites as PAINT over the theme's own
    /// ground ([`census_paint`], the rig's instrument read inside the module
    /// against the same bytes the renderer will send) at `scale` times the
    /// coverage the ribbon reports on this frame.
    fn lit_row(rib: &Ribbon, cx: &Ctx<'_>, row: u16, scale: f32) -> Vec<u16> {
        let bg = cx.cfg.theme_bg;
        let light = !cx.cfg.dark_theme;
        let mut out: Vec<u16> = Vec::new();
        for cell in rib.cells().iter().filter(|c| c.row == row) {
            let a = (rib.cov_of(cx, cell) * scale).round().clamp(0.0, 255.0) as u8;
            if a > 0 && census_paint(over_premul(bg, premul_rgb(rib.ink.at(cell.t), a), a), light) {
                out.push(cell.col);
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    /// The longest DARK run strictly inside `lit`'s own span — the rig's
    /// `ribscan` verdict, in cells.
    fn dark_run(lit: &[u16]) -> u16 {
        let (mut worst, mut run) = (0u16, 0u16);
        let (Some(&first), Some(&last)) = (lit.first(), lit.last()) else {
            return 0;
        };
        for col in first..=last {
            if lit.contains(&col) {
                run = 0;
            } else {
                run += 1;
                worst = worst.max(run);
            }
        }
        worst
    }

    /// **THE BAND LEAVES AS ONE OBJECT.** Through every frame of the focus
    /// ember the PAINTED cells of a band are ONE interval — no dark run
    /// inside it — and the whole band goes out in the frame the ember ends.
    ///
    /// The defect this pins (measured on glass 2026-09-14: 3 of 21 frames of
    /// the blur window held a dark run of 5 to 8 cells on Nord, and 8 of 30
    /// frames a run of up to 33 on the default theme, against 0 of 30 on
    /// v0.85.0, which has no ember at all) is NOT a tear and not a missing
    /// cell: it is the bed's own arithmetic. [`bed_ink`] puts every stop on
    /// one composited LUMINANCE — the legibility bar — and therefore NOT on
    /// one distance from the ground, so one shared fade takes the warm third
    /// off the glass while its neighbours are still bright and the band
    /// reads as two pieces with a hole.
    ///
    /// **Judged by [`census_paint`] and NOT by [`seen`]**, which is the whole
    /// difference between this spelling and the one it replaces. The first
    /// spelling asked `seen` whether a cell was lit — the predicate
    /// [`seen_floor`] inverts — so the ember's exit level was compared with
    /// the definition of the exit level, and the answer was yes by
    /// construction on any theme, including the one where the shipped binary
    /// was opening 33-cell holes. Here the fixture's own arm, and the rig on
    /// glass, use the same instrument and neither of them is this module's.
    ///
    /// Two falsifiers, so a fixture that cannot fail cannot pass: the shipped
    /// PRE-EMBER fade (a bare `spend` over the whole band) reconstructed on
    /// the same frames, which splits the band by 21 cells over 26 frames; and
    /// — when the floor is reverted to the 24-byte lift this replaced — this
    /// arm itself, which goes red on the default ground at 33 cells, the same
    /// number the glass census reported.
    #[test]
    fn the_focus_ember_never_opens_a_dark_run_inside_the_band() {
        for (theme, fg, bg) in [
            ("default #111318", SHIPPED_FG, SHIPPED_BG),
            ("Nord", NORD_FG, NORD_BG),
        ] {
            let mut c = cfg(true, true);
            c.theme_fg = fg;
            c.theme_bg = bg;
            let t0 = Instant::now();
            let mut rib = Ribbon::new();
            // SIXTY cells, so the walk folds twice and the warm third lands
            // in the MIDDLE of the band as well as at its ends: a hole the
            // scanner can see has to have lit cells on both sides of it.
            type_keys(
                &mut rib,
                t0,
                Keys {
                    g: geom(),
                    row: 2,
                    col0: 2,
                    n: 60,
                    period_ms: 40,
                    disp: 0.9,
                },
                &c,
            );
            let last = at(t0, 59 * 40);
            let caret = (2u16, 62u16);
            let cx = ctx(at(last, 60), &c, caret, 0.9);
            rib.plan(&cx);
            let full = lit_row(&rib, &cx, 2, 1.0);
            assert!(
                full.len() >= 40,
                "{theme}: the fixture's band is lit before the blur ({} cells)",
                full.len()
            );
            assert_eq!(dark_run(&full), 0, "{theme}: the fixture starts whole");
            let blur = at(last, 60);
            rib.on_event(&Event::Focus(false), blur, &cx);
            let (mut worst_new, mut worst_old, mut old_split_frames) = (0u16, 0u16, 0u32);
            let mut shrunk_while_lit = Vec::new();
            for ms in (0..=300).step_by(4) {
                let now = at(blur, ms);
                let cx = ctx(now, &c, caret, 0.0);
                rib.plan(&cx);
                let burned = ms as f32 / 1000.0 / FOCUS_EMBER_S;
                let now_lit = lit_row(&rib, &cx, 2, 1.0);
                // THE PRE-FIX EMBER, EXACTLY: the same frame with the ember
                // lifted off the envelope, scaled by the bare `spend` that
                // was this module's whole ember law before the per-stop
                // floor. Reconstruction, not a model.
                let burning = rib.ember_at.take();
                let old_lit = lit_row(&rib, &cx, 2, spend(clamp01(burned)));
                rib.ember_at = burning;
                worst_new = worst_new.max(dark_run(&now_lit));
                let od = dark_run(&old_lit);
                worst_old = worst_old.max(od);
                if od >= 2 {
                    old_split_frames += 1;
                }
                if !now_lit.is_empty() && now_lit != full {
                    shrunk_while_lit.push((ms, now_lit.len()));
                }
            }
            assert_eq!(
                worst_new, 0,
                "{theme}: the ember opened a dark run of {worst_new} cells inside the band"
            );
            assert!(
                shrunk_while_lit.is_empty(),
                "{theme}: cells left the band before the end of the ember: {shrunk_while_lit:?}"
            );
            let gone = ctx(at(blur, 301), &c, (2, 62), 0.0);
            rib.plan(&gone);
            assert!(
                lit_row(&rib, &gone, 2, 1.0).is_empty(),
                "{theme}: the ember's end takes every cell"
            );
            // THE FALSIFIER: the pre-fix ember splits this same band.
            assert!(
                worst_old >= 2 && old_split_frames >= 2,
                "{theme}: the fixture cannot show the defect it pins \
                 (worst pre-fix dark run {worst_old} cells over {old_split_frames} frames)"
            );
            println!(
                "ember/{theme}: shipped worst dark run {worst_new}; \
                 pre-fix {worst_old} cells over {old_split_frames} frames"
            );
        }
    }

    /// Every ground aterm SHIPS, read out of the product's own theme table:
    /// `ColorScheme::default` — the theme a config with no `theme` line gets
    /// — plus every built-in, dark and light, each with the `dark_theme`
    /// flag the GUI would give it (`aterm_render::theme_is_dark`). Read, not
    /// listed, so a theme added to the product cannot be a theme nobody ran.
    fn shipped_grounds() -> Vec<(String, u32, u32, bool)> {
        let rgb =
            |c: aterm_types::Rgb| (u32::from(c.r) << 16) | (u32::from(c.g) << 8) | u32::from(c.b);
        let d = aterm_types::scheme::ColorScheme::default();
        let mut out = vec![("Default".to_string(), rgb(d.foreground), rgb(d.background))];
        out.extend(
            aterm_types::scheme::builtin_names()
                .into_iter()
                .filter_map(|n| {
                    aterm_types::scheme::builtin(n)
                        .map(|s| (n.to_string(), rgb(s.foreground), rgb(s.background)))
                }),
        );
        // `builtin_names` carries "Default" too; one ground, one row.
        let mut seen_bg: Vec<u32> = Vec::new();
        out.into_iter()
            .filter(|(_, _, bg)| {
                let first = !seen_bg.contains(bg);
                seen_bg.push(*bg);
                first
            })
            .map(|(n, fg, bg)| (n, fg, bg, aterm_render::theme_is_dark(bg)))
            .collect()
    }

    /// **THE FLOOR MEANS ONE THING ON EVERY THEME THAT SHIPS.** For every
    /// ground in the product's own table, every stop the bed's bar leaves as
    /// PAINT at full coverage ([`census_paint`], the rig's instrument — NOT
    /// [`seen`]) is still paint at the coverage the ember leaves it at. The
    /// ember may dim a stop; it may not be what takes it off the glass.
    ///
    /// This is the twin the 24-byte lift did not have. That constant was
    /// measured on Nord, proved on Nord, and shipped — and it is the STRICTER
    /// of its two halves on Nord (24 bytes off `#2E3440` leaves the band at a
    /// peak of 71..90, over the census's 60) and the LOOSER on the default
    /// `#111318` (50..59, under it). The second arm here reconstructs
    /// it and requires it to FAIL, so the sweep has a red arm and a green one
    /// on the same fixture.
    #[test]
    fn the_ember_never_takes_a_stop_under_the_census_on_any_shipped_ground() {
        /// The 24-byte lift this module shipped for one day, reconstructed.
        fn abs_lift_floor(ink: u32, bg: u32) -> f32 {
            let ch = |c: u32, sh: u32| f32::from(((c >> sh) & 0xff) as u8);
            let old_seen = |px: u32| {
                let (r, g, b) = (ch(px, 16), ch(px, 8), ch(px, 0));
                let lift = (r - ch(bg, 16))
                    .abs()
                    .max((g - ch(bg, 8)).abs())
                    .max((b - ch(bg, 0)).abs());
                lift >= 24.0 && r.max(g).max(b) - r.min(g).min(b) >= 40.0
            };
            (1..=255u16)
                .rev()
                .map(|a| u8::try_from(a).unwrap_or(255))
                .find(|&a| !old_seen(over_premul(bg, premul_rgb(ink, a), a)))
                .map_or(1.0, |a| f32::from(a) + 1.0)
        }
        let mut old_broke = Vec::new();
        let mut grounds = 0u32;
        for (name, fg, bg, dark) in shipped_grounds() {
            let mut c = cfg(dark, true);
            c.theme_fg = fg;
            c.theme_bg = bg;
            let mut lut = BedInkLut::default();
            lut.sync(&c);
            let cap = cov_cap(&c);
            let light = !dark;
            let exit = |ink: u32, floor: f32| {
                // `EMBER_FLOOR_MARGIN` over the floor, clipped to the bed's
                // own ceiling: a floor over the ceiling is "hold at full".
                let a = (floor * EMBER_FLOOR_MARGIN).clamp(0.0, cap) as u8;
                over_premul(bg, premul_rgb(ink, a), a)
            };
            let level = |px: u32| {
                let c = |sh: u32| f32::from(((px >> sh) & 0xff) as u8);
                if light {
                    c(16).min(c(8)).min(c(0))
                } else {
                    c(16).max(c(8)).max(c(0))
                }
            };
            let (mut lit_at_bar, mut lost_new, mut lost_old) = (0u32, Vec::new(), Vec::new());
            let (mut lo, mut hi) = (f32::INFINITY, 0.0f32);
            for i in 0..BED_INK_LUT_LEN {
                let t = i as f32 / (BED_INK_LUT_LEN - 1) as f32;
                let ink = lut.at(t);
                let cov = cap as u8;
                if !census_paint(over_premul(bg, premul_rgb(ink, cov), cov), light) {
                    // The BAR already leaves this stop under the instrument
                    // at full coverage (Solarized Dark's foreground does it
                    // to half the arc). Not the ember's to fix, and the
                    // floor lands over the whole coverage there, so the stop
                    // holds until the pool clears.
                    continue;
                }
                lit_at_bar += 1;
                let px = exit(ink, lut.seen_floor_at(t));
                lo = lo.min(level(px));
                hi = hi.max(level(px));
                if !census_paint(px, light) {
                    lost_new.push((t, px));
                }
                if !census_paint(exit(ink, abs_lift_floor(ink, bg)), light) {
                    lost_old.push(t);
                }
            }
            grounds += 1;
            println!(
                "ground/{name} bg=#{bg:06X} {} — {lit_at_bar} of {BED_INK_LUT_LEN} stops lit at the bar, \
                 exit level {lo:.0}..{hi:.0}, lost by the floor {} (by the 24-byte lift {})",
                if dark { "dark" } else { "light" },
                lost_new.len(),
                lost_old.len()
            );
            assert!(
                lost_new.is_empty(),
                "{name}: the ember took {} stops the bar left lit under the paint census — \
                 {:?}",
                lost_new.len(),
                lost_new
                    .iter()
                    .map(|(t, px)| format!("t={t:.2} #{px:06X}"))
                    .take(6)
                    .collect::<Vec<_>>()
            );
            if !lost_old.is_empty() {
                old_broke.push((name, lost_old.len(), lit_at_bar));
            }
        }
        assert!(
            grounds >= 12,
            "the product's theme table went missing ({grounds} grounds)"
        );
        // THE RED ARM: the constant this replaced is not a statement about
        // visibility at all — it loses most of the arc on the theme the
        // product opens with, and the whole point is that no fixture stated
        // in `seen` could ever have said so.
        println!(
            "the 24-byte lift loses stops on {} of {grounds} shipped grounds: {old_broke:?}",
            old_broke.len()
        );
        let default_lost = old_broke
            .iter()
            .find(|(n, _, _)| n == "Default")
            .map_or(0, |(_, lost, _)| *lost);
        assert!(
            default_lost >= 100,
            "the 24-byte lift must be REFUTED on the default ground for this sweep to have a red \
             arm; it lost {default_lost} stops"
        );
    }

    /// **THE ARITHMETIC BEHIND THE FLOOR, PRINTED.** The bed's bar equalises
    /// the arc's LUMINANCE, not its distance from the ground, so the alpha at
    /// which a stop stops being [`seen`] differs across the arc by a factor
    /// of four or more — which is exactly how much of the fade a shared
    /// multiplier gets wrong.
    #[test]
    fn the_warm_stops_go_out_at_several_times_the_coverage_the_cool_ones_do() {
        for (theme, fg, bg) in [
            ("default #111318", SHIPPED_FG, SHIPPED_BG),
            ("Nord", NORD_FG, NORD_BG),
        ] {
            let mut c = cfg(true, true);
            c.theme_fg = fg;
            c.theme_bg = bg;
            let mut lut = BedInkLut::default();
            lut.sync(&c);
            let mut rows = Vec::new();
            for i in 0..12u32 {
                let t = i as f32 / 12.0;
                rows.push((t, lut.at(t), lut.seen_floor_at(t)));
            }
            let lo = rows.iter().map(|r| r.2).fold(f32::INFINITY, f32::min);
            let hi = rows.iter().map(|r| r.2).fold(0.0f32, f32::max);
            for (t, ink, floor) in &rows {
                println!(
                    "seen_floor/{theme} t={t:.2} ink=#{ink:06X} peak={} floor={floor:.0}/236",
                    ((ink >> 16) & 0xff).max((ink >> 8) & 0xff).max(ink & 0xff)
                );
            }
            assert!(
                hi >= 3.0 * lo,
                "{theme}: the arc's colour floors must spread — {lo:.0} to {hi:.0}"
            );
            assert!(
                hi < UNDER_COV_CAP,
                "{theme}: every stop must be visible at the bed's own ceiling ({hi:.0})"
            );
        }
    }

    #[test]
    fn the_hot_edge_embers_out_with_its_body_on_focus_loss() {
        // §8.2 "Focus lost | ember in 300 ms | spend": the hairline is the
        // head's property and takes the head cell's envelope, so it spends
        // WITH the body. A hot edge that held at 38 while the body under it
        // spent to nothing, then vanished with the pool at +300 ms, was a
        // white line with no keystroke behind it — the anti-stray class.
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 10, 8, &c, 0.9);
        let lost = at(t0, 7 * 60 + 100);
        let mut sink = Sink::default();
        let frame = |rib: &mut Ribbon, sink: &mut Sink, now: Instant| {
            let cx = ctx(now, &c, (2, 18), 0.9);
            rib.plan(&cx);
            let mut f = sink.frame();
            rib.emit(&cx, &mut f);
        };
        frame(&mut rib, &mut sink, lost);
        let full = peak_channel(&sink.out);
        assert!(
            full > 0,
            "a hot hand must leave a hot edge for the law to have been tested"
        );
        rib.on_event(&Event::Focus(false), lost, &ctx(lost, &c, (2, 18), 0.9));
        frame(&mut rib, &mut sink, at(lost, 150));
        let mid = peak_channel(&sink.out);
        assert!(
            mid < full,
            "halfway through the ember the hot edge must already be spending ({full} → {mid})"
        );
        frame(&mut rib, &mut sink, at(lost, 250));
        assert!(
            !sink.under.is_empty(),
            "the body must still be on glass for the pin to mean anything"
        );
        let low = peak_channel(&sink.out);
        // RESTATED 2026-09-14: the hairline's share of its own start is the
        // HEAD CELL'S ENVELOPE, exactly — which is the law, where the old
        // `low * 4 < full` was a number that happened to hold while every
        // stop spent one shared curve. Under the per-stop ember floor the
        // head's stop keeps the range its ink has, and the hairline keeps
        // exactly that: measured, not assumed.
        let cx = ctx(at(lost, 250), &c, (2, 18), 0.9);
        let head = rib
            .cells()
            .iter()
            .max_by_key(|l| l.col)
            .copied()
            .expect("the head is in the pool");
        let env = rib.env_of(&cx, &head);
        let share = low as f32 / full as f32;
        assert!(
            (share - env).abs() < 0.06,
            "+250 ms into the ember the hot edge is at {low} of {full} (share {share:.3}); \
             it must ride the head cell's own envelope ({env:.3}), not a clock of its own"
        );
        // RESTATED again 2026-09-14 (the floor as a contrast): "going out
        // with its body" is a claim about the CLOCK, and `env < 0.7` was a
        // claim about the head stop's RANGE — which belongs to the theme.
        // `spend` is `(1 − u)²`, so at +250 ms of 300 the ember has spent
        // 97% of whatever range the head has: the envelope here must already
        // be within a few parts in a hundred of the one it ends the ember
        // at, and the hairline must be falling with it.
        let end_cx = ctx(at(lost, 299), &c, (2, 18), 0.9);
        let env_end = rib.env_of(&end_cx, &head);
        let law = env_end + (1.0 - env_end) * spend(250.0 / 300.0);
        assert!(
            low <= mid && (env - law).abs() < 0.03 && env_end < 1.0,
            "…and it must be going out with its body on the ember's own clock, not holding \
             until the pool clears ({full} → {mid} → {low}, envelope {env:.3}, the ember's \
             law at this instant {law:.3}, its floor {env_end:.3})"
        );
        frame(&mut rib, &mut sink, at(lost, 300));
        assert!(
            sink.out.is_empty()
                && rib.at_rest()
                && rib.next_change_deadline(at(lost, 300)).is_none(),
            "…and at the ember's end there is nothing, hairline included, and no wake asked for"
        );
    }

    /// **THE FOCUS SEAM'S WAKE CLAUSE** (2026-09-13, Rainbow Path v3 step 1).
    /// The host now forwards RAW focus (`CursorGlow::note_focus`), and the
    /// host's own law for an unfocused window that is being typed into — a
    /// control-socket agent's window — is that its light keeps drawing (the
    /// "live typed wake" fold). So a typed key laid while the ember burns
    /// CLEARS the ember and re-lights from the level the light had, through
    /// `edge-in`, and the pool is not swept at the ember's end. Before this
    /// arm every cell laid under a burning ember was born at `spend(1) = 0`.
    #[test]
    fn a_typed_key_under_a_burning_ember_clears_it_and_relights_from_the_level() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 10, 8, &c, 0.9);
        let lost = at(t0, 7 * 60 + 100);
        rib.plan(&ctx(lost, &c, (2, 18), 0.9));
        let pre = plan_peak(&rib);
        rib.on_event(&Event::Focus(false), lost, &ctx(lost, &c, (2, 18), 0.9));
        let cx = ctx(at(lost, 150), &c, (2, 18), 0.9);
        rib.plan(&cx);
        let mid = plan_peak(&rib);
        let full = (UNDER_COV_CAP * BODY_COLD_SHARE) as u8;
        // RESTATED 2026-09-14 (the per-stop floor as a contrast): halfway
        // through, the body is below the level it had at the blur. It is NOT
        // below the cold-body ceiling any more, and that was never the
        // claim: the peak belongs to whichever stop in the run has the least
        // range between the bed's bar and the colour floor, and on a dark
        // ground a warm stop's range is a fifth of its light.
        assert!(
            mid > 0 && mid < pre,
            "halfway through the ember the body is spending ({pre} → {mid}, cold ceiling {full})"
        );
        // A key at +150 ms: the hand is on the window, focused or not.
        let key = at(lost, 150);
        let cx = ctx(key, &c, (2, 19), 0.9);
        rib.on_event(&typed(), key, &cx);
        rib.plan(&cx);
        assert!(
            plan_peak(&rib) >= mid,
            "the key re-arms the run from the ember's level, never below it ({} < {mid})",
            plan_peak(&rib)
        );
        let cx = ctx(at(key, 30), &c, (2, 19), 0.9);
        rib.plan(&cx);
        assert!(
            plan_peak(&rib) >= full - 2,
            "one edge-in after the key the run is back at its ceiling ({})",
            plan_peak(&rib)
        );
        let cx = ctx(at(lost, 320), &c, (2, 19), 0.9);
        rib.plan(&cx);
        assert!(
            !rib.at_rest() && plan_peak(&rib) >= full - 2,
            "past the ember's end the ribbon is still on the glass: the key cleared the ember"
        );
    }

    /// **THE CURTAIN** (Rainbow Path v3 §2.8, A2; 2026-09-13, restated
    /// 2026-09-14). At a coordinate-space seam the band is drawn into the
    /// hand it last stood under over `CURTAIN_S` and spent to exactly zero —
    /// FARTHEST-FIRST, so the count falls in steps and never off a cliff —
    /// and nothing is laid until it is over: a key and a jump during the
    /// curtain leave the pool as it is, a key after it lays afresh. The host
    /// asks for frames throughout (the suck-in is motion).
    ///
    /// **STATED IN THE QUANTITY THE INSTRUMENTS READ.** The law's own metric
    /// is [`Ribbon::lit_segments`] — `trail status`'s `ribbon_segments=`, and
    /// with [`Ribbon::ink_segments`] the number the glass census counts. An
    /// earlier spelling of this pin asserted a geometric `cell_lit`, which
    /// scores a quad at alpha 1 exactly as it scores one at alpha 236 and so
    /// cannot see a brightness cliff at all; and it stopped sampling at
    /// +198 ms, one frame before the fall it was pinning. Both halves are
    /// fixed here: the sweep runs PAST `CURTAIN_S` to +330 ms, and every
    /// frame's step is measured on the segment counts.
    ///
    /// The bounds are [`CURTAIN_MAX_STEP_FRAC`] and [`CURTAIN_MIN_FRAMES`]:
    /// the natural retract this curtain imitates never steps more than 6 of
    /// ~20 cells, because it is staggered farthest-first. A curtain on ONE
    /// clock stepped the whole band — `ribbon_segments` 19 → 0 in a single
    /// frame on glass, at every band age swept.
    #[test]
    fn a_curtain_gathers_the_band_into_its_hand_over_a_quarter_second_with_no_cliff_and_lays_nothing()
     {
        let c = cfg(true, true);
        let g = geom();
        let cw = g.cw as f32;
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 10, 20, &c, 0.9);
        let last = at(t0, 19 * 60);
        // THE CARET HAS ALREADY MOVED when the host notices the space ended:
        // this is the alt-screen path — a Return launched the program, so the
        // caret is at column 0 of the NEXT row while the band still lies at
        // columns 10..29 of row 2. The curtain must gather into the BAND'S
        // head, not slide 30 columns to the caret.
        let caret = (3u16, 0u16);
        let mut sink = Sink::default();
        #[derive(Clone, Copy)]
        struct Sample {
            peak: u8,
            sum: u32,
            left: f32,
            cells: usize,
            segs: usize,
            ink: usize,
        }
        let frame = |rib: &mut Ribbon, sink: &mut Sink, now: Instant| -> Sample {
            let cx = ctx(now, &c, caret, 0.9);
            rib.plan(&cx);
            let segs = rib.lit_segments().count();
            let ink = rib.ink_segments().count();
            let mut f = sink.frame();
            rib.emit(&cx, &mut f);
            Sample {
                peak: sink.under.iter().map(|q| q.alpha).max().unwrap_or(0),
                sum: sink.under.iter().map(|q| u32::from(q.alpha)).sum(),
                left: sink
                    .under
                    .iter()
                    .map(|q| f32::from(q.x))
                    .fold(f32::INFINITY, f32::min),
                cells: (0..40u16)
                    .filter(|&col| cell_lit(&sink.under, g, 2, col))
                    .count(),
                segs,
                ink,
            }
        };
        let before = frame(&mut rib, &mut sink, at(last, 8));
        assert!(
            before.peak > 200 && before.cells == 20 && before.segs >= 20,
            "the band is whole before the seam (peak {}, {} cells, {} segments)",
            before.peak,
            before.cells,
            before.segs
        );
        let fell = at(last, 16);
        rib.curtain(fell);
        assert!(rib.curtained());
        assert!(rib.brisk(fell), "a falling curtain asks for every frame");
        assert!(
            rib.next_change_deadline(fell)
                .is_some_and(|d| d <= fell + Duration::from_secs_f32(CURTAIN_S)),
            "…and books its end"
        );
        // Nothing is laid under the curtain: a key, an echo, a jump.
        let cells = rib.cells().len();
        let cx = ctx(at(fell, 40), &c, (3, 1), 0.9);
        rib.on_event(&typed(), at(fell, 40), &cx);
        rib.on_event(
            &Event::Sweep {
                row: 3,
                col0: 0,
                col1: 1,
            },
            at(fell, 40),
            &cx,
        );
        rib.on_event(&nav((3, 1), (3, 40)), at(fell, 40), &cx);
        assert_eq!(rib.cells().len(), cells, "a curtain lays nothing");
        // Eleven frames at 30 fps, PAST the end of the fall: the band's total
        // light falls every frame, the left end is drawn toward the band's
        // own head, and neither segment count ever steps more than
        // `CURTAIN_MAX_STEP_FRAC` of what it fell on.
        let mut prev = before;
        let mut worst_step = 0usize;
        let mut worst_ink_step = 0usize;
        let mut ink_frames = 0usize;
        let mut dark_at = None;
        for ms in [33u64, 66, 99, 132, 165, 198, 231, 264, 297, 330] {
            let now = frame(&mut rib, &mut sink, at(fell, ms));
            assert!(
                now.peak <= prev.peak,
                "+{ms} ms: the curtain never brightens ({} → {})",
                prev.peak,
                now.peak
            );
            // NEVER BRIGHTENS. The tolerance is geometric, not temporal: the
            // suck-in moves every boundary, so the slab lattice under a
            // shortening run re-samples and the summed alpha can wobble by a
            // few parts in ten thousand between two frames of a fall that is
            // monotone in every cell's envelope.
            assert!(
                now.sum <= prev.sum + prev.sum / 500,
                "+{ms} ms: the band's total light rose ({} → {})",
                prev.sum,
                now.sum
            );
            if now.sum > 0 {
                assert!(
                    now.left >= prev.left - 0.5,
                    "+{ms} ms: the band is drawn toward its head, never away ({} → {})",
                    prev.left,
                    now.left
                );
            }
            let step = prev.segs.saturating_sub(now.segs);
            let ink_step = prev.ink.saturating_sub(now.ink);
            worst_step = worst_step.max(step);
            worst_ink_step = worst_ink_step.max(ink_step);
            let bar = CURTAIN_MAX_STEP_FRAC * before.segs.max(before.ink) as f32;
            assert!(
                step as f32 <= bar && ink_step as f32 <= bar,
                "+{ms} ms: the curtain stepped {step} claimed / {ink_step} inked segments in one \
                 frame ({} → {} / {} → {}); the bound is {bar:.1} \
                 ({CURTAIN_MAX_STEP_FRAC} of the {} it fell on)",
                prev.segs,
                now.segs,
                prev.ink,
                now.ink,
                before.segs.max(before.ink)
            );
            if now.ink > 0 {
                ink_frames += 1;
            }
            println!(
                "curtain +{ms} ms: peak {} sum {} left {:.1} cells {} segs {} ink {}",
                now.peak, now.sum, now.left, now.cells, now.segs, now.ink
            );
            if now.ink == 0 && dark_at.is_none() {
                dark_at = Some(ms);
            }
            prev = now;
        }
        println!("curtain worst step: {worst_step} claimed, {worst_ink_step} inked");
        // NOT VACUOUS: the fall must actually have been a fall — the counts
        // reached zero inside the sweep, and they did it in steps.
        assert!(
            dark_at.is_some_and(|ms| ms >= 132),
            "the band went dark at {dark_at:?} ms — a curtain lasts CURTAIN_S"
        );
        assert!(
            worst_ink_step > 0,
            "the sweep never saw the band leave: it was already dark"
        );
        assert!(
            prev.sum * 8 < before.sum,
            "the fall spent nothing: {} → {} over the whole sweep",
            before.sum,
            prev.sum
        );
        assert!(
            ink_frames >= CURTAIN_MIN_FRAMES,
            "the curtain drew ink on {ink_frames} of the swept frames; the law is \
             {CURTAIN_MIN_FRAMES} (a cut draws one, a one-clock curtain two)"
        );
        // Exactly zero at the end, the pool empty, the next key lays afresh.
        let end = frame(&mut rib, &mut sink, at(fell, 250));
        assert_eq!((end.peak, end.segs, end.ink), (0, 0, 0));
        assert!(rib.at_rest() && !rib.curtained());
        let after = at(fell, 300);
        let cx = ctx(after, &c, (4, 6), 0.9);
        rib.on_event(&typed(), after, &cx);
        rib.plan(&cx);
        assert_eq!(
            rib.cells().len(),
            1,
            "after the curtain a key lays its cell"
        );
        // **THE GATHER IS INTO THE BAND'S OWN HEAD** (2026-09-14). The band
        // lies at columns 10..29 of row 2; its head is column 29, whose right
        // edge is column 30's left edge, and that is where every boundary
        // must converge. The seam's caret is NOT that place, and the shipped
        // curtain froze `Ribbon::caret`'s bare COLUMN: at the alt-screen seam
        // the Return that launched the program had already taken the caret to
        // column 0, so the whole band was dragged 30 columns — 270 px — to
        // column 0 while it faded. A slide across the row, not a gather into
        // the hand.
        //
        // Both of the shapes that caret takes are swept, because they fail
        // differently: (3, 0) — the next row — is the alt-screen path, and
        // (2, 0) — column 0 of the BAND'S OWN row, a carriage-return redraw
        // or a resize landing — is the one a per-row read of the caret still
        // gets wrong.
        for seam_caret in [(3u16, 0u16), (2, 0)] {
            let mut rib2 = Ribbon::new();
            type_run(&mut rib2, t0, 10, 20, &c, 0.9);
            // The host's last observed caret before the space ended.
            let cx = ctx(at(last, 8), &c, seam_caret, 0.9);
            rib2.plan(&cx);
            rib2.curtain(fell);
            let near_end = at(fell, 216);
            let cx = ctx(near_end, &c, seam_caret, 0.9);
            rib2.plan(&cx);
            {
                let mut f2 = sink.frame();
                rib2.emit(&cx, &mut f2);
            }
            let left_x = sink
                .under
                .iter()
                .map(|q| f32::from(q.x))
                .fold(f32::INFINITY, f32::min);
            let head_x = f32::from(g.origin_x) + 30.0 * cw;
            assert!(
                !sink.under.is_empty(),
                "seam caret {seam_caret:?}: the band was gone before the gather could be read"
            );
            assert!(
                (left_x - head_x).abs() <= 3.0 * cw,
                "seam caret {seam_caret:?}: the band gathered to x {left_x:.1}, not to its own \
                 head at {head_x:.1} (column 0 is x {:.1}, {:.0} cells away)",
                f32::from(g.origin_x),
                (head_x - f32::from(g.origin_x)) / cw
            );
        }
    }

    /// **THE CURTAIN IS DRAWN UNDER LETTERS THAT STAY LEGIBLE** (Rainbow Path
    /// v3 §2.8, law B1 over step 7's frames). The 0.24 s the band spends
    /// leaving is drawn UNDER the incoming app's first frames — `less` painting
    /// its page, `vim` its buffer — and the design's claim is that this is safe
    /// "by construction", because the bed is source-over at the bar. That is an
    /// argument, not a measurement: `letters_stay_legible_under_the_ribbon…`
    /// sweeps `bed_ink`'s own range, but it never runs a curtain, and the
    /// curtain is the one state that spends every cohort's light on one
    /// envelope and drags every boundary across the row while it does. So this
    /// pin sweeps the real emitted frames: every device row a letter of the
    /// typed row (and of the reach into the row above) can touch, at every
    /// column the band crosses, on every 8 ms of the fall — the text
    /// composites at `BODY_CONTRAST_BAR` throughout.
    ///
    /// **AND IT IS SENSITIVE TO THE CURTAIN** (2026-09-14). An earlier
    /// spelling passed with the ENTIRE mechanism deleted — the `spend`
    /// multiply, the boundary suck-in and the lay-gate all removed — because
    /// all it pinned was "a lit ribbon is legible", which
    /// `letters_stay_legible_under_the_ribbon…` already pins, and a pin that
    /// cannot refute anything is worse than none. The frames it sweeps are now
    /// required to BE a curtain's: the light spends away (red on its own if
    /// the `spend` multiply goes), the ink retreats toward the hand, and a key
    /// struck under it lays nothing (red on its own if the gate goes).
    ///
    /// The rail is deliberately outside the sweep, exactly as in
    /// `the_rail_is_yellow_below_the_row_bottom_and_the_glyph_box_stays_at_the_bar`:
    /// its top is pinned at the row bottom and no glyph of the row reaches it.
    #[test]
    fn the_curtain_is_legible_under_the_app_s_first_frames_on_every_frame_it_falls() {
        let c = cfg(true, true);
        let g = geom();
        let (cw, ch) = (g.cw as f32, g.ch as f32);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 10, 20, &c, 0.9);
        let last = at(t0, 19 * 60);
        let caret = (2u16, 30u16);
        let fell = at(last, 16);
        // THE CELLS THE CURTAIN FALLS ON, so the lay-gate below has something
        // to refuse.
        let laid = rib.cells().len();
        rib.curtain(fell);
        assert!(rib.curtained(), "precondition: a curtain is falling");
        // THE LAY-GATE, checked on the frames the sweep is about to walk: a
        // key struck under the curtain lays nothing, so what the letters
        // composite against is the LEAVING band and not a fresh one. Red on
        // its own if the gate goes.
        // The key lands well clear of the band's own columns, so a lay that
        // got through is a NEW cell and not a re-wet of one already there.
        let cx = ctx(at(fell, 8), &c, (2, 41), 0.9);
        rib.on_event(&typed(), at(fell, 8), &cx);
        assert_eq!(
            rib.cells().len(),
            laid,
            "the sweep is not under a lay-gate: a key laid a cell during the curtain"
        );
        let row_top = f32::from(g.origin_y) + 2.0 * ch;
        let row_bottom = row_top + ch;
        let mut sink = Sink::default();
        let mut worst = f32::INFINITY;
        let mut worst_at = (0.0_f32, 0.0_f32, 0_u64);
        let mut lit_frames = 0;
        let mut peak_seen = 0_u8;
        let mut first: Option<(u32, f32)> = None;
        let mut last_lit: Option<(u64, u32, f32)> = None;
        for ms in (0..=248).step_by(8) {
            let cx = ctx(at(fell, ms), &c, caret, 0.9);
            rib.plan(&cx);
            {
                let mut f = sink.frame();
                rib.emit(&cx, &mut f);
            }
            let peak = sink.under.iter().map(|q| q.alpha).max().unwrap_or(0);
            if peak > 0 {
                lit_frames += 1;
                peak_seen = peak_seen.max(peak);
            }
            // THE THREE MOVING PARTS, sampled on the same frames the letters
            // are (see the paragraph above): the spend's envelope, the
            // suck-in's leftmost boundary, and the total light.
            let sum: u32 = sink.under.iter().map(|q| u32::from(q.alpha)).sum();
            let left = sink
                .under
                .iter()
                .map(|q| f32::from(q.x))
                .fold(f32::INFINITY, f32::min);
            if ms == 0 {
                first = Some((sum, left));
            }
            if sum > 0 {
                last_lit = Some((ms, sum, left));
            }
            // The whole span the run could occupy, plus a cell either side —
            // the suck-in moves the boundaries, so a fixed window would stop
            // sampling the band rather than proving anything about it.
            let mut x = f32::from(g.origin_x) + 9.0 * cw;
            while x < f32::from(g.origin_x) + 32.0 * cw {
                let mut y = row_top - 0.2 * ch;
                while y < row_bottom {
                    let k = contrast(DEFAULT_FG, composite_at(&sink.under, x, y));
                    if k < worst {
                        worst = k;
                        worst_at = (x, y - row_bottom, ms);
                    }
                    y += 1.0;
                }
                x += 1.0;
            }
        }
        assert!(
            lit_frames >= 3 && peak_seen > 0,
            "precondition: the curtain actually drew ({lit_frames} lit frames, peak {peak_seen})"
        );
        // **SENSITIVE TO THE CURTAIN, NOT TO A LIT RIBBON** (2026-09-14). An
        // earlier spelling of this pin passed with the ENTIRE mechanism
        // deleted — the `spend` multiply, the boundary suck-in and the
        // lay-gate all removed — because all it pinned was "a lit ribbon is
        // legible", which `letters_stay_legible_under_the_ribbon…` already
        // pins. A pin that cannot refute anything is worse than none, so the
        // frames swept are now required to BE a curtain's: its light spends
        // away, its boundaries are drawn into the hand, and nothing is laid
        // under it. Each clause is red on its own if its mechanism goes.
        let (sum0, left0) = first.expect("the sweep starts at +0 ms");
        let (end_ms, end_sum, end_left) = last_lit.expect("the curtain drew");
        assert!(
            end_sum * 8 < sum0,
            "the sweep was not a falling curtain: the band's light went {sum0} → {end_sum} \
             by +{end_ms} ms"
        );
        assert!(
            end_left > left0 + 3.0 * cw,
            "the sweep was not a gathering curtain: the band's ink retreated from {left0:.1} \
             only to {end_left:.1} px, under three cells — it is supposed to end up under the \
             hand at {:.1}",
            f32::from(g.origin_x) + 30.0 * cw
        );

        // NOT VACUOUS: a sweep that missed the band would answer the bare
        // ground's own contrast, which is the one number this pin can produce
        // while proving nothing.
        let ground = contrast(DEFAULT_FG, DEFAULT_BG);
        println!(
            "curtain legibility: {lit_frames} lit frames, peak cov {peak_seen}, worst {worst:.3}:1 vs the bare ground's {ground:.3}:1"
        );
        assert!(
            worst < ground - 0.5,
            "the sweep never crossed the band: worst {worst:.3}:1 is the bare ground's {ground:.3}:1"
        );
        assert!(
            worst >= BODY_CONTRAST_BAR,
            "a letter under the falling curtain composites at {worst:.3}:1 (x {}, {:.0} px above the row bottom, +{} ms into the fall); the bar is {BODY_CONTRAST_BAR}:1",
            worst_at.0,
            -worst_at.1,
            worst_at.2
        );
    }

    #[test]
    fn an_idle_ribbon_draws_nothing_and_asks_for_no_frame() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 10, 4, &c, 0.5);
        let now = at(t0, 30_000);
        let cx = ctx(now, &c, (2, 14), 0.0);
        rib.plan(&cx);
        let mut sink = Sink::default();
        {
            let mut f = sink.frame();
            rib.emit(&cx, &mut f);
        }
        assert!(sink.under.is_empty() && sink.out.is_empty() && sink.halos.is_empty());
        assert!(rib.at_rest());
        assert!(rib.next_change_deadline(now).is_none());
        assert!(rib.field().is_empty());
    }

    // -- §4, D14: one emitter, two profiles --------------------------------

    #[test]
    fn tall_and_underline_are_one_emitter_with_two_shoulders() {
        let tall = cfg(true, true);
        let under = cfg(true, false);
        let (a, b) = (Ribbon::body_profile(&tall), Ribbon::body_profile(&under));
        assert!((a.shoulder - SHOULDER_TALL).abs() < 1e-6);
        assert!((b.shoulder - SHOULDER_UNDERLINE).abs() < 1e-6);
        assert!(a.up_ch > b.up_ch, "the tall body reaches into row − 1");
        assert!(
            (a.dn_ch - b.dn_ch).abs() < 1e-6,
            "the two spellings differ ABOVE the spine and nowhere else"
        );

        // …and the LIGHT law is one law: the same cell, the same age, the same
        // spine gives the same peak coverage under both profiles. Only the
        // band's extent moves.
        let t0 = Instant::now();
        let mut peaks = Vec::new();
        let mut rows = Vec::new();
        for c in [&tall, &under] {
            let mut rib = Ribbon::new();
            type_run(&mut rib, t0, 10, 6, c, 0.7);
            let now = at(t0, 5 * 60);
            let cx = ctx(now, c, (2, 16), 0.7);
            rib.plan(&cx);
            let mut sink = Sink::default();
            {
                let mut f = sink.frame();
                rib.emit(&cx, &mut f);
            }
            peaks.push(sink.under.iter().map(|q| q.alpha).max().unwrap_or(0));
            rows.push(
                sink.under
                    .iter()
                    .map(|q| f32::from(q.y))
                    .reduce(f32::min)
                    .unwrap_or(0.0),
            );
        }
        assert_eq!(
            peaks[0], peaks[1],
            "the tall and underline spellings must price light identically"
        );
        assert!(
            rows[0] < rows[1],
            "the tall body's top edge must sit above the underline's"
        );
    }

    // -- §4.2 / D16: the band is published so nothing is born inside it -----

    #[test]
    fn the_ribbon_publishes_the_band_stardust_must_stay_out_of() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 10, 6, &c, 0.8);
        let now = at(t0, 5 * 60);
        let cx = ctx(now, &c, (2, 16), 0.8);
        rib.plan(&cx);
        let g = geom();
        let band = rib.band(2, 13).expect("a laid cell publishes a band");
        let cell_top = f32::from(g.origin_y) + 2.0 * g.ch as f32;
        assert!(
            band.top < cell_top,
            "the tall band reaches into row − 1, which is D16's whole point"
        );
        assert!(band.contains(band.spine));
        assert!(!band.contains(band.top - 0.05 * g.ch as f32));
        // The sky band §5.4 hands stardust is entirely above the body.
        let sky_lo = band.top - 0.30 * g.ch as f32;
        let sky_hi = band.top - 0.04 * g.ch as f32;
        assert!(sky_lo < sky_hi && !band.contains(sky_hi));
        assert!(
            rib.band(2, 60).is_none(),
            "an unlaid cell publishes nothing"
        );
    }

    // -- §3.3: the light fork ---------------------------------------------

    #[test]
    fn a_light_theme_ribbon_is_source_over_ink_and_never_additive() {
        let c = cfg(false, false);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 10, 6, &c, 0.9);
        let now = at(t0, 5 * 60);
        let cx = ctx(now, &c, (2, 16), 0.9);
        rib.plan(&cx);
        let mut sink = Sink::default();
        {
            let mut f = sink.frame();
            rib.emit(&cx, &mut f);
        }
        assert!(!sink.under.is_empty(), "the light rail still draws");
        assert!(
            sink.out.is_empty(),
            "no additive light on white — L6, and the hot edge is dark-only"
        );
        let cap = InkRole::Leading.alpha_cap() as u8;
        for q in &sink.under {
            assert!(q.alpha > 0, "every light-theme quad composites SOURCE-OVER");
            assert!(q.alpha <= cap, "light alpha {} over the {cap} cap", q.alpha);
        }
    }

    #[test]
    fn a_dark_body_rides_its_ceiling_and_the_strip_shows_above_it() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 10, 12, &c, 1.0);
        let now = at(t0, 11 * 60);
        let cx = ctx(now, &c, (2, 22), 1.0);
        rib.plan(&cx);
        let mut sink = Sink::default();
        {
            let mut f = sink.frame();
            rib.emit(&cx, &mut f);
        }
        let peak = sink.under.iter().map(|q| q.alpha).max().unwrap_or(0);
        assert!(
            peak > (UNDER_COV_CAP * 0.9) as u8,
            "a hot ribbon must actually reach its ceiling (peak {peak}); a body that cannot is the 'dim and muddy' defect"
        );
        // THE STRIP'S ORACLE: on a hot mark the body sits at its own cap, so
        // the only thing that can take the spine past it is the baseline
        // strip's accent — if it does not, the strip is not there.
        assert!(
            peak > UNDER_COV_CAP as u8,
            "the baseline strip must show above the body's ceiling (peak {peak}, cap {UNDER_COV_CAP})"
        );
        assert!(
            f32::from(peak) <= BODY_FRAME_TOP,
            "and the strip's lift may never take the spine past the ledger's frame top"
        );
    }

    // -- §30 (2026-09-13): the comet, the vivid rail, the attack, the gate --

    /// The composited colour of `under` at device pixel `(x, y)`: every quad
    /// covering it, in emission order, over the default ground — Over for a
    /// source-over quad, additive for an additive one — the CPU reference's
    /// own blend.
    fn composite_at(under: &[GlowQuad], x: f32, y: f32) -> u32 {
        use aterm_render::add_sat;
        let (px, py) = (x.floor(), y.floor());
        under
            .iter()
            .filter(|q| {
                let qx = f32::from(q.x);
                (qx..qx + f32::from(q.w)).contains(&px) && f32::from(q.y) == py
            })
            .fold(DEFAULT_BG, |acc, q| {
                if q.alpha == 0 {
                    add_sat(acc, q.color)
                } else {
                    over_premul(acc, q.color, q.alpha)
                }
            })
    }

    /// The device rows of `under` at column `x`, as `(y, composited)`.
    fn column_at(under: &[GlowQuad], x: f32) -> Vec<(f32, u32)> {
        let mut ys: Vec<f32> = under
            .iter()
            .filter(|q| {
                let qx = f32::from(q.x);
                (qx..qx + f32::from(q.w)).contains(&x.floor())
            })
            .map(|q| f32::from(q.y))
            .collect();
        ys.sort_by(|a, b| a.partial_cmp(b).expect("finite"));
        ys.dedup();
        ys.into_iter()
            .map(|y| (y, composite_at(under, x, y)))
            .collect()
    }

    /// **THE VIVID RAIL** (§30): below the row bottom the rainbow is the FULL
    /// ROYGBIV — yellow composites as yellow at a max channel over 200 — and
    /// above it every device row a letter can touch is still the bed, at the
    /// bar. The owner, 2026-09-13: *"I don't see much yellow? that's
    /// confusing"* — *"the rainbow pallet doesn't seems to be the FULL
    /// ROYGBIV rainbow"*. The bed's own pin
    /// (`the_dimmest_stop_of_the_bed_composites_at_a_max_channel_of_about_80_under_the_bar`)
    /// still holds beside this one: the two zones are two inks.
    #[test]
    fn the_rail_is_yellow_below_the_row_bottom_and_the_glyph_box_stays_at_the_bar() {
        use crate::spectrum::{SPECTRUM_ANCHOR_AT, SPECTRUM_LUT_LEN};
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 10, 8, &c, 0.9);
        let cx = ctx(at(t0, 8 * 60), &c, (2, 18), 0.9);
        rib.plan(&cx);
        let mut sink = Sink::default();
        {
            let mut f = sink.frame();
            rib.emit(&cx, &mut f);
        }
        let g = geom();
        let (cw, ch) = (g.cw as f32, g.ch as f32);
        let row_top = f32::from(g.origin_y) + 2.0 * ch;
        let row_bottom = row_top + ch;
        // Yellow's anchor on the walk — the arc is perceptually paced, so it
        // is `SPECTRUM_ANCHOR_AT[2]`, not `2/6`.
        let t_yellow = SPECTRUM_ANCHOR_AT[2] as f32 / (SPECTRUM_LUT_LEN - 1) as f32;
        let seg = rib
            .plan_segments()
            .iter()
            .min_by(|a, b| {
                (a.t - t_yellow)
                    .abs()
                    .partial_cmp(&(b.t - t_yellow).abs())
                    .expect("finite")
            })
            .expect("a planned run");
        assert!(
            (seg.t - t_yellow).abs() < 0.04,
            "the run must cross yellow's anchor (nearest slab t = {:.3}, yellow {t_yellow:.3})",
            seg.t
        );
        let x = seg.x + 0.5;
        // 0.11–0.17 ch below the row bottom: inside the leading, in the rail's
        // plateau, under no letter of the typed row.
        let y_rail = row_bottom + (0.12 * ch).round();
        let lit = composite_at(&sink.under, x, y_rail);
        println!(
            "the column at yellow (x {x:.1}): {:?}",
            column_at(&sink.under, x)
                .iter()
                .map(|(y, c)| format!("y{}=#{c:06X}", *y as i32 - row_bottom as i32))
                .collect::<Vec<_>>()
        );
        let (r, gr, b) = ((lit >> 16) & 0xff, (lit >> 8) & 0xff, lit & 0xff);
        // 2.4× THE BED'S OLIVE (re-ruled 2026-09-14): the rail's own ceiling
        // (`RAIL_LUMA_CEIL` 0.54 — `(200, 200, 0)` ink, `≈ (190, 190, 3)` at
        // the cap) puts a yellow that reads as yellow under the baseline,
        // r ≈ g with little blue. It is brighter than the caret's floor on
        // purpose: the rail is never under a letter, and L5 is the caret's
        // law in the glyph rows.
        // r within a sixteenth of g: the sampled slab lerps a step past the
        // anchor toward green (`#B6C000` measured), which is still yellow.
        assert!(
            max_channel(lit) >= 185 && r.abs_diff(gr) <= 16 && b < 40,
            "0.15 ch below the row bottom at yellow's anchor the rail composites #{lit:06X}; the owner asked to SEE yellow (peak ≥ 185, r ≈ g, little blue)"
        );
        // …and the rail is the FULL arc: every stop the run lays composites
        // at a max channel over 130 down there, warm and cold alike.
        // (A vertex owns the slab to its RIGHT, so the run's last vertex is
        // not sampled.)
        for w in rib.plan_segments().windows(2).step_by(3) {
            let seg = &w[0];
            let lit = composite_at(&sink.under, seg.x + 0.5, y_rail);
            if seg.cov < STATUS_LIT_COV {
                continue;
            }
            assert!(
                max_channel(lit) >= 130,
                "the rail at t = {:.3} composites #{lit:06X} (max {}); every stop is vivid on the rail",
                seg.t,
                max_channel(lit)
            );
        }
        // THE GLYPH BOX STAYS AT THE BAR: every device row above the row
        // bottom — the typed row and the reach into the row above — composites
        // the text at 5.25:1, at every column of the run. The rail's top is
        // pinned at the row bottom, so the wave's crest cannot lift it into
        // the descenders.
        let mut worst = f32::INFINITY;
        let mut worst_at = (0.0, 0.0);
        let mut x = f32::from(g.origin_x) + 10.0 * cw;
        while x < f32::from(g.origin_x) + 18.0 * cw {
            let mut y = row_top - 0.2 * ch;
            while y < row_bottom {
                let lit = composite_at(&sink.under, x, y);
                let k = contrast(DEFAULT_FG, lit);
                if k < worst {
                    worst = k;
                    worst_at = (x, y - row_bottom);
                }
                y += 1.0;
            }
            x += 1.0;
        }
        assert!(
            worst >= BODY_CONTRAST_BAR,
            "the text composites at {worst:.3}:1 at (x {}, {:.0} px above the row bottom); the bar is {BODY_CONTRAST_BAR}:1 over every row a letter can touch",
            worst_at.0,
            -worst_at.1
        );
        // (A "no vivid quad above the row bottom" heuristic on the max
        // channel is NOT a pin: the bed's own blue composites at 238 there.
        // The bar sweep above is the claim.)
    }

    /// The rail's ink, stop by stop: every anchor inside the band
    /// `[RAIL_LUMA_FLOOR, RAIL_LUMA_CEIL]` — the cold half lifted to the
    /// floor and no further, the warm half scaled down to the caret's
    /// ceiling with its hue exact, red carried PURE — and every one at
    /// least 130 on its brightest channel composited over the bed. Printed
    /// so a re-tune has a table to move.
    #[test]
    fn the_rail_ink_table() {
        use crate::spectrum::SPECTRUM_ANCHORS;
        let names = [
            "red", "orange", "yellow", "green", "blue", "indigo", "violet",
        ];
        let cov = UNDER_COV_CAP as u8;
        println!();
        println!(
            "band [{RAIL_LUMA_FLOOR:.3}, {RAIL_LUMA_CEIL:.3}] — the rail's own ceiling below the baseline (the caret's floor {} × the field's share {} rules the glyph rows)",
            crate::cursor_glow::RAINBOW_CARET_LIGHT_FLOOR,
            crate::cursor_glow::RAINBOW_SPARKLE_LIGHT_SHARE
        );
        println!("stop     arc      rail     Y rail  composited@236 (max, light)   bed@236");
        for (name, &arc) in names.iter().zip(SPECTRUM_ANCHORS.iter()) {
            let ink = rail_ink(arc);
            let y = relative_luminance(ink);
            let lit = over_premul(DEFAULT_BG, premul_rgb(ink, cov), cov);
            let bed = over_premul(
                DEFAULT_BG,
                premul_rgb(bed_ink(arc, bed_luma_budget(DEFAULT_FG)), cov),
                cov,
            );
            println!(
                "{name:7}  #{arc:06X}  #{ink:06X}  {y:.3}   #{lit:06X} ({:3}, {:3.0})          #{bed:06X}",
                max_channel(lit),
                relative_luminance(lit) * 255.0
            );
            assert!(
                (RAIL_LUMA_FLOOR - 0.003..=RAIL_LUMA_CEIL).contains(&y),
                "{name}: the rail's ink #{ink:06X} (Y {y:.3}) is outside the band"
            );
            let y_full = relative_luminance(arc);
            if (RAIL_LUMA_FLOOR..=RAIL_LUMA_CEIL).contains(&y_full) {
                assert_eq!(ink, arc, "{name}: a stop inside the band is carried PURE");
            } else if y_full > RAIL_LUMA_CEIL {
                // Scaled through its own hue: every channel in the same
                // ratio to the anchor's, to a level of rounding.
                let k = max_channel(ink) as f32 / max_channel(arc) as f32;
                for sh in [16, 8, 0] {
                    let (a, i) = (((arc >> sh) & 0xff) as f32, ((ink >> sh) & 0xff) as f32);
                    assert!(
                        (a * k - i).abs() <= 1.5,
                        "{name}: the rail changed the hue (#{arc:06X} → #{ink:06X})"
                    );
                }
                assert!(
                    y > RAIL_LUMA_CEIL - 0.01,
                    "{name}: scaled to the ceiling, not under it"
                );
            } else {
                assert!(
                    y < RAIL_LUMA_FLOOR + 0.01,
                    "{name}: a cold stop is lifted to the floor and no further"
                );
                assert!(
                    sat(ink) >= 0.5,
                    "{name}: still a colour on the rail (S {:.2})",
                    sat(ink)
                );
            }
            assert!(
                max_channel(lit) >= 130,
                "{name}: vivid on the rail (max {})",
                max_channel(lit)
            );
            assert!(
                relative_luminance(lit) * 255.0 <= RAIL_LUMA_CEIL * 255.0 + 0.5,
                "{name}: the rail composites over its own ceiling"
            );
        }
    }

    /// **THE COMET** (§30): fattest at the hand, thinner behind it. The
    /// reach below the spine is `COMET_DN_HAND_CH` under the caret and the
    /// floor at the tail; the reach above is `TALL_UP_CH` at the head and
    /// `COMET_UP_TAIL_CH` past the taper; both fall monotonically. The flat
    /// spelling is the same shape at every cell, as it was.
    #[test]
    fn the_comet_is_fattest_at_the_hand_and_thins_behind_it() {
        let g = geom();
        let ch = g.ch as f32;
        let dn = |b: Band| b.bottom - b.spine;
        let up = |b: Band| b.spine - b.top;
        // Reduced motion stills the wave, so the bands read the pure profile.
        let mut comet = cfg(true, true);
        comet.reduced_motion = true;
        let mut flat = flat(true, true);
        flat.reduced_motion = true;
        for c in [&comet, &flat] {
            let t0 = Instant::now();
            let mut rib = Ribbon::new();
            type_run(&mut rib, t0, 10, 24, c, 1.0);
            let cx = ctx(at(t0, 23 * 60 + 8), c, (2, 34), 1.0);
            rib.plan(&cx);
            let head = rib.band(2, 33).expect("the head cell");
            let mid = rib.band(2, 27).expect("six cells behind");
            let tail = rib.band(2, 12).expect("twenty-one cells behind");
            println!(
                "flat {}: dn head/mid/tail = {:.2}/{:.2}/{:.2} ch, up = {:.2}/{:.2}/{:.2} ch",
                c.ribbon_flat,
                dn(head) / ch,
                dn(mid) / ch,
                dn(tail) / ch,
                up(head) / ch,
                up(mid) / ch,
                up(tail) / ch
            );
            if c.ribbon_flat {
                assert!(
                    (dn(head) - DN_TOP_CH * ch).abs() <= 1.0,
                    "the flat wedge at the hand"
                );
                assert!((up(head) - TALL_UP_CH * ch).abs() <= 0.5);
                assert!(
                    (up(tail) - TALL_UP_CH * ch).abs() <= 0.5,
                    "the flat body is one shape"
                );
                continue;
            }
            assert!(
                (dn(head) - COMET_DN_HAND_CH * ch).abs() <= 1.0,
                "the comet's lobe under the hand is {:.2} ch, not {COMET_DN_HAND_CH}",
                dn(head) / ch
            );
            assert!(
                (dn(tail) - DN_FLOOR_CH * ch).abs() <= 1.0,
                "the lobe settles to the floor behind the hand ({:.2} ch)",
                dn(tail) / ch
            );
            assert!(
                dn(head) > dn(mid) && dn(mid) > dn(tail),
                "the lobe closes monotonically"
            );
            assert!(
                (up(head) - TALL_UP_CH * ch).abs() <= 0.5,
                "the head cell keeps the full body ({:.2} ch)",
                up(head) / ch
            );
            assert!(
                (up(tail) - COMET_UP_TAIL_CH * ch).abs() <= 1.0,
                "the tail thins to {COMET_UP_TAIL_CH} ch ({:.2})",
                up(tail) / ch
            );
            assert!(
                up(head) > up(mid) && up(mid) > up(tail),
                "the body thins monotonically"
            );
        }
    }

    /// **THE FROM-THE-HAND ATTACK** (§30): new light enters from the hand.
    /// Since 2026-09-14 the new light is the ATTACH under the caret cell —
    /// the cell the key lays where the last attach stood inherits its
    /// level — and the attach pours in from the head's edge outward. On the
    /// echo frame the whole attach is at the birth floor (T2); from the
    /// second frame on the slab nearest the head's edge leads the far
    /// slab; by `ATTACK_WIPE_S` it is uniform. The flat spelling fades in in place: every slab equal
    /// on every frame.
    #[test]
    fn a_new_cell_s_light_enters_from_the_caret_side() {
        let g = geom();
        let cw = g.cw as f32;
        // The attach: the caret cell, 18, from the head's right edge on.
        let x0 = f32::from(g.origin_x) + 18.0 * cw;
        let x1 = x0 + cw;
        for c in [cfg(true, true), flat(true, true)] {
            let t0 = Instant::now();
            let mut rib = Ribbon::new();
            type_run(&mut rib, t0, 10, 8, &c, 0.9);
            let born = at(t0, 7 * 60);
            let mut frames = Vec::new();
            println!(
                "flat {}: the attach's slabs (cov), the head's edge first; the caret cell's right edge last",
                c.ribbon_flat
            );
            for ms in [0u64, 8, 16, 24, 32, 48] {
                let cx = ctx(at(born, ms), &c, (2, 18), 0.9);
                rib.plan(&cx);
                let slabs: Vec<u8> = rib
                    .plan_segments()
                    .iter()
                    .filter(|s| s.x >= x0 && s.x <= x1)
                    .map(|s| s.cov)
                    .collect();
                println!("  +{ms:2} ms: {slabs:?}");
                assert!(slabs.len() >= 3, "three slabs per cell at this fixture");
                frames.push(slabs);
            }
            // The first vertex is the head's own right edge (the brighter
            // side owns it); the attach's OWN slabs are the rest.
            let own = |f: &Vec<u8>| f[1..].to_vec();
            let floor = (BIRTH_EDGE_FLOOR * UNDER_COV_CAP * 0.8) as u8;
            for &s in &own(&frames[0]) {
                assert!(
                    s >= floor,
                    "the echo frame is on glass at the floor (T2): {s} < {floor}"
                );
                assert!(
                    s <= (BIRTH_EDGE_FLOOR * UNDER_COV_CAP) as u8 + 2,
                    "the echo frame is AT the floor, not above it: {s}"
                );
            }
            let f16 = own(&frames[2]);
            let f24 = own(&frames[3]);
            let last = own(&frames[5]);
            if c.ribbon_flat {
                for f in [&f16, &f24, &last] {
                    let (lo, hi) = (f.iter().min().unwrap(), f.iter().max().unwrap());
                    assert!(hi - lo <= 2, "the flat body fades in IN PLACE: {f:?}");
                }
                continue;
            }
            // +16 ms and +24 ms: the head's side (the first own slab, a
            // third of the cell from the head's edge) leads the far side
            // (the last slab) by a visible margin.
            for (ms, f) in [(16, &f16), (24, &f24)] {
                let (near, far) = (f[0], f[f.len() - 1]);
                assert!(
                    near >= far + 30,
                    "+{ms} ms: the slab at the head's edge ({near}) must lead the far slab ({far}); the light enters from the hand"
                );
            }
            // …and by the wipe's end the cell is uniform.
            let (lo, hi) = (last.iter().min().unwrap(), last.iter().max().unwrap());
            assert!(
                hi - lo <= 2,
                "past ATTACK_WIPE_S the cell is uniform: {last:?}"
            );
        }
    }

    /// **THE GATE** (§30): the flat spelling draws no rail and the shape it
    /// had — nothing below the flat body's own reach, nothing vivid anywhere
    /// in `under`. (The byte-for-byte pin is the deletion golden in
    /// `cursor_glow`, `the_flat_spelling_collapses_every_comet_branch_byte_for_byte`.)
    #[test]
    fn the_flat_spelling_draws_no_rail() {
        let c = flat(true, true);
        assert!(!Ribbon::comet(&c) && !Ribbon::rail_lit(&c));
        assert!(Ribbon::comet(&cfg(true, true)) && Ribbon::rail_lit(&cfg(true, true)));
        assert!(
            !Ribbon::rail_lit(&cfg(false, true)),
            "the light fork keeps the bed alone below the baseline (L6)"
        );
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 10, 8, &c, 0.9);
        let cx = ctx(at(t0, 8 * 60), &c, (2, 18), 0.9);
        rib.plan(&cx);
        let mut sink = Sink::default();
        {
            let mut f = sink.frame();
            rib.emit(&cx, &mut f);
        }
        let g = geom();
        let ch = g.ch as f32;
        let row_bottom = f32::from(g.origin_y) + 3.0 * ch;
        for q in &sink.under {
            assert!(
                f32::from(q.y) < row_bottom + DN_TOP_CH * ch + 1.0,
                "the flat body reaches no deeper than {DN_TOP_CH} ch (y {} vs row bottom {row_bottom})",
                q.y
            );
            let lit = over_premul(DEFAULT_BG, q.color, q.alpha);
            assert!(
                contrast(DEFAULT_FG, lit) >= BODY_CONTRAST_BAR,
                "the flat body is the bed everywhere: #{lit:06X} at y {}",
                q.y
            );
        }
    }

    // -- seam point 5 ------------------------------------------------------

    #[test]
    fn head_rgb_is_the_authored_stop_on_dark_and_the_rail_ink_on_light() {
        let dark = cfg(true, true);
        let light = cfg(false, false);
        let t0 = Instant::now();
        for c in [&dark, &light] {
            let mut rib = Ribbon::new();
            type_run(&mut rib, t0, 10, 6, c, 0.8);
            let cx = ctx(at(t0, 5 * 60), c, (2, 16), 0.8);
            rib.plan(&cx);
            let t = rib.field_at_caret();
            assert!(
                (t - walk_t(5.0)).abs() < 1e-5,
                "the caret's own cell is the field"
            );
            let arc = spectrum(clamp01(tri(t)));
            let got = rib.head_rgb(c).expect("a cursor has been observed");
            if c.dark_theme {
                assert_eq!(got, arc, "dark themes hand the companion the AUTHORED stop");
                assert_ne!(
                    got,
                    bed_ink(arc, bed_luma_budget(c.theme_fg)),
                    "…and this stop is one the bed dims, so the pin is not vacuous"
                );
            } else {
                assert_eq!(
                    got,
                    light_role(c).ink(arc),
                    "light themes hand it the rail's ink"
                );
            }
        }
        assert!(
            Ribbon::new().head_rgb(&dark).is_none(),
            "no cursor observed, no colour"
        );
    }

    // -- §18: the budget ---------------------------------------------------

    #[test]
    fn a_hot_paragraph_at_retina_keeps_its_head_row_whole_and_sheds_only_the_oldest_tail() {
        let c = cfg(true, true);
        let g = geom2x();
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        let mut sink = Sink::default();
        let mut ms = 0u64;
        // 5 ms a key: the three rows must all still be LIVE when the third is
        // done for the BUDGET to be the thing under test. Under the
        // row-scoped one-finger law (2026-09-12) a row the hand has left
        // keeps only the clock it had, so at 20 ms a key the first row would
        // be in its own swoosh by the time the second finished — a drain,
        // not a shed. At 5 ms the first row is 0.76 s idle when the third
        // finishes: still reaching, every cell at full envelope.
        for row in 2..5u16 {
            let keys = Keys {
                g,
                row,
                col0: 2,
                n: 76,
                period_ms: 5,
                disp: 1.0,
            };
            type_keys(&mut rib, at(t0, ms), keys, &c);
            ms += 76 * 5;
            let cx = ctx_in(at(t0, ms), &c, (row, 78), 1.0, g);
            rib.plan(&cx);
            {
                let mut f = sink.frame();
                rib.emit(&cx, &mut f);
            }
            let live_rows = row - 1;
            assert!(
                sink.under.len() <= RIBBON_QUAD_BUDGET,
                "{live_rows} retina rows spent {} quads against a {RIBBON_QUAD_BUDGET} budget",
                sink.under.len()
            );
            // The row under the hand is whole on every frame — one key, one
            // cell, one light (§7.3) — whatever else is live.
            for col in 2..78u16 {
                assert!(
                    cell_lit(&sink.under, g, row, col),
                    "row {row} col {col} is dark on its own frame with {live_rows} rows live"
                );
            }
            match live_rows {
                1 => assert!(
                    rib.slabs_per_cell() < SLABS_PER_CELL,
                    "a 76-cell retina line does not fit at three slabs per cell; the density must give"
                ),
                2 => {
                    for col in 2..78u16 {
                        assert!(
                            cell_lit(&sink.under, g, 2, col),
                            "two retina rows fit the budget; row 2 col {col} was shed anyway"
                        );
                    }
                }
                _ => {
                    // Three do not fit: the newer of the two older rows is
                    // whole, and what is shed comes off the OLDEST row's tail
                    // — contiguous to its head, never out of its middle.
                    for col in 2..78u16 {
                        assert!(
                            cell_lit(&sink.under, g, 3, col),
                            "row 3 col {col} was shed before row 2"
                        );
                    }
                    let lit: Vec<bool> = (2..78u16)
                        .map(|col| cell_lit(&sink.under, g, 2, col))
                        .collect();
                    // Three rows exceed the budget at full density. The
                    // budget answers FIRST with the density dial (§18:
                    // `slabs_for` — and since the comet's rail, 2026-09-13,
                    // the rail's rows are priced in it, so three retina rows
                    // fit at one slab per cell) and only then by shedding
                    // the oldest tail. Either answer is the law; a shed is
                    // never out of the middle.
                    assert!(
                        lit.iter().any(|&l| !l) || rib.slabs_per_cell() == 1,
                        "three retina rows exceed the budget: the density must give or the oldest must lose its tail"
                    );
                    let first = lit.iter().position(|&l| l).unwrap_or(lit.len());
                    assert!(
                        lit[first..].iter().all(|&l| l),
                        "the shed must come off the oldest row's tail, not out of its middle"
                    );
                }
            }
        }
    }

    // -- §18 / §20.1: determinism -----------------------------------------

    #[test]
    fn a_frame_is_a_pure_function_of_its_events_and_now() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let drive = |rib: &mut Ribbon| {
            type_run(rib, t0, 10, 12, &c, 0.85);
            let now = at(t0, 12 * 60);
            let cx = ctx(now, &c, (2, 21), 0.85);
            rib.on_event(&Event::Erase, now, &cx);
            rib.plan(&cx);
            let now = at(t0, 12 * 60 + 200);
            let cx = ctx(now, &c, (2, 50), 0.85);
            rib.on_event(&nav((2, 21), (2, 50)), now, &cx);
            rib.plan(&cx);
            let keys = Keys {
                g: geom(),
                row: 2,
                col0: 50,
                n: 3,
                period_ms: 60,
                disp: 0.85,
            };
            type_keys(rib, at(t0, 12 * 60 + 200), keys, &c);
        };
        let frame = |rib: &mut Ribbon, sink: &mut Sink| {
            let now = at(t0, 12 * 60 + 900);
            let mut cx = ctx(now, &c, (2, 53), 0.5);
            cx.phase = 0.3;
            rib.plan(&cx);
            let mut f = sink.frame();
            rib.emit(&cx, &mut f);
        };
        let (mut a, mut b) = (Ribbon::new(), Ribbon::new());
        drive(&mut a);
        drive(&mut b);
        let (mut sa, mut sb) = (Sink::default(), Sink::default());
        frame(&mut a, &mut sa);
        frame(&mut b, &mut sb);
        assert!(
            !sa.under.is_empty(),
            "the frame must draw for the pin to mean anything"
        );
        assert_eq!(
            sa.under, sb.under,
            "two ribbons fed the same events differ in `under`"
        );
        assert_eq!(sa.out, sb.out, "…or in `out`");
        // …and the SAME ribbon re-planned at the same `now` is the same frame:
        // the plan is a function of `now`, not of how many times it ran.
        let mut again = Sink::default();
        frame(&mut a, &mut again);
        assert_eq!(
            sa.under, again.under,
            "re-planning at one `now` changed the frame"
        );
        assert_eq!(sa.out, again.out);
    }

    #[test]
    fn a_scroll_moves_the_ribbon_with_the_viewport_and_drops_what_leaves() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 10, 4, &c, 0.5);
        assert!(rib.live_cells() > 0);
        rib.translate_scroll(1);
        assert!(rib.cells().iter().all(|l| l.row == 1));
        rib.translate_scroll(4);
        assert!(rib.at_rest(), "light on a line nobody typed is not kept");
    }

    /// Codex's 57-row screen (`ESC[{vt};57r`), the fixture the band tests
    /// are stated on: the measured viewport tops and the pinned composer
    /// row 54 all need rows the 40-row fixture does not have.
    fn geom_codex() -> Geom {
        Geom {
            cw: 9,
            ch: 18,
            rows: 57,
            cols: 151,
            origin_x: 0,
            origin_y: 0,
            win_w: 1359,
            win_h: 1026,
            head: 0,
        }
    }

    /// Type `n` cells on `row` from `col0`, one key per 60 ms, on the Codex
    /// geometry.
    fn type_row(rib: &mut Ribbon, t0: Instant, row: u16, col0: u16, n: u16, c: &Config) {
        type_keys(
            rib,
            t0,
            Keys {
                g: geom_codex(),
                row,
                col0,
                n,
                period_ms: 60,
                disp: 0.5,
            },
            c,
        );
    }

    /// **A ROW BAND CARRIES ITS RIBBON WITH ITS TEXT** (seam point 12, the
    /// band path) — Codex phase A: the inline viewport `[vt..56]` slides
    /// DOWN one row per streamed line with the composer inside it, while the
    /// transcript above `vt` stands still. Six cells typed on the composer
    /// row 20 must land on row 21 with their column, field stop, birth and
    /// cohort untouched; three cells on transcript row 5 must not move; and
    /// BETWEEN the band move and the next plan the field index must already
    /// answer for the moved cells (the engine seeds `caret_t` from the
    /// caret's field before it plans) and for nothing on the vacated row.
    /// Before the fix this batch reached the engine as a reset and the
    /// measured session read `ribbon_segments 16 → 0` on the first line.
    #[test]
    fn a_band_move_carries_the_composer_band_down_and_leaves_the_transcript_alone() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_row(&mut rib, t0, 5, 10, 3, &c);
        type_row(&mut rib, at(t0, 500), 20, 10, 6, &c);
        let before: Vec<Cell> = rib.cells().to_vec();
        assert_eq!(before.iter().filter(|l| l.row == 20).count(), 6);
        assert_eq!(before.iter().filter(|l| l.row == 5).count(), 3);
        let composer: Vec<Option<f32>> = (10..16).map(|col| rib.field_at(20, col)).collect();
        assert!(
            composer.iter().all(Option::is_some),
            "fixture: the composer is lit"
        );
        let transcript: Vec<Option<f32>> = (10..13).map(|col| rib.field_at(5, col)).collect();
        assert!(
            transcript.iter().all(Option::is_some),
            "fixture: the transcript is lit"
        );
        assert_eq!(rib.caret(), Some((20, 16)));

        rib.translate_band(19, 56, 1);

        let after = rib.cells();
        assert_eq!(
            after.len(),
            before.len(),
            "a band move loses no cell inside its band"
        );
        for b in &before {
            let want = if b.row == 20 { 21 } else { b.row };
            assert!(
                after.iter().any(|a| a.row == want
                    && a.col == b.col
                    && a.t == b.t
                    && a.born == b.born
                    && a.cohort == b.cohort
                    && a.retract_at == b.retract_at),
                "cell ({}, {}) did not arrive on row {want} with its clocks and prices",
                b.row,
                b.col
            );
        }
        assert!(
            after.iter().all(|a| a.row != 20),
            "the vacated row keeps no light"
        );
        for (i, col) in (10..16).enumerate() {
            assert_eq!(
                rib.field_at(21, col),
                composer[i],
                "the field moved with its cell"
            );
            assert_eq!(
                rib.field_at(20, col),
                None,
                "the vacated cell answers nothing"
            );
        }
        for (i, col) in (10..13).enumerate() {
            assert_eq!(
                rib.field_at(5, col),
                transcript[i],
                "the transcript stood still"
            );
        }
        assert_eq!(rib.caret(), Some((21, 16)), "the caret rides its band");
    }

    /// Codex phase B: the viewport is pinned at the bottom and every
    /// streamed line archives the transcript `[0..51]` UP one row under a
    /// fixed composer. Light on transcript row 3 rides to row 2; the
    /// composer's light on row 54 and its caret do not move; light on row 0
    /// leaves through the top of the band — dropped, not parked on row 0.
    #[test]
    fn a_top_anchored_band_moves_the_transcript_up_and_pins_the_footer() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_row(&mut rib, t0, 0, 4, 2, &c);
        type_row(&mut rib, at(t0, 300), 3, 10, 4, &c);
        type_row(&mut rib, at(t0, 900), 54, 13, 5, &c);
        let footer: Vec<Option<f32>> = (13..18).map(|col| rib.field_at(54, col)).collect();
        let t3 = rib.field_at(3, 11).expect("fixture: row 3 is lit");
        assert_eq!(rib.caret(), Some((54, 18)));

        rib.translate_band(0, 51, -1);

        assert!(rib.cells().iter().all(|l| l.row != 0 && l.row != 3));
        assert_eq!(
            rib.cells().iter().filter(|l| l.row == 2).count(),
            4,
            "row 3 rode up to row 2"
        );
        assert_eq!(rib.field_at(2, 11), Some(t3));
        assert!(
            rib.cells().iter().all(|l| l.row != u16::MAX),
            "nothing wrapped around the top"
        );
        assert_eq!(
            rib.cells().iter().filter(|l| l.row == 54).count(),
            5,
            "the pinned composer is outside the band and untouched"
        );
        for (i, col) in (13..18).enumerate() {
            assert_eq!(rib.field_at(54, col), footer[i]);
        }
        assert_eq!(
            rib.caret(),
            Some((54, 18)),
            "a caret outside the band stays put"
        );
        assert_eq!(
            rib.cells().len(),
            9,
            "row 0's two cells left through the top; nothing else was lost"
        );
    }

    /// What leaves the band is GONE: a ribbon on the bottom row 56 pushed
    /// down by the viewport's slide has no row to land on, so its cells and
    /// its cohort retire, the field index answers `None` on both the old row
    /// and the row past the edge, and the ribbon is at rest. The caret is a
    /// position and saturates at the band's edge instead.
    #[test]
    fn a_band_move_drops_what_leaves_the_band_and_retires_its_cohort() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_row(&mut rib, t0, 56, 10, 6, &c);
        assert!(rib.live_cells() == 6 && !rib.cohorts.is_empty());
        assert_eq!(rib.index.live_rows(), 1);

        rib.translate_band(50, 56, 1);

        assert!(
            rib.at_rest(),
            "light carried off the band's edge is not kept"
        );
        assert!(
            rib.cohorts.is_empty(),
            "a cohort with no cells retires with them"
        );
        for col in 10..16 {
            assert_eq!(rib.index.at(56, col), None);
            assert_eq!(rib.index.at(57, col), None);
        }
        assert_eq!(
            rib.index.live_rows(),
            0,
            "the lane retired into the spare pool"
        );
        assert_eq!(
            rib.index.spare.len(),
            1,
            "…and is there for the next row that lights"
        );
        assert_eq!(
            rib.caret(),
            Some((56, 16)),
            "the caret saturates at the edge"
        );
    }

    /// **A SCROLL CARRIES THE RIBBON WITH ITS TEXT** (seam point 12 — the
    /// PTY's one-row scroll on every Enter at the foot of the screen): the
    /// hand types four cells and backspaces the last, so the caret stands ON
    /// a retracting cell whose stop is not the newest key's; then the screen
    /// scrolls one row. BETWEEN the scroll and the next plan the ribbon must
    /// already answer for the moved text — the caret one row up, the field
    /// at the caret the SAME cell's stop, nothing left on the old row (the
    /// engine reads these for a jump that shares the tick with the scroll) —
    /// and the NEXT FRAME, planned at the same instant with the host's
    /// translated caret, must be the pre-scroll frame moved up exactly one
    /// cell height: every body quad and every hot-edge quad the same colour
    /// and width, one `ch` higher, and every cell's clocks and prices
    /// untouched. A scroll past the text drops it.
    ///
    /// The frame clause is stated at the 2× fixture (`ch 36`) on purpose:
    /// `ribbon_beam` phases its 4×4 Bayer dither on ABSOLUTE device y, so a
    /// translated frame is byte-identical only when the cell height is a
    /// multiple of four — true of the 2× fixture and of the owner's retina
    /// cell (`ch 28`), false of the 1× fixture's `ch 18`, where the same
    /// move re-phases the dither by two rows and every quad's byte shifts
    /// one level (the rasterizer's dither, not the ribbon's light).
    ///
    /// FAILED before the fix on the first clause after the scroll: the
    /// caret stayed on row 2 and the index was dropped, so `field_at(1, 13)`
    /// was `None` until the next plan and `field_at_caret` fell through to
    /// the newest typing cell's stop (col 12's, not col 13's).
    #[test]
    fn a_scroll_carries_the_ribbon_with_its_text() {
        let c = cfg(true, true);
        let g = geom2x();
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        // Row 2, cols 10..14, the caret at (2, 14); then a Backspace puts
        // col 13 on its retract clock and the caret on it.
        type_run(&mut rib, t0, 10, 4, &c, 0.5);
        let now = at(t0, 4 * 60);
        erase_at(&mut rib, now, (2, 13), &c);
        let t13 = rib
            .field_at(2, 13)
            .expect("the retracting cell keeps its stop");
        let t12 = rib.field_at(2, 12).expect("col 12 is lit");
        assert_ne!(
            t13, t12,
            "the fixture needs a caret stop the newest key does not share"
        );
        assert_eq!(rib.caret(), Some((2, 13)));
        assert_eq!(rib.field_at_caret(), t13);
        assert!(
            rib.cells().iter().any(|l| l.retract_at.is_some()),
            "the fixture needs a cell on its retract clock"
        );
        let cx = ctx_in(now, &c, (2, 13), 0.7, g);
        rib.plan(&cx);
        let mut before = Sink::default();
        {
            let mut f = before.frame();
            rib.emit(&cx, &mut f);
        }
        assert!(
            !before.under.is_empty(),
            "the frame must draw for the law to bite"
        );
        assert!(
            !before.out.is_empty(),
            "the hot edge must draw for the law to bite"
        );
        let cells_before: Vec<Cell> = rib.cells().to_vec();

        rib.translate_scroll(1);

        // Between the scroll and the next plan: the ribbon answers for the
        // moved text already.
        assert_eq!(rib.caret(), Some((1, 13)), "the caret rides the scroll");
        assert_eq!(
            rib.field_at(1, 13),
            Some(t13),
            "the field moved with its cell"
        );
        assert_eq!(rib.field_at(2, 13), None, "nothing is left on the old row");
        assert_eq!(
            rib.field_at_caret(),
            t13,
            "the caret's stop is its own cell's, not the newest key's"
        );
        // Only the row moved: every clock and every price is untouched.
        let cells_after = rib.cells();
        assert_eq!(
            cells_after.len(),
            cells_before.len(),
            "a scroll drops nothing on the grid"
        );
        for (a, b) in cells_before.iter().zip(cells_after) {
            assert_eq!(b.row + 1, a.row);
            assert_eq!(
                (b.col, b.cohort, b.born, b.retract_at, b.typing),
                (a.col, a.cohort, a.born, a.retract_at, a.typing)
            );
            assert_eq!(
                (b.t, b.life_s, b.cov0, b.birth_disp),
                (a.t, a.life_s, a.cov0, a.birth_disp)
            );
        }
        // The next frame, planned at the same instant with the host's
        // translated caret, is the pre-scroll frame one cell height higher.
        let cx1 = ctx_in(now, &c, (1, 13), 0.7, g);
        rib.plan(&cx1);
        let mut after = Sink::default();
        {
            let mut f = after.frame();
            rib.emit(&cx1, &mut f);
        }
        let ch = u16::try_from(g.ch).expect("ch");
        assert_eq!(
            after.under.len(),
            before.under.len(),
            "the body kept every quad"
        );
        for (a, b) in before.under.iter().zip(&after.under) {
            assert_eq!(
                (b.x, b.w, b.h, b.color, b.alpha),
                (a.x, a.w, a.h, a.color, a.alpha)
            );
            assert_eq!(
                b.y + ch,
                a.y,
                "a body quad moved by {} px, not one cell height ({ch})",
                i32::from(a.y) - i32::from(b.y)
            );
            assert_eq!(b.row + 1, a.row, "the damage hint rides with the quad");
        }
        assert_eq!(
            after.out.len(),
            before.out.len(),
            "the hot edge kept every quad"
        );
        for (a, b) in before.out.iter().zip(&after.out) {
            assert_eq!(
                (b.x, b.w, b.h, b.color, b.alpha),
                (a.x, a.w, a.h, a.color, a.alpha)
            );
            assert_eq!(
                b.y + ch,
                a.y,
                "a hot-edge quad moved by {} px, not one cell height ({ch})",
                i32::from(a.y) - i32::from(b.y)
            );
        }
        // A scroll past the text drops it: light on a line nobody typed.
        rib.translate_scroll(2);
        assert!(rib.at_rest());
        assert_eq!(rib.field_at(0, 13), None);
    }

    // -- §18 cadence: a fade's next level is solved, not polled -------------

    /// **A FADE'S NEXT u8 LEVEL IS SOLVED IN CLOSED FORM** (§18's cadence
    /// law): for the theme's `spend` and the cell's expiry melt, over bright
    /// and dim marks and every phase of the fade, the instant
    /// [`level_step_spend`] / [`level_step_melt`] names is the first at
    /// which `round(peak · curve)` actually drops, to within 0.2 ms of a
    /// 10 µs poll — and `None` exactly when the mark is already at zero.
    #[test]
    fn a_fades_next_level_is_solved_not_polled() {
        let poll = |level_now: f32, curve: &dyn Fn(f32) -> f32, u: f32, span: f32| -> f32 {
            let mut t = 0.0f32;
            loop {
                t += 0.000_01;
                if (curve(u + t / span)).round() < level_now {
                    return t;
                }
                assert!(t < 5.0, "the polled level never dropped");
            }
        };
        let mut checked = 0usize;
        for peak in [UNDER_COV_CAP * 0.6, 30.0, 4.0] {
            for u in [0.0, 0.3, 0.7, 0.9, 0.97] {
                let span = RETRACT_FADE_S;
                let level = (peak * spend(u)).round();
                match level_step_spend(peak, u, span) {
                    None => assert!(
                        level < 1.0,
                        "spend peak {peak} u {u}: None while at {level}"
                    ),
                    Some(dt) => {
                        let got = poll(level, &|v| peak * spend(v), u, span);
                        assert!(
                            (dt - got).abs() < 2e-4,
                            "spend peak {peak} u {u}: solved {dt} s, polled {got} s"
                        );
                        checked += 1;
                    }
                }
                let life = 1.7;
                let level = (peak * expiry_melt(u)).round();
                match level_step_melt(peak, u, life) {
                    None => assert!(level < 1.0, "melt peak {peak} u {u}: None while at {level}"),
                    Some(dt) => {
                        let got = poll(level, &|v| peak * expiry_melt(v), u, life);
                        assert!(
                            (dt - got).abs() < 2e-4,
                            "melt peak {peak} u {u}: solved {dt} s, polled {got} s"
                        );
                        checked += 1;
                    }
                }
            }
        }
        assert!(checked >= 20, "only {checked} steps checked");
    }

    // -- the cohort's clock (2026-09-08, the owner's screenshot) -------------

    /// **A LINE STILL BEING TYPED KEEPS EVERY CELL FROM ITS FIRST KEY TO THE
    /// CARET.** The owner's screenshot ("rainbow" lit, "theme" DARK, "truly"
    /// half lit, "magical" lit, "and" DARK, "specai" lit at the caret):
    /// "rainbow" at 9 cps on a hot spine (`birth_disp` 0.9, `cell_life`
    /// 5.88 s), a 700 ms thinking pause, "theme" slowly at 4 cps on the
    /// dipped spine (0.45, 2.11 s), then " truly magical" at 8 cps hot again.
    /// Planned every 16 ms until the last key, every glyph cell from the
    /// first key to the caret's neighbour is in the pool. FAILED before the
    /// cohort's clock (measured): at +4.192 s "theme"'s `h` — born at
    /// +2.08 s with 2.11 s of life (its `t`, the first key after the pause,
    /// took the chain floor `4.5 × 0.95 s + 0.1 = 4.38 s`) — ran out on its
    /// own clock while "rainbow" (5.88 s) on its left and " truly" on its
    /// right stood lit: a dark cell inside the live span, under a hand that
    /// was still typing (key 23 of 27). Also pinned here, UNCHANGED: after the last key
    /// nothing is dropped through the 0.75 s grace, and the mark is off the
    /// glass — pool empty, at rest — [`SWOOSH_TOTAL_S`] after it.
    #[test]
    fn a_line_still_being_typed_keeps_every_cell_from_its_first_key_to_the_caret() {
        let c = cfg(true, true);
        let g = geom();
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        let row = 4u16;
        let col0 = 2u16;
        // `(word, ms per key, pause before, birth_disp)`.
        let words: [(&str, u64, u64, f32); 4] = [
            ("rainbow ", 110, 0, 0.9),
            ("theme", 250, 700, 0.45),
            (" truly ", 125, 0, 0.9),
            ("magical", 125, 0, 0.9),
        ];
        let mut key_ms = 0u64;
        let mut caret = col0;
        let mut now_ms = 0u64;
        let mut dark: Vec<String> = Vec::new();
        let mut keys = 0;
        for (word, per_key, pause, disp) in words {
            key_ms += pause;
            for _ in word.chars() {
                key_ms += per_key;
                while now_ms + 16 <= key_ms {
                    now_ms += 16;
                    let cx = ctx_in(at(t0, now_ms), &c, (row, caret), disp, g);
                    rib.plan(&cx);
                    let missing: Vec<u16> = (col0..caret)
                        .filter(|&col| rib.field_at(row, col).is_none())
                        .collect();
                    if !missing.is_empty() && dark.len() < 6 {
                        dark.push(format!(
                            "+{:.3}s keys={keys} caret={caret} dark={missing:?}",
                            now_ms as f32 / 1000.0
                        ));
                    }
                }
                caret += 1;
                keys += 1;
                let cx = ctx_in(at(t0, key_ms), &c, (row, caret), disp, g);
                rib.on_event(&typed(), at(t0, key_ms), &cx);
                rib.plan(&cx);
            }
        }
        assert_eq!(keys, 27);
        assert!(
            dark.is_empty(),
            "a cell went dark under a hand still typing:\n{}",
            dark.join("\n")
        );
        // UNCHANGED: the swoosh is still the ending. Through the grace every
        // cell stands; SWOOSH_TOTAL_S after the last key the mark is gone.
        let last = key_ms;
        let cx = ctx_in(at(t0, last + 700), &c, (row, caret), 0.9, g);
        rib.plan(&cx);
        assert_eq!(
            (col0..caret)
                .filter(|&col| rib.field_at(row, col).is_some())
                .count(),
            usize::from(caret - col0),
            "every cell stands through the 0.75 s grace"
        );
        let gone = last + (SWOOSH_TOTAL_S * 1000.0) as u64 + 20;
        let cx = ctx_in(at(t0, gone), &c, (row, caret), 0.9, g);
        rib.plan(&cx);
        let mut sink = Sink::default();
        rib.emit(&cx, &mut sink.frame());
        assert!(
            rib.at_rest() && sink.under.is_empty() && sink.out.is_empty(),
            "off the glass {SWOOSH_TOTAL_S} s after the last key"
        );
    }

    // -- 2026-09-13: the owner's gaps ----------------------------------------

    /// A typed echo licensed as one move — `from` → `to` on the same row.
    fn typed_move(from: (u16, u16), to: (u16, u16)) -> Event {
        Event::Move {
            from,
            to,
            licence: Licence::Typed,
            dir: Dir::of(
                i32::from(to.1) - i32::from(from.1),
                i32::from(to.0) - i32::from(from.0),
            ),
        }
    }

    fn typed_space() -> Event {
        Event::Typed {
            cells: 1,
            shifted: false,
            class: TypedClass::Space,
        }
    }

    /// The planned coverage of the slabs INSIDE cell `col` on row 2 (the
    /// interior vertices, boundaries excluded).
    fn interior_covs(rib: &Ribbon, col: u16) -> Vec<u8> {
        let cw = geom().cw as f32;
        let (x0, x1) = (f32::from(col) * cw, f32::from(col + 1) * cw);
        rib.plan_segments()
            .iter()
            .filter(|s| s.x > x0 + 0.5 && s.x < x1 - 0.5)
            .map(|s| s.cov)
            .collect()
    }

    /// The planned coverage AT the boundary `col` (the vertex on its x).
    fn boundary_cov(rib: &Ribbon, col: u16) -> Option<u8> {
        let x = f32::from(col) * geom().cw as f32;
        rib.plan_segments()
            .iter()
            .find(|s| (s.x - x).abs() < 0.5)
            .map(|s| s.cov)
    }

    /// THE OWNER'S SLITS: two keys whose echoes land on one tick replay as
    /// `Typed, Typed, Sweep, Move` against the landing caret; the first
    /// `Typed` lays one past the live cohort's end and used to mint a second
    /// cohort, which `build_runs` split into two runs and `plan_run`
    /// feathered at [`RUN_TAIL_EASE`] — a 5/4/5 px dark ramp at the last
    /// cell of every such batch, 14/14 on the installed app. One cohort now,
    /// one run, no interior boundary under the body's level.
    #[test]
    fn a_coalesced_echo_leaves_one_cohort_and_no_feather_inside_the_row() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 2, 6, &c, 0.9);
        // The batch: keys at +360/+370 ms, both echoed on the tick at +380.
        let k1 = at(t0, 360);
        let k2 = at(t0, 370);
        let now = at(t0, 380);
        let cx = ctx(now, &c, (2, 10), 0.9);
        rib.on_event(&typed(), k1, &cx);
        rib.on_event(&typed(), k2, &cx);
        rib.on_event(
            &Event::Sweep {
                row: 2,
                col0: 8,
                col1: 10,
            },
            k1,
            &cx,
        );
        rib.on_event(&typed_move((2, 8), (2, 10)), now, &cx);
        rib.plan(&cx);
        assert_eq!(
            rib.cohorts().iter().filter(|k| k.row == 2).count(),
            1,
            "the batch's last cell joined the row's one cohort: {:?}",
            rib.cohorts()
        );
        for col in 2..10u16 {
            assert!(
                rib.cells().iter().any(|l| l.row == 2 && l.col == col),
                "col {col} is laid"
            );
        }
        // Settled, every interior boundary carries the body's own level:
        // nothing at a tenth, nothing at a quarter.
        let cx = ctx(at(t0, 500), &c, (2, 10), 0.9);
        rib.plan(&cx);
        let body = boundary_cov(&rib, 6).expect("a boundary inside the band");
        for col in 3..10u16 {
            let b = boundary_cov(&rib, col).expect("boundary {col}");
            assert!(
                u32::from(b) * 10 >= u32::from(body) * 9,
                "boundary {col} is feathered: {b} against the body's {body}"
            );
        }
        // …and a second batch on the same tick shape does not seam either.
        let k3 = at(t0, 560);
        let k4 = at(t0, 570);
        let now = at(t0, 580);
        let cx = ctx(now, &c, (2, 12), 0.9);
        rib.on_event(&typed(), k3, &cx);
        rib.on_event(&typed(), k4, &cx);
        rib.on_event(
            &Event::Sweep {
                row: 2,
                col0: 10,
                col1: 12,
            },
            k3,
            &cx,
        );
        rib.on_event(&typed_move((2, 10), (2, 12)), now, &cx);
        rib.plan(&cx);
        assert_eq!(rib.cohorts().iter().filter(|k| k.row == 2).count(), 1);
    }

    /// A key replayed against a caret whose cell is already lit lays that
    /// cell again and keeps its attack age instead of restarting the 18 ms
    /// edge-in on a settled cell. (Since 2026-09-14 the engine holds a key
    /// whose echo has not landed rather than replaying it a tick early; the
    /// shape that still reaches this re-lay is a glyph typed before a
    /// one-shot's move, replayed at the caret before that move — its run's
    /// own last cell.)
    #[test]
    fn a_key_replayed_before_its_echo_keeps_the_previous_cell_s_attack() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 2, 4, &c, 0.9);
        let cx = ctx(at(t0, 400), &c, (2, 6), 0.9);
        rib.plan(&cx);
        let attack_at = rib
            .cells()
            .iter()
            .find(|l| l.row == 2 && l.col == 5)
            .expect("the last glyph's cell")
            .attack_at;
        let before = interior_covs(&rib, 5);
        // The next key, replayed on a tick that precedes its echo: the
        // caret still stands at 6, so the lay lands on cell 5 again.
        let k = at(t0, 420);
        let cx = ctx(at(t0, 421), &c, (2, 6), 0.9);
        rib.on_event(&typed(), k, &cx);
        rib.plan(&cx);
        let cell = rib
            .cells()
            .iter()
            .find(|l| l.row == 2 && l.col == 5)
            .expect("still one cell");
        assert_eq!(cell.attack_at, attack_at, "the attack was restarted");
        assert_eq!(cell.born, k, "a fresh key must not inherit the old witness");
        assert_eq!(
            interior_covs(&rib, 5),
            before,
            "cell 5 dipped on the re-lay"
        );
    }

    /// THE BRIGHTER SIDE OWNS A BOUNDARY: a new cell's attack lives inside
    /// the new light — the settled cell beside it keeps every one of its
    /// slabs — and an erased neighbour's spend never sags the surviving
    /// head cell, so nothing pops when the erased cell retires. Since
    /// 2026-09-14 the new light is the ATTACH under the caret cell: the
    /// cell the key lays where the attach stood inherits the level the
    /// attach already had (no byte moves down on the key), and the attack
    /// is the next attach's, one cell on.
    #[test]
    fn a_boundary_takes_the_brighter_cell_s_coverage() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 2, 4, &c, 0.9);
        let cx = ctx(at(t0, 400), &c, (2, 6), 0.9);
        rib.plan(&cx);
        let settled = interior_covs(&rib, 5);
        let edge = boundary_cov(&rib, 6).expect("the head's right edge");
        // A new key: its cell attacks over 18 ms; frame 2 ms in.
        let k = at(t0, 420);
        let cx = ctx(at(t0, 422), &c, (2, 7), 0.9);
        rib.on_event(&typed(), k, &cx);
        rib.plan(&cx);
        assert_eq!(
            interior_covs(&rib, 5),
            settled,
            "cell 5 dipped under cell 6's attack"
        );
        assert!(
            boundary_cov(&rib, 6).expect("the shared boundary") >= edge - 1,
            "the shared boundary sagged toward the attacking cell"
        );
        let laid: Vec<u8> = interior_covs(&rib, 6);
        assert!(
            laid.iter().all(|&v| v + 1 >= edge),
            "the cell laid where the attach stood keeps the attach's level: {laid:?} under {edge}"
        );
        let young: Vec<u8> = interior_covs(&rib, 7);
        assert!(
            !young.is_empty() && young.iter().all(|&v| v < edge),
            "the attack lives in the attach under the caret cell: {young:?} under {edge}"
        );
        // Settle, then erase the head: the survivor's slabs hold while the
        // erased neighbour spends, and hold when it retires.
        let cx = ctx(at(t0, 700), &c, (2, 7), 0.9);
        rib.plan(&cx);
        let hold = interior_covs(&rib, 5);
        let hold_edge = boundary_cov(&rib, 6).expect("edge");
        erase_at(&mut rib, at(t0, 720), (2, 6), &c);
        for ms in [730u64, 800, 900, 940, 970, 1000] {
            let cx = ctx(at(t0, ms), &c, (2, 6), 0.9);
            rib.plan(&cx);
            assert_eq!(
                interior_covs(&rib, 5),
                hold,
                "+{ms} ms: the survivor sagged"
            );
            assert!(
                boundary_cov(&rib, 6).expect("edge") + 1 >= hold_edge,
                "+{ms} ms: the survivor's edge sagged"
            );
        }
        assert!(
            !rib.cells().iter().any(|l| l.col == 6),
            "the erased cell retired inside {RETRACT_FADE_S} s"
        );
    }

    /// THE EDITING HAND HOLDS THE ROW: a Backspace run refreshes the cohort
    /// like a typed key — the survivors do not drain away under a hand that
    /// is still editing, and the fix key finds them whole.
    #[test]
    fn an_erase_holds_the_row_s_cohort_through_the_edit() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 2, 8, &c, 0.9);
        let last = at(t0, 7 * 60);
        erase_at(&mut rib, at(last, 400), (2, 9), &c);
        erase_at(&mut rib, at(last, 800), (2, 8), &c);
        erase_at(&mut rib, at(last, 1200), (2, 7), &c);
        let cx = ctx(at(last, 1500), &c, (2, 7), 0.9);
        rib.plan(&cx);
        let coh = rib
            .cohorts()
            .iter()
            .find(|k| k.row == 2)
            .expect("the cohort");
        assert!(
            matches!(coh.phase, Phase::Laying | Phase::Grace),
            "the cohort went into its exit under the editing hand: {:?}",
            coh.phase
        );
        for col in 2..7u16 {
            assert!(
                rib.cells()
                    .iter()
                    .any(|l| l.row == 2 && l.col == col && l.retract_at.is_none()),
                "survivor {col} was drained under the hand"
            );
        }
        assert!(
            boundary_cov(&rib, 4).expect("inside the survivors") >= STATUS_LIT_COV,
            "the survivors are lit"
        );
    }

    /// A Backspace whose retreat echoes a tick late: the `Erase` was replayed
    /// against the pre-retreat caret and found nothing to its right; the
    /// typed retreat that follows retracts from ITS landing.
    #[test]
    fn an_erase_whose_echo_lands_a_tick_late_still_retracts_the_erased_cell() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 2, 8, &c, 0.9);
        let last = at(t0, 7 * 60);
        // The erase, replayed on a tick that precedes its echo.
        let k = at(last, 300);
        let cx = ctx(k, &c, (2, 10), 0.9);
        rib.on_event(&Event::Erase, k, &cx);
        rib.plan(&cx);
        assert!(
            rib.cells().iter().all(|l| l.retract_at.is_none()),
            "nothing right of the pre-retreat caret to retract"
        );
        // The retreat lands on the next tick.
        let now = at(last, 316);
        let cx = ctx(now, &c, (2, 9), 0.9);
        rib.on_event(&typed_move((2, 10), (2, 9)), now, &cx);
        rib.plan(&cx);
        let erased = rib
            .cells()
            .iter()
            .find(|l| l.row == 2 && l.col == 9)
            .expect("the erased glyph's cell is still spending");
        assert!(
            erased.retract_at.is_some(),
            "the erased cell stayed lit under the blank"
        );
        assert!(
            rib.cells()
                .iter()
                .filter(|l| l.col < 9)
                .all(|l| l.retract_at.is_none()),
            "only the erased cell retracts"
        );
        // A typed key clears the pending erase: a later re-anchor of the
        // hand's own is not read as an erase's retreat.
        let now = at(last, 400);
        let cx = ctx(now, &c, (2, 10), 0.9);
        rib.on_event(&typed(), now, &cx);
        rib.plan(&cx);
        assert_eq!(rib.pending_erase, 0);
    }

    /// THE HAND LEFT THE ROW: a typed move onto the next row abandons the
    /// row it left into the point it left at, and keys on the new row do
    /// not hold it — it is gone well inside the swoosh.
    #[test]
    fn a_typed_row_change_lets_the_old_row_leave_toward_the_wrap_point() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        let keys = Keys {
            g: geom(),
            row: 2,
            col0: 100,
            n: 20,
            period_ms: 60,
            disp: 0.9,
        };
        type_keys(&mut rib, t0, keys, &c);
        let last = at(t0, 19 * 60);
        let now = at(last, 60);
        let cx = ctx(now, &c, (3, 0), 0.9);
        rib.on_event(&typed_move((2, 120), (3, 0)), now, &cx);
        rib.plan(&cx);
        let old = rib
            .cohorts()
            .iter()
            .find(|k| k.row == 2)
            .expect("the old row");
        assert!(old.abandoned, "the old row was not abandoned");
        assert_eq!(
            old.retract_col,
            Some(120),
            "it retracts into the wrap point"
        );
        // Keys on the new row: the old row is not held.
        let keys = Keys {
            g: geom(),
            row: 3,
            col0: 0,
            n: 12,
            period_ms: 60,
            disp: 0.9,
        };
        type_keys(&mut rib, at(now, 60), keys, &c);
        let cx = ctx(at(now, 1000), &c, (3, 12), 0.9);
        rib.plan(&cx);
        assert!(
            !rib.cells().iter().any(|l| l.row == 2),
            "the old row is still lit 1 s after the hand left it"
        );
        assert!(rib.cells().iter().any(|l| l.row == 3), "the new row is lit");
    }

    /// A COMPOSER'S RE-ANCHOR RELAYS THE MOVED WORD: Ink moves the row's last
    /// word down with the wrap key and the caret lands after it — the word's
    /// cells are laid on the new row from the last Space's measure, and its
    /// old cells leave with the old row. Both observed shapes: one move to
    /// the word's end, and the two-move rewrite whose first half parks at
    /// the box's inset.
    #[test]
    fn a_composer_re_anchor_relays_the_moved_word_on_the_new_row() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let script = |rib: &mut Ribbon| {
            // "ab cd" typed at cols 70..75 on row 2; the caret stands at 75.
            let mut now = t0;
            for (i, ev) in [typed(), typed(), typed_space(), typed(), typed()]
                .iter()
                .enumerate()
            {
                now = at(t0, i as u64 * 60);
                let cx = ctx(now, &c, (2, 71 + i as u16), 0.9);
                rib.on_event(ev, now, &cx);
                rib.plan(&cx);
            }
            now
        };
        // ONE MOVE: 'e' wraps "cd" down; "cde" lands at 2..5, caret 5.
        let mut rib = Ribbon::new();
        let last = script(&mut rib);
        let k = at(last, 60);
        let cx = ctx(k, &c, (3, 5), 0.9);
        rib.on_event(&typed(), k, &cx);
        rib.on_event(&typed_move((2, 75), (3, 5)), k, &cx);
        rib.plan(&cx);
        for col in 2..5u16 {
            assert!(
                rib.cells()
                    .iter()
                    .any(|l| l.row == 3 && l.col == col && l.typing),
                "one move: the moved word's cell {col} is lit on the new row"
            );
        }
        assert!(
            !rib.cells().iter().any(|l| l.row == 3 && l.col < 2),
            "one move: nothing left of the inset"
        );
        let old = rib
            .cohorts()
            .iter()
            .find(|k| k.row == 2)
            .expect("the old row");
        assert!(old.abandoned && old.retract_col == Some(75));
        // TWO MOVES: the caret parks at the inset first; the seam refuses
        // the second half; the next key's licensed echo settles it.
        let mut rib = Ribbon::new();
        let last = script(&mut rib);
        let k = at(last, 60);
        let cx = ctx(k, &c, (3, 2), 0.9);
        rib.on_event(&typed(), k, &cx);
        rib.on_event(&typed_move((2, 75), (3, 2)), k, &cx);
        rib.plan(&cx);
        assert!(
            !rib.cells().iter().any(|l| l.row == 3),
            "the parked caret lays nothing on the new row (the marker's cell stays dark): {:?}",
            rib.cells()
                .iter()
                .filter(|l| l.row == 3)
                .map(|l| l.col)
                .collect::<Vec<_>>()
        );
        let k2 = at(last, 130);
        let cx = ctx(k2, &c, (3, 6), 0.9);
        rib.on_event(&typed(), k2, &cx);
        rib.on_event(&typed_move((3, 5), (3, 6)), k2, &cx);
        rib.plan(&cx);
        for col in 2..6u16 {
            assert!(
                rib.cells().iter().any(|l| l.row == 3 && l.col == col),
                "two moves: cell {col} is lit once the landing is known"
            );
        }
        assert!(!rib.cells().iter().any(|l| l.row == 3 && l.col < 2));
        assert_eq!(rib.cohorts().iter().filter(|k| k.row == 3).count(), 1);
    }

    /// A PROGRAM'S ROUND TRIP does not strand the band: a TUI repaints a row
    /// below and parks the cursor back; the hand's next key takes the band
    /// back, and the moment in between retracted toward the hand's column,
    /// never the parked one.
    #[test]
    fn a_program_round_trip_does_not_strand_the_band_under_a_typing_hand() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 2, 8, &c, 0.9);
        let last = at(t0, 7 * 60);
        let cx = ctx(at(last, 100), &c, (3, 0), 0.9);
        rib.on_event(&pty((2, 10), (3, 0)), at(last, 100), &cx);
        rib.plan(&cx);
        let coh = rib.cohorts().iter().find(|k| k.row == 2).expect("the band");
        assert!(coh.abandoned && coh.rejoinable);
        assert_eq!(
            coh.retract_col,
            Some(10),
            "retracts toward the hand, not the parked cursor"
        );
        let cx = ctx(at(last, 116), &c, (2, 10), 0.9);
        rib.on_event(&pty((3, 0), (2, 10)), at(last, 116), &cx);
        rib.plan(&cx);
        let k = at(last, 200);
        let cx = ctx(k, &c, (2, 11), 0.9);
        rib.on_event(&typed(), k, &cx);
        rib.plan(&cx);
        let cohorts: Vec<_> = rib.cohorts().iter().filter(|k| k.row == 2).collect();
        assert_eq!(cohorts.len(), 1, "one band, taken back: {cohorts:?}");
        assert!(!cohorts[0].abandoned);
        assert_eq!((cohorts[0].col0, cohorts[0].col1), (2, 11));
        assert!(
            rib.cells()
                .iter()
                .filter(|l| l.row == 2)
                .all(|l| l.retract_at.is_none()),
            "no cell is left draining under the hand"
        );
    }

    /// A rebirth on the row after a FINISHED swoosh continues the walk from
    /// where the retired band ended, inside the chain window.
    #[test]
    fn a_rebirth_after_a_finished_swoosh_continues_the_walk() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 2, 10, &c, 0.9);
        let want = rib.cohorts()[0].t_at(12);
        let last = at(t0, 9 * 60);
        let cx = ctx(at(last, 3000), &c, (2, 12), 0.0);
        rib.plan(&cx);
        assert!(rib.at_rest(), "the swoosh has finished");
        let k = at(last, 3100);
        let cx = ctx(k, &c, (2, 13), 0.3);
        rib.on_event(&typed(), k, &cx);
        rib.plan(&cx);
        let reborn = rib.cohorts().iter().find(|k| k.row == 2).expect("reborn");
        assert!(
            (reborn.t0 - want).abs() < 1e-5,
            "the walk restarted at {} instead of continuing at {want}",
            reborn.t0
        );
        // Past the chain window, red again.
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 2, 10, &c, 0.9);
        let cx = ctx(at(last, 6000), &c, (2, 12), 0.0);
        rib.plan(&cx);
        let k = at(last, 6100);
        let cx = ctx(k, &c, (2, 13), 0.3);
        rib.on_event(&typed(), k, &cx);
        rib.plan(&cx);
        assert_eq!(
            rib.cohorts()
                .iter()
                .find(|k| k.row == 2)
                .expect("reborn")
                .t0,
            0.0
        );
    }

    /// The swoosh shortens the mark INTO the hand and stops at the caret
    /// block: no column lights past the block's right edge, and the ATTACH
    /// under the caret cell (2026-09-14) — lit since the key, the head's own
    /// light — only ever SHRINKS toward the block's left edge with the rest
    /// of the mark: no fresh column lights inside the caret cell.
    #[test]
    fn the_swoosh_stops_at_the_caret_block_s_left_edge() {
        let c = cfg(true, true);
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_run(&mut rib, t0, 2, 8, &c, 0.9);
        let last = at(t0, 7 * 60);
        let caret_x = 10.0 * geom().cw as f32;
        let block_right = caret_x + geom().cw as f32;
        let mut reach_prev = f32::INFINITY;
        for ms in [1000u64, 1100, 1200, 1300, 1400] {
            let cx = ctx(at(last, ms), &c, (2, 10), 0.0);
            rib.plan(&cx);
            let reach = rib
                .plan_segments()
                .iter()
                .map(|s| s.x)
                .fold(f32::NEG_INFINITY, f32::max);
            assert!(
                reach <= block_right + 0.5,
                "+{ms} ms: a slab at x {reach} is past the caret block (right edge {block_right})"
            );
            assert!(
                reach <= reach_prev + 0.5,
                "+{ms} ms: the mark's reach grew from {reach_prev} to {reach} under a lifted hand"
            );
            reach_prev = reach;
            for s in rib.plan_segments() {
                assert!(
                    s.x <= block_right + 0.5,
                    "+{ms} ms: a slab at x {} is past the caret block (right edge {block_right})",
                    s.x
                );
            }
        }
    }

    // -- 2026-09-14: the owner's notch and slivers ---------------------------
    //
    // THE OWNER, on the shipped v0.85.0, Claude Code's composer, `and I also
    // seee an ugly gap` typed at 12 cps, the caret a filled block right of
    // "gap": "there is a right angle of black from the cursor to the
    // underline. also there are some painting errors" — "isn't painting
    // nicely from the cursor". Measured on glass before the fix (headless,
    // his cell, `docs/measured/cursor-notch-2026-09-14.md`): the band ended
    // at the block's left edge, its rail 24 device rows deep, so the ground
    // showed as a 31 × 24 px black foot under the block; and every run's
    // first boundary was feathered whatever stood to its left. The pins
    // below are stated on a per-device-column census of the emitted quads
    // at his geometry (`geom_owner`, 30 × 56 device px), at the densities
    // his glass affords (three slabs per cell for the composer line alone,
    // one for a screenful of lit rows).

    /// The owner's glass: 144 × 60 cells of 15 × 28 pt at scale 2 — 30 × 56
    /// device px.
    fn geom_owner() -> Geom {
        Geom {
            cw: 30,
            ch: 56,
            rows: 60,
            cols: 144,
            origin_x: 0,
            origin_y: 0,
            win_w: 4320,
            win_h: 3360,
            head: 0,
        }
    }

    /// The owner's config on his glass: the default dark theme at his
    /// `intensity 0.70`.
    fn cfg_owner() -> Config {
        let mut c = cfg(true, true);
        c.intensity = 0.70;
        c
    }

    /// The owner's row on his glass: the composer line, row 56 of 60.
    const OWNER_ROW: u16 = 56;

    /// Type the owner's line on [`OWNER_ROW`] from column 2 at
    /// `period_ms` a key with a `pause_ms` thinking pause after `also ` (the
    /// first key of `seee` is a word's first key), planning after every key
    /// as the engine does. Returns the last key's offset from `t0` and the
    /// caret column after it.
    fn type_owner_line(
        rib: &mut Ribbon,
        t0: Instant,
        c: &Config,
        disp: f32,
        period_ms: u64,
        pause_ms: u64,
    ) -> (u64, u16) {
        let g = geom_owner();
        let mut ms = 0u64;
        let mut col = 2u16;
        let mut last = 0u64;
        for (i, _) in "and I also seee an ugly gap".chars().enumerate() {
            let now = at(t0, ms);
            let cx = ctx_in(now, c, (OWNER_ROW, col + 1), disp, g);
            rib.on_event(&typed(), now, &cx);
            rib.plan(&cx);
            col += 1;
            last = ms;
            ms += period_ms;
            if i == 10 {
                ms += pause_ms;
            }
        }
        (last, col)
    }

    /// One frame on the owner's glass: plan and emit at `now` with the caret
    /// at `caret`, into `sink`.
    fn owner_frame(
        rib: &mut Ribbon,
        sink: &mut Sink,
        c: &Config,
        now: Instant,
        caret: u16,
        disp: f32,
    ) {
        let cx = ctx_in(now, c, (OWNER_ROW, caret), disp, geom_owner());
        rib.plan(&cx);
        let mut f = sink.frame();
        rib.emit(&cx, &mut f);
    }

    /// The device rows the owner's row's band can occupy: half a cell above
    /// its top (the crown and the sky's stars are above that) to a cell
    /// below its bottom (the rail).
    fn owner_rows() -> std::ops::Range<i32> {
        let g = geom_owner();
        let top = i32::from(OWNER_ROW) * g.ch as i32;
        top - g.ch as i32 / 2..top + 2 * g.ch as i32
    }

    /// **THE ATTACH** — the notch, pinned. Under a live caret the band's top
    /// edge holds its plateau height through the head cell up to the block's
    /// left edge, and the body — the rail's foot and the crown, not only the
    /// strip — continues under the caret cell, so the block stands ON the
    /// band: no device column from the head cell's left edge to the block's
    /// right edge is ground-dark, none has a top lit row under the plateau
    /// by more than one row, none a rail shallower than the head's by more
    /// than one row, and none a peak more than a dither step under the
    /// head's. On a COLD hand (the first key of a word, momentum 0) as on a
    /// hot one — and the hot edge's hairline stands over the head cell on
    /// both, because its existence no longer waits for momentum
    /// ([`HOT_EDGE_GAIN_FLOOR`]).
    ///
    /// RED on main on both hands: the caret cell's thirty columns read DARK
    /// (the band ended at the block's left edge — a 30 × 22 px foot of
    /// ground under the block on this geometry), and the cold hand's `out`
    /// stream was empty (`hot_edge_gain` was exactly zero under
    /// `HOT_EDGE_DISP_MIN`).
    #[test]
    fn the_caret_block_stands_on_the_band_under_a_cold_hand_and_a_hot_one() {
        for (disp, hand) in [(0.0f32, "cold"), (0.7, "hot")] {
            let c = cfg_owner();
            let g = geom_owner();
            let cw = g.cw as i32;
            let t0 = Instant::now();
            let mut rib = Ribbon::new();
            let (last, caret) = type_owner_line(&mut rib, t0, &c, disp, 83, 700);
            let mut sink = Sink::default();
            owner_frame(&mut rib, &mut sink, &c, at(t0, last + 100), caret, disp);
            assert_eq!(
                rib.slabs_per_cell(),
                3,
                "{hand}: the composer line alone plans at three slabs"
            );
            let head_x0 = i32::from(caret - 1) * cw;
            let block_x1 = i32::from(caret + 1) * cw;
            let x0 = head_x0 - 3 * cw;
            let cens = column_census(&sink.under, x0..block_x1, owner_rows(), 8);
            // The plateau: the highest top over the three cells before the head.
            let plateau = cens[..3 * cw as usize]
                .iter()
                .filter_map(|e| e.map(|(t, _, _)| t))
                .min()
                .expect("the cells before the head are lit");
            let head = &cens[3 * cw as usize..4 * cw as usize];
            let head_bot = head
                .iter()
                .filter_map(|e| e.map(|(_, b, _)| b))
                .max()
                .unwrap_or(0);
            let head_peak = head
                .iter()
                .filter_map(|e| e.map(|(_, _, p)| p))
                .max()
                .unwrap_or(0);
            let mut dark = Vec::new();
            for (k, e) in cens.iter().enumerate().skip(3 * cw as usize) {
                let x = x0 + k as i32;
                let Some((top, bot, peak)) = *e else {
                    dark.push(x);
                    continue;
                };
                assert!(
                    top <= plateau + 1,
                    "{hand}: column {x} tops out at {top}, under the plateau's {plateau}"
                );
                if x >= head_x0 + cw {
                    assert!(
                        bot >= head_bot - 1,
                        "{hand}: the foot under the block at column {x} ends at {bot}, the head's rail at {head_bot}"
                    );
                    assert!(
                        peak + 2 >= head_peak,
                        "{hand}: the band under the block at column {x} peaks at {peak}, the head at {head_peak}"
                    );
                }
            }
            assert!(
                dark.is_empty(),
                "{hand}: ground-dark columns between the head cell and the block's right edge: {} of them, from x {}",
                dark.len(),
                dark[0]
            );
            // THE HAIRLINE stands over the head cell, cold hand included.
            let hot = column_census(&sink.out, head_x0..head_x0 + cw, owner_rows(), 1);
            let unlit = hot.iter().filter(|e| e.is_none()).count();
            assert_eq!(
                unlit, 0,
                "{hand}: {unlit} of the head cell's columns carry no hot edge"
            );
        }
    }

    /// One frame's census on the owner's row: `(ms, caret, column_census
    /// over the leg's columns at floor 8)`.
    type FrameCensus = (u64, u16, Vec<Option<(i32, i32, u8)>>);

    /// One frame of `column_census` per 16 ms on the owner's row over `ms`:
    /// before each frame `step` applies whatever the leg has due at that
    /// instant and names the caret column the frame is planned at.
    fn owner_frames(
        rib: &mut Ribbon,
        t0: Instant,
        c: &Config,
        disp: f32,
        ms: std::ops::RangeInclusive<u64>,
        xs: std::ops::Range<i32>,
        mut step: impl FnMut(&mut Ribbon, u64) -> u16,
    ) -> Vec<FrameCensus> {
        let mut sink = Sink::default();
        let mut frames = Vec::new();
        let (mut ms, to_ms) = (*ms.start(), *ms.end());
        while ms <= to_ms {
            let caret = step(rib, ms);
            owner_frame(rib, &mut sink, c, at(t0, ms), caret, disp);
            frames.push((
                ms,
                caret,
                column_census(&sink.under, xs.clone(), owner_rows(), 8),
            ));
            ms += 16;
        }
        frames
    }

    /// The fade's own steepest per-frame slope, in levels: `spend` falls at
    /// most `2 / RETRACT_FADE_S` a second from a 215-level body, 29 levels a
    /// 16 ms frame, plus the wave's own anti-aliasing at the plateau.
    const FADE_STEP_MAX: i32 = 36;

    /// **THE BLOCK STANDS ON WHOLE LIGHT AFTER A JUMP, AND ON THE KEYS THAT
    /// FOLLOW IT** (2026-09-14, the second review round). The round's
    /// headline is that the caret block stands ON the band; the pins for it
    /// all put the block at a typed run's head, so a rule that feathered the
    /// end the hand types at whenever the run still carried a corridor
    /// shipped green. Ten keys, Home (cols 0..1 are dark, so the move lays a
    /// corridor), End (back over the corridor's own live cells — a scrub),
    /// then three more keys: the head cell and the block's own cell must
    /// read the band's own level across every device column of the owner's
    /// 30 × 56 cell.
    ///
    /// RED under the rejected `Cohort::landing_col` rule: the head cell read
    /// 182 / 117 / 53 across its three slab thirds and the block's own cell
    /// 20 flat — 9% of the band, standing, for the corridor's whole life.
    #[test]
    fn the_block_stands_on_whole_light_after_a_jump_and_the_keys_that_follow_it() {
        let g = geom_owner();
        let cw = g.cw as i32;
        let row = OWNER_ROW;
        for (disp, hand) in [(0.0f32, "cold"), (0.7, "hot")] {
            let c = cfg_owner();
            let t0 = Instant::now();
            let mut rib = Ribbon::new();
            let keys = Keys {
                g,
                row,
                col0: 2,
                n: 10,
                period_ms: 83,
                disp,
            };
            type_keys(&mut rib, t0, keys, &c);
            let mut ms = 9 * 83 + 120;
            for (from, to) in [((row, 12u16), (row, 0u16)), ((row, 0), (row, 12))] {
                let now = at(t0, ms);
                let cx = ctx_in(now, &c, to, disp, g);
                rib.on_event(&nav(from, to), now, &cx);
                rib.plan(&cx);
                ms += 120;
            }
            let mut caret = 12u16;
            for _ in 0..3 {
                ms += 83;
                caret += 1;
                let now = at(t0, ms);
                let cx = ctx_in(now, &c, (row, caret), disp, g);
                rib.on_event(&typed(), now, &cx);
                rib.plan(&cx);
            }
            ms += 300;
            let mut sink = Sink::default();
            owner_frame(&mut rib, &mut sink, &c, at(t0, ms), caret, disp);
            // The plateau is read off the four settled cells before the head;
            // the head cell and the block's own cell must match it.
            let x0 = i32::from(caret - 5) * cw;
            let x1 = i32::from(caret + 1) * cw;
            let cens = column_census(&sink.under, x0..x1, owner_rows(), 8);
            let plateau = cens[..4 * cw as usize]
                .iter()
                .filter_map(|e| e.map(|(_, _, p)| p))
                .max()
                .expect("the band before the head is lit");
            let show = |lo: usize| {
                (0..3)
                    .map(|j| cens[lo + j * (cw as usize / 3)].map_or(0, |(_, _, p)| i32::from(p)))
                    .collect::<Vec<_>>()
            };
            for (k, e) in cens.iter().enumerate().skip(4 * cw as usize) {
                let x = x0 + k as i32;
                let peak = e.map_or(0, |(_, _, p)| p);
                assert!(
                    i32::from(plateau) - i32::from(peak) <= 2,
                    "{hand}: device column {x} reads {peak} against the band's {plateau} — the head cell is {:?} and the block's own {:?}",
                    show(4 * cw as usize),
                    show(5 * cw as usize)
                );
            }
        }
    }

    /// **THE BLOCK STANDS ON THE BAND THROUGH A LEAVING CARET CELL** — read
    /// off `frames` (`owner_frames` over device columns from `x0`, `cw` px
    /// a cell): on every frame every column of the caret cell is lit; no
    /// column of the span rises by more than a dither step (2 levels) in
    /// one frame — in particular not the frame the leaving cell retires and
    /// the attach takes its place; and no column from the caret cell's left
    /// edge to `hold_x1` — the cells the block has stood on — falls faster
    /// than the fade's own slope: what the block leaves behind drains from
    /// the level it showed.
    fn assert_block_on_band(frames: &[FrameCensus], x0: i32, cw: i32, hold_x1: i32, leg: &str) {
        for (i, (ms, caret, cens)) in frames.iter().enumerate() {
            let cx0 = i32::from(*caret) * cw;
            let dark: Vec<i32> = (cx0..cx0 + cw)
                .filter(|x| cens[(x - x0) as usize].is_none())
                .collect();
            assert!(
                dark.is_empty(),
                "{leg}, +{ms} ms, caret {caret}: {} ground-dark columns under the block, from x {}",
                dark.len(),
                dark[0]
            );
            if i == 0 {
                continue;
            }
            let prev = &frames[i - 1].2;
            let level = |e: &Option<(i32, i32, u8)>| e.map_or(0, |(_, _, p)| i32::from(p));
            for (k, (now, was)) in cens.iter().zip(prev.iter()).enumerate() {
                let x = x0 + k as i32;
                let (now, was) = (level(now), level(was));
                assert!(
                    now - was <= 2,
                    "{leg}, +{ms} ms: column {x} rose {was} -> {now} in one frame"
                );
                if (cx0..hold_x1).contains(&x) {
                    assert!(
                        was - now <= FADE_STEP_MAX,
                        "{leg}, +{ms} ms: column {x} fell {was} -> {now} in one frame, faster than the fade"
                    );
                }
            }
        }
    }

    /// **THE ATTACH SURVIVES AN ERASE** (review of the attach, 2026-09-14).
    /// A Backspace leaves the caret over the cell it erased, which drains
    /// under the block (`Cell::retract_at`) and retires 240 ms on; a
    /// content retirement (`Ribbon::retire_cells`) puts a melting cell
    /// under the caret the same way. The attach's law is "the caret cell
    /// carries no LIVE light", so a leaving cell under the caret is floored
    /// at the attach's level ([`Ribbon::plan_run`]): the block stands on
    /// the band while the cell spends, the frame that retires it changes no
    /// byte, and a cell the block leaves behind — a held Backspace at 30/s
    /// — drains from the level it showed, its retract restarted the frame
    /// the block leaves it ([`Ribbon::build`]). Four legs, each on a cold
    /// and a hot hand, per frame at 16 ms through the retirement, on the
    /// owner's geometry: one Backspace; four at 30/s; the caret's own run's
    /// cells retired by content under the caret (an insert shifted the
    /// glyphs); and a neighbour cohort's cell — an arrow's wake — retiring
    /// under the caret.
    ///
    /// RED on b4510ab2 (the attach's first cut, whose predicate was `head ==
    /// col1 && right_nb.is_none()`): the caret cell's peak went 215 → 107
    /// over 224 ms with its darkest column reaching 0 — the notch back under
    /// the block — then at +240 ms the erased cell retired, the attach
    /// switched on and the cell jumped to 214 in ONE frame: a 107-level
    /// rise at the peak, 214 at the darkest column.
    #[test]
    fn the_block_stays_on_the_band_while_the_cell_under_it_leaves_and_when_it_is_gone() {
        let g = geom_owner();
        let cw = g.cw as i32;
        let row = OWNER_ROW;
        let fade_ms = (RETRACT_FADE_S * 1000.0) as u64;
        let melt_ms = (RETIRE_MELT_S * 1000.0) as u64;
        for (disp, hand) in [(0.0f32, "cold"), (0.7, "hot")] {
            // LEGS 1 AND 2: `erases` Backspaces `period` ms apart after an
            // eight-key word (cells 2..=9, the caret at 10).
            for (period, erases, leg) in [
                (0u64, 1u16, "one Backspace"),
                (33, 4, "Backspace held at 30/s"),
            ] {
                let leg = format!("{hand} hand, {leg}");
                let c = cfg_owner();
                let t0 = Instant::now();
                let mut rib = Ribbon::new();
                let keys = Keys {
                    g,
                    row,
                    col0: 2,
                    n: 8,
                    period_ms: 83,
                    disp,
                };
                type_keys(&mut rib, t0, keys, &c);
                let ms0 = 7 * 83 + 300;
                let last_erase = ms0 + u64::from(erases - 1) * period;
                let mut done = 0u16;
                let mut caret = 10u16;
                let x0 = i32::from(10 - erases) * cw;
                let frames = owner_frames(
                    &mut rib,
                    t0,
                    &c,
                    disp,
                    ms0 - 16..=last_erase + fade_ms + 200,
                    x0..11 * cw,
                    |rib, ms| {
                        while done < erases && ms >= ms0 + u64::from(done) * period {
                            caret -= 1;
                            let now = at(t0, ms);
                            let cx = ctx_in(now, &c, (row, caret), disp, g);
                            rib.on_event(&Event::Erase, now, &cx);
                            done += 1;
                        }
                        caret
                    },
                );
                assert_block_on_band(&frames, x0, cw, 10 * cw, &leg);
                assert!(
                    !rib.cells().iter().any(|l| l.row == row && l.col >= caret),
                    "{leg}: the erased cells retired inside the window"
                );
                assert_eq!(caret, 10 - erases, "{leg}: every erase landed");
            }
            // LEG 3: the caret's own run's cells retired by CONTENT under the
            // caret — three arrows left into the word, a key typed there,
            // and the witness retiring the cells whose glyphs the insert
            // shifted (the caret cell first among them).
            {
                let leg = format!("{hand} hand, own cells retired under the caret");
                let c = cfg_owner();
                let t0 = Instant::now();
                let mut rib = Ribbon::new();
                let keys = Keys {
                    g,
                    row,
                    col0: 2,
                    n: 8,
                    period_ms: 83,
                    disp,
                };
                type_keys(&mut rib, t0, keys, &c);
                let mut ms = 7 * 83 + 300;
                let cx = ctx_in(at(t0, ms), &c, (row, 7), disp, g);
                rib.on_event(&nav((row, 10), (row, 7)), at(t0, ms), &cx);
                rib.plan(&cx);
                ms += 300;
                let cx = ctx_in(at(t0, ms), &c, (row, 8), disp, g);
                rib.on_event(&typed(), at(t0, ms), &cx);
                rib.plan(&cx);
                let ms0 = ms + 60;
                let ids: Vec<(u16, u16, Instant)> = rib
                    .cells()
                    .iter()
                    .filter(|l| l.row == row && l.col >= 8)
                    .map(|l| (l.row, l.col, l.born))
                    .collect();
                let mut retired = false;
                let x0 = 7 * cw;
                let frames = owner_frames(
                    &mut rib,
                    t0,
                    &c,
                    disp,
                    ms0 - 16..=ms0 + melt_ms + 200,
                    x0..11 * cw,
                    |rib, ms| {
                        if !retired && ms >= ms0 {
                            // TWO, not three (at the merge with the scrub
                            // round): the three arrows left over the word's
                            // own lit cells are a SCRUB now, which lays no
                            // wake at all, and the key at col 8 re-lays the
                            // cell at col 7 in place rather than minting one
                            // — so the cells at and right of the caret are
                            // the typed word's own two, and the witness call
                            // alone retires them.
                            assert_eq!(
                                rib.retire_cells(&ids, at(t0, ms)),
                                2,
                                "{leg}: the shifted cells retire"
                            );
                            retired = true;
                        }
                        8
                    },
                );
                assert_block_on_band(&frames, x0, cw, 9 * cw, &leg);
                assert!(
                    !rib.cells().iter().any(|l| l.row == row && l.col >= 8),
                    "{leg}: the retired cells left the pool inside the window"
                );
            }
            // LEG 4: a NEIGHBOUR cohort's cell under the caret — a three-key
            // word, an arrow hop right over empty cells (a wake of its own
            // cohort, 5..=9), the hop back onto the word's end, and the wake
            // retired by content under the caret.
            {
                let leg = format!("{hand} hand, a wake's cell retired under the caret");
                let c = cfg_owner();
                let t0 = Instant::now();
                let mut rib = Ribbon::new();
                let keys = Keys {
                    g,
                    row,
                    col0: 2,
                    n: 3,
                    period_ms: 83,
                    disp,
                };
                type_keys(&mut rib, t0, keys, &c);
                // The hop's wake lives `WAKE_LIFE_S · WAKE_HOP_LIFE_SHARE`
                // from the hop and is born 100 ms after it: back and
                // retired inside that.
                let mut ms = 2 * 83 + 200;
                let cx = ctx_in(at(t0, ms), &c, (row, 9), disp, g);
                rib.on_event(&nav((row, 5), (row, 9)), at(t0, ms), &cx);
                rib.plan(&cx);
                ms += 120;
                let cx = ctx_in(at(t0, ms), &c, (row, 5), disp, g);
                rib.on_event(&nav((row, 9), (row, 5)), at(t0, ms), &cx);
                rib.plan(&cx);
                let typed_cohort = rib
                    .cells()
                    .iter()
                    .find(|l| l.row == row && l.col == 4)
                    .expect("the word's last cell")
                    .cohort;
                let ids: Vec<(u16, u16, Instant)> = rib
                    .cells()
                    .iter()
                    .filter(|l| l.row == row && l.col >= 5)
                    .map(|l| (l.row, l.col, l.born))
                    .collect();
                assert!(
                    rib.cells()
                        .iter()
                        .any(|l| l.row == row && l.col == 5 && l.cohort != typed_cohort),
                    "{leg}: the wake is a cohort of its own"
                );
                let ms0 = ms + 80;
                let mut retired = false;
                // The span is the head, the caret cell and the wake's next
                // cell: the wake's FAR end is its own matter — the frame
                // its landing cell starts leaving, `head_col` resolves the
                // run's newest cell instead and its stream flips, so the
                // tail feather moves from one end of the wake to the other
                // (a ~30-level rise at the far end, on main too).
                let x0 = 4 * cw;
                let frames = owner_frames(
                    &mut rib,
                    t0,
                    &c,
                    disp,
                    ms0 - 16..=ms0 + melt_ms + 200,
                    x0..7 * cw,
                    |rib, ms| {
                        if !retired && ms >= ms0 {
                            assert!(
                                rib.retire_cells(&ids, at(t0, ms)) >= 5,
                                "{leg}: the wake retires"
                            );
                            retired = true;
                        }
                        5
                    },
                );
                assert_block_on_band(&frames, x0, cw, 6 * cw, &leg);
                assert!(
                    !rib.cells().iter().any(|l| l.row == row && l.col >= 5),
                    "{leg}: the retired wake left the pool inside the window"
                );
            }
        }
    }

    /// **THE FLOOR IS A TYPED KEY'S** (review, 2026-09-14): the hot edge's
    /// gain floor exists so the first key of a word attaches the band's top
    /// to the caret; a cold arrow hop's wake is no key, and its head carries
    /// no hairline on a cold hand — exactly as before the floor — while a
    /// hot hand's wake keeps the hairline it always had.
    ///
    /// RED on b4510ab2: a cold `nav((56,2),(56,8))` emitted 131 `out` quads
    /// (peak 35 at intensity 0.70) where main emitted none.
    #[test]
    fn a_cold_arrow_s_wake_carries_no_hairline_and_a_hot_one_keeps_its_own() {
        let g = geom_owner();
        let row = OWNER_ROW;
        for (disp, hand, lit) in [(0.0f32, "cold", false), (0.7, "hot", true)] {
            let c = cfg_owner();
            let t0 = Instant::now();
            let mut rib = Ribbon::new();
            let cx = ctx_in(t0, &c, (row, 8), disp, g);
            rib.on_event(&nav((row, 2), (row, 8)), t0, &cx);
            rib.plan(&cx);
            let mut sink = Sink::default();
            owner_frame(&mut rib, &mut sink, &c, at(t0, 200), 8, disp);
            assert!(
                !sink.under.is_empty(),
                "{hand}: the hop's wake is laid and lit"
            );
            assert_eq!(
                !sink.out.is_empty(),
                lit,
                "{hand} hand: the wake's hairline ({} out quads) — a floor is a typed key's",
                sink.out.len()
            );
        }
    }

    /// The peak level per device column over `xs` inside the owner's row
    /// window, `0` where nothing is lit.
    fn column_peaks(under: &[GlowQuad], xs: std::ops::Range<i32>) -> Vec<i32> {
        column_census(under, xs, owner_rows(), 1)
            .iter()
            .map(|e| e.map_or(0, |(_, _, p)| i32::from(p)))
            .collect()
    }

    /// Columns of `peaks` (offsets into it) darker than BOTH sides by more
    /// than `tol` levels — the brightest column within a cell to the left
    /// and within a cell to the right — past the first `skip` columns (the
    /// run's oldest cell, whose feather is designed).
    fn sliver_columns(peaks: &[i32], cw: usize, skip: usize, tol: i32) -> Vec<usize> {
        (skip..peaks.len())
            .filter(|&i| {
                let left = peaks[i.saturating_sub(cw)..i]
                    .iter()
                    .copied()
                    .max()
                    .unwrap_or(0);
                let right = peaks[i + 1..(i + 1 + cw).min(peaks.len())]
                    .iter()
                    .copied()
                    .max()
                    .unwrap_or(0);
                peaks[i] + tol < left && peaks[i] + tol < right
            })
            .collect()
    }

    /// **NO SLIVERS** — a boundary two live runs share carries the older
    /// side's light. A word typed on the owner's glass and an arrow hop
    /// beside it (the hop's wake is a cohort of its own, laid abutting the
    /// word — `heal_seams` folds typed cohorts only) print ONE continuous
    /// band: no device column inside it is darker than both its sides by
    /// more than a dither step, and the top edge steps no more than one
    /// device row between adjacent columns, except at the run's oldest cell.
    ///
    /// RED on main: the wake run's first boundary took the flat tenth
    /// (`left.is_none()`), a column at a quarter of the body's level and a
    /// three-slab ramp back up — "a dark cell plus a step" between the word
    /// and its wake.
    #[test]
    fn a_boundary_two_live_runs_share_carries_the_older_side_s_light() {
        let c = cfg_owner();
        let g = geom_owner();
        let cw = g.cw as i32;
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        type_keys(
            &mut rib,
            t0,
            Keys {
                g,
                row: OWNER_ROW,
                col0: 2,
                n: 6,
                period_ms: 83,
                disp: 0.7,
            },
            &c,
        );
        // The hop: → three times, coalesced — a wake on 8, 9, 10 of its own.
        let hop = at(t0, 5 * 83 + 120);
        let cx = ctx_in(hop, &c, (OWNER_ROW, 11), 0.7, g);
        rib.on_event(&nav((OWNER_ROW, 8), (OWNER_ROW, 11)), hop, &cx);
        rib.plan(&cx);
        let mut sink = Sink::default();
        owner_frame(&mut rib, &mut sink, &c, at(hop, 300), 11, 0.7);
        assert_eq!(
            rib.cohorts().iter().filter(|k| k.row == OWNER_ROW).count(),
            2,
            "the word and its wake are two cohorts: {:?}",
            rib.cohorts()
        );
        assert_eq!(rib.runs.len(), 2, "…and two runs abutting at column 8");
        let x0 = 2 * cw;
        let x1 = 12 * cw;
        let peaks = column_peaks(&sink.under, x0..x1);
        let dark: Vec<i32> = peaks
            .iter()
            .enumerate()
            .filter(|(_, p)| **p == 0)
            .map(|(i, _)| x0 + i as i32)
            .collect();
        assert!(dark.is_empty(), "dark columns inside the band: {dark:?}");
        let slivers: Vec<i32> = sliver_columns(&peaks, cw as usize, cw as usize, 2)
            .into_iter()
            .map(|i| x0 + i as i32)
            .collect();
        assert!(
            slivers.is_empty(),
            "columns darker than both sides by more than a dither step: {slivers:?} (peaks {:?})",
            &peaks[5 * cw as usize..7 * cw as usize]
        );
        let tops = column_census(&sink.under, x0 + cw..x1, owner_rows(), 8);
        for w in tops.windows(2) {
            let (Some(a), Some(b)) = (w[0], w[1]) else {
                continue;
            };
            assert!(
                (a.0 - b.0).abs() <= 1,
                "the top edge steps {} rows between adjacent columns ({} → {})",
                (a.0 - b.0).abs(),
                a.0,
                b.0
            );
        }
    }

    /// **THE FEATHER GROWS IN AS ITS NEIGHBOUR GOES** — the Claude Code
    /// re-anchor. `and I also ` typed, then the composer's licensed park
    /// (`(56,13) → (56,2)`, `licence=key`: `re_anchor` drains the row's cells
    /// toward the landing over the retract's 0.4 s) and its return on the
    /// same tick, then `seee an ugly gap` typed on at 12 cps while the old
    /// cells fade and retire one by one under the new word. Read at 16 ms
    /// from the park through the hand's grace: no device column's peak
    /// FALLS by more than the fade's own steepest frame between two frames
    /// (36 levels — the retract's `spend` is `(1 − u)²` over 0.24 s, so its
    /// first 16 ms take `2 × 251 × 16/240 ≈ 34` levels off the strip's
    /// peak), and on every settled frame (past the newest cell's 40 ms
    /// wipe) no column
    /// inside the live band is darker than both its sides by more than a
    /// dither step.
    ///
    /// RED on main: the frame an old cell RETIRED moved the run's first
    /// boundary onto the next cell, whose left edge fell from the body's
    /// level to a tenth of it in one tick — a dark column appearing at a
    /// word boundary under a hand still typing, the owner's sliver at
    /// `seee|an`.
    #[test]
    fn the_feather_grows_in_as_a_drained_neighbour_goes_never_in_one_frame() {
        let c = cfg_owner();
        let g = geom_owner();
        let cw = g.cw as i32;
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        let first = "and I also ";
        let mut ms = 0u64;
        let mut col = 2u16;
        for _ in first.chars() {
            let now = at(t0, ms);
            let cx = ctx_in(now, &c, (OWNER_ROW, col + 1), 0.7, g);
            rib.on_event(&typed(), now, &cx);
            rib.plan(&cx);
            col += 1;
            ms += 83;
        }
        // The park and its return, one tick.
        let park = at(t0, ms + 20);
        let cx = ctx_in(park, &c, (OWNER_ROW, col), 0.7, g);
        rib.on_event(&typed_move((OWNER_ROW, col), (OWNER_ROW, 2)), park, &cx);
        rib.on_event(&typed_move((OWNER_ROW, 2), (OWNER_ROW, col)), park, &cx);
        rib.plan(&cx);
        let mut keys: Vec<(u64, u16)> = Vec::new();
        let mut k = ms + 70;
        for _ in "seee an ugly gap".chars() {
            keys.push((k, col + 1));
            col += 1;
            k += 83;
        }
        let last_key = keys.last().map_or(k, |e| e.0);
        let mut sink = Sink::default();
        let x0 = 2 * cw;
        let x1 = 30 * cw;
        let mut prev: Option<Vec<i32>> = None;
        let mut next_key = 0usize;
        let mut caret = col - 16;
        let mut worst_drop = (0i32, 0i32, 0u64);
        let mut slivers_seen = Vec::new();
        let mut t = ms + 20;
        let mut last_key_at = 0u64;
        // Through the hand's grace and no further: past it the swoosh's
        // retract pulls the mark's edge into the caret, and a column the
        // edge passes goes dark in a frame by design (`retract_x`).
        // RESTATED AT THE 0.86 RC MERGE (2026-09-14): main wrote this bound
        // as `LIFT_GRACE_S` (0.75 s), the literal Rainbow Path v3 §2.6 S5
        // replaced with the melody's own phrase rest. The grace in force is
        // `Cohort::rest_s` — `phrase_rest_ms(ioi) = max(900 ms, 2.5 · ioi)`
        // — and this script types at a steady 83 ms, so `2.5 · 83 = 208 ms`
        // is under the floor and the rest IS `PHRASE_REST_MIN_S`. The law
        // read here is unchanged and the window is 150 ms WIDER than main
        // measured it, so the feather is now watched over the whole of the
        // grace the merged tree actually gives this hand.
        let until = last_key + (PHRASE_REST_MIN_S * 1000.0) as u64;
        while t <= until {
            while next_key < keys.len() && keys[next_key].0 <= t {
                let (kt, kc) = keys[next_key];
                let now = at(t0, kt);
                let cx = ctx_in(now, &c, (OWNER_ROW, kc), 0.7, g);
                rib.on_event(&typed(), now, &cx);
                caret = kc;
                last_key_at = kt;
                next_key += 1;
            }
            owner_frame(&mut rib, &mut sink, &c, at(t0, t), caret, 0.7);
            let peaks = column_peaks(&sink.under, x0..x1);
            if let Some(p) = &prev {
                for (i, (&a, &b)) in p.iter().zip(&peaks).enumerate() {
                    if a - b > worst_drop.0 {
                        worst_drop = (a - b, x0 + i as i32, t);
                    }
                }
            }
            let settled = t.saturating_sub(last_key_at) >= 60;
            if settled {
                // The LIVE band: the cells that are not leaving, to the
                // block. The drained cells to its left go out farthest from
                // the park first — a valley opening between the old word
                // and the new is the re-anchor's own drain, not a seam —
                // and the live band's first cell, whose left edge the
                // feather grows into as its neighbour goes, may feather.
                let live_col = rib
                    .cells()
                    .iter()
                    .filter(|l| !l.leaving())
                    .map(|l| l.col)
                    .min()
                    .unwrap_or(2);
                let first = (usize::from(live_col) - 2) * cw as usize;
                let live = &peaks[first.min(peaks.len())..];
                for i in sliver_columns(live, cw as usize, cw as usize, 2) {
                    slivers_seen.push((t, x0 + (first + i) as i32, live[i]));
                }
            }
            prev = Some(peaks);
            t += 16;
        }
        assert!(
            rib.cells().iter().all(|l| l.col >= 13),
            "by the end of the grace the drained cells have retired"
        );
        assert!(
            worst_drop.0 <= 36,
            "a column's light fell {} levels in one 16 ms frame (x {}, +{} ms): the feather popped in",
            worst_drop.0,
            worst_drop.1,
            worst_drop.2
        );
        assert!(
            slivers_seen.is_empty(),
            "columns darker than both sides inside the live band: {:?}",
            &slivers_seen[..slivers_seen.len().min(8)]
        );
    }

    /// **ONE SLAB PER CELL** — a screenful of lit rows on the owner's glass
    /// (three hundred cells on three rows above the composer line) drives
    /// `slabs_per_cell` to one and the frame to the budget's edge, and the
    /// attach and the seam law hold there too: the block stands on the
    /// band, no column inside the composer line's band is a sliver, and the
    /// top edge steps no more than the comet taper's own per-cell lerp
    /// (three device rows, from 0.30 ch over twelve cells resolved once per
    /// cell — see `docs/measured/cursor-notch-2026-09-14.md` for the price
    /// of finer).
    #[test]
    fn the_attach_and_the_seam_law_hold_at_one_slab_per_cell() {
        let c = cfg_owner();
        let g = geom_owner();
        let cw = g.cw as i32;
        let t0 = Instant::now();
        let mut rib = Ribbon::new();
        let mut ms = 0u64;
        for row in 50..53u16 {
            type_keys(
                &mut rib,
                at(t0, ms),
                Keys {
                    g,
                    row,
                    col0: 1,
                    n: 100,
                    period_ms: 1,
                    disp: 0.7,
                },
                &c,
            );
            ms += 100;
        }
        let mut col = 2u16;
        for _ in "and I also seee an ugly gap".chars() {
            let now = at(t0, ms);
            let cx = ctx_in(now, &c, (OWNER_ROW, col + 1), 0.7, g);
            rib.on_event(&typed(), now, &cx);
            rib.plan(&cx);
            col += 1;
            ms += 20;
        }
        let mut sink = Sink::default();
        owner_frame(&mut rib, &mut sink, &c, at(t0, ms - 20 + 100), col, 0.7);
        assert_eq!(rib.slabs_per_cell(), 1, "327 retina cells plan at one slab");
        assert!(
            sink.under.len() + 2 > RIBBON_QUAD_BUDGET - 200,
            "the frame is at the budget's edge: {} quads",
            sink.under.len()
        );
        let head_x0 = i32::from(col - 1) * cw;
        let block_x1 = i32::from(col + 1) * cw;
        let cens = column_census(&sink.under, head_x0 - 3 * cw..block_x1, owner_rows(), 8);
        let plateau = cens[..3 * cw as usize]
            .iter()
            .filter_map(|e| e.map(|(t, _, _)| t))
            .min()
            .expect("lit");
        for (k, e) in cens.iter().enumerate().skip(3 * cw as usize) {
            let x = head_x0 - 3 * cw + k as i32;
            let (top, _, _) = e.unwrap_or_else(|| panic!("column {x} is ground-dark at one slab"));
            assert!(
                top <= plateau + 1,
                "column {x} tops out at {top} under the plateau {plateau}"
            );
        }
        let x0 = 2 * cw;
        let peaks = column_peaks(&sink.under, x0..block_x1);
        let slivers = sliver_columns(&peaks, cw as usize, cw as usize, 2);
        assert!(slivers.is_empty(), "slivers at one slab: {slivers:?}");
        let tops = column_census(&sink.under, x0 + cw..block_x1, owner_rows(), 8);
        let worst = tops
            .windows(2)
            .filter_map(|w| Some((w[0]?.0 - w[1]?.0).abs()))
            .max()
            .unwrap_or(0);
        assert!(
            worst <= 3,
            "the top edge steps {worst} rows at one slab per cell"
        );
    }
}
