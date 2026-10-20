<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-predict: spec recovery from its unit tests

> Status 2026-10-16, after the merge with the tests-and-gates audit: the rows naming 6 tests it
> deleted are gone, and the spec rows no longer cite those tests. Counts elsewhere in this file
> were taken before.

> Status 2026-10-15: this is the crate's migration worklist. Each spec below is proved
> through Trust/Clean about the compiled Rust function, and then the tests it subsumes are
> deleted. The one test in §4 that samples no spec can be deleted now. Nothing has been
> proved yet.
>
> 2026-10-16: two contracts changed, found by the safety theorem
> (`tests/reconciled_view_safety.rs`, a seeded property test over a real engine): nothing is
> armed behind a line terminator while the line's guesses are in flight (`predict-accept-decline`),
> and a matching glyph proves a guess only when it is attributable (`reconcile-contract`, with
> `Owed` below). The rows that state them, `queue-cap`'s test name and the §2 map are updated; three
> regression tests are added there. The counts in this header predate them.
>
> 2026-10-16, after review: `Owed` is per guess (`g.owed`, the newest owe from before it), a guess
> retired without credit owes its own echo, every owe is dated by its press (the notes take `now`),
> owed echoes travel in `LinkEstimate`, and a Backspace cancels a glyph declined since the newest guess
> (`declined_tail`). The rows `predict-accept-decline`, `reconcile-contract`,
> `submit-carries-proven-guesses`, `unmodelled-input-retires`, `backspace-pops-own` and
> `link-estimate-swap` state them; §2 maps the eight tests that pin each owe site and change.
>
> 2026-10-17, predictive editing phase 0: the safety theorem's link model is now the predictor's
> own glitch window (never less than 250 ms), which a fixed 250 ms deadline had made vacuous on
> every slower link (§5.9). It found that an expiry owed nothing and kept the epoch's proof (§5.10):
> an expired guess now owes its key's echo and voids the proof of its epoch (`expiry-contract`,
> `visibility-model-refinement`, the model's `Expire`). Each fix is a `Rule` the debug-only
> `Predictor::disable_rule_for_test` seam switches off, and the theorem catches each one in its
> clause at a named directed case. The theorem gains FRAME FIDELITY (every displayed frame equals the
> screen its newest key settles to, every cell and the cursor, with a decoration rule) and
> TIMELINESS (nothing on glass older than the window, or flushed), which found that a mid-line echo
> proved its epoch (§5.11): a credited echo now proves only over a clean tail (`tail-guard`), read
> against a row the press hands the predictor (`key-row-sample`, `KeySample`, `predict_glyph`), and
> hosts observe through `Seen` (`seen-opacity`, `reconcile_seen`; `reconcile` is its wrapper).
> A guess retired without credit now owes its own glyph (`typed-owes`, §5.12). Running the sweep over
> forty seeds found that the guess after a Backspace that cancels our own guess anchored a cell off,
> fixed with its named case (`backspace-pops-own`, `predict-placement`, §5.13), and a run typed back
> into a wrapped line that re-proved (§5.14), whose rule (`tail-guard`: every guess of a run typed over
> nothing or its own glyph) is pinned by unit tests only: that counterexample came from an insert
> pushing the tail past the margin, which the harness then took out of scope, so the theorem carries no
> evidence of it. The theorem's scope was corrected in four places it claimed too much (a key whose own
> echo leaves the editor's model, a frame inside unprompted output, a frame on an unsettled base, a late
> echo that landed under a later guess of the same epoch).
>
> 2026-10-17, after review of phase 0: a far end that writes a mid-line glyph before the tail it shifts
> proved on the frame between the two, so a standing proof not at the end of the line is re-read against
> its run's snapshot at every reconcile and at the press that continues it (`standing-tail`, §5.15). A
> matching glyph is an echo only with the real cursor past it (`reconcile-contract`, `Rule::CursorAdvance`,
> §5.16): a run typed along an autosuggestion was credited before any echo and shrank the window the
> theorem's link model judges lateness by; the theorem gains ATTRIBUTION, which holds every credit to
> its own key's output. A run at the end of the line proves without a tail comparison, so an
> autosuggestion drawn, changed or cleared beside it no longer darkens the keys after it (`tail-guard`,
> `key-row-sample`: `tail_line_end`, `line_end_at`, `moved_tail_at`), and a head whose cell still holds
> what its press saw there, the cursor on it, waits instead of flushing (a glyph typed onto a suggestion
> it diverges from). A cancelled guess owes its glyph (`backspace-pops-own`, `Rule::CancelOwes`), a guess
> of the current epoch retired without credit voids the epoch's proof (`Rule::UncreditedVoids`, §5.17),
> an opaque blank is nothing to type over, and `wants_key_row` no longer asks the mode (a host that keeps
> none asks `mode()`). The theorem's TIMELINESS gains a control for its window half, and its scope two
> more corrections (a credit made while an earlier key's output is overdue, and one made while the line
> editor has lost its column); with them the faithful predictor holds on sixty seeds of a world that now
> also rewrites tails and draws suggestions.
>
> 2026-10-17, predictive editing increment 2 (edits that move no cell): `←`, `→` and a Backspace at the end
> of the text are armed by `predict_edit` as ops of a STATE CHAIN on the known line (`edit-key-routing`,
> `edit-fold`, `edit-line-lifetime`), reconciled by whole states at the smallest match, with a net-null tail
> owed and a part-way echo waited on (`edit-reconcile`), shown only once a motion of the epoch has echoed
> (`edit-class-proof`, the model's abstract edit class), painted all or none with its own cursor
> (`edit-overlay`), expiring whole (`edit-expiry`), every key in one custody (`edit-custody`). An arrow that
> cannot be placed is declined with its pixels kept. The theorem's world gains four line editors (readline,
> readline with ICH, zle, a cooked tty) and a cooked `cat`, edit keys routed through `predict_edit`, the
> PROVENANCE oracle, ATOMICITY and CURSOR LOCALITY; every new rule but four is caught in its clause (the four
> have no consequence in this world, §5.20). Eighty seeds then found a glyph-lane FIDELITY break the edit
> lane made reachable: a run typed before an erase we did not place had landed was credited by the next
> key's echo (`stale-anchor`, §5.18), and one more place the theorem's ATTRIBUTION scope claimed too much
> (§5.19). Its landing on main (aae75887e5) added two guards, each with a regression: the R-ROW guards
> compare `hi + 2 ≤ cols` with checked arithmetic, so no saturation at `u16::MAX` admits the last column
> (`predict-accept-decline`), and with an erase pending a chain flushes when the SENTINEL cell one past
> every state is not blank, before any state is matched (`edit-reconcile`).

Source: `crates/aterm-predict` (read only). As of 2026-10-17 `src/lib.rs` has 106 `#[test]` fns and
`tests/conformance_visibility.rs` has 6, for 112 in all (the 2026-10-15 recovery read 56 and 3). The
recovery read every test and the code it reaches; the rows added since were written with the code
they state, and the tests named in them ran.

Result: **33 specs** cover **112 tests**. The one test §4 found to sample no spec is deleted.

Gap numbers refer to the gap table in
the historical development audit `docs/AUDIT-trust-for-test-migration-2026-10-15.md` §7.
Gap 0 (an installed seal that publishes Clean rows) is a prerequisite for every spec, so the column
lists it on every row. No spec here is blocked by gap 0 alone.

## 0. Notation used in the statements

- Test ids are crate-relative: `src/lib.rs::name` and `tests/conformance_visibility.rs::name`.
- `P` is a `Predictor`. Its fields are as in `src/lib.rs`: `mode`, `preds` (a `VecDeque<Prediction>`
  with the oldest at the front), `srtt`, `expiry_backoff`, `slow_samples`, `fast_samples`,
  `adaptive_slow`, `confirmed_epoch`, `proof_cell`, `proof_echo`, `proof_line_end`, `epoch`,
  `grid_cols`, `deferred_wrap`, `anchor_lost`, `grandfathered_epoch`, `coords`, `owed_at`,
  `declined_tail`, `next_seq`, `cancelled_at`, `cancelled_pair`, `glyph_owes`, `tail`, `tail_cells`, `tail_line_end`,
  `line_end_at`, `moved_tail_at`, `line` (the known line: `row`, `lo`, `epoch`, `text`, `cursor`,
  `vacated`, with `hi = lo + |text|`), `chain`, `proven`, `carried_proven`, `editing_off`, `edit_capable`
  and `run_stale`. A guess `g` has `row`, `col`, `ch`, `born`, `epoch`, `owed`, `seq`, `op` (Insert,
  Left, Right or Erase), `class` (Glyph or Motion) and `stale`. `Engaged(P) ⇔ chain ∧ preds ≠ []` (a
  state chain is in flight; `fn engaged`).
- `Void(P)`: `confirmed_epoch := false`, `proof_cell := None`, `proof_echo := None`,
  `proof_line_end := false` (the private `fn void_proof`; the four move together), `proven := 0`
  (`Rule::ProofsEndWithTheEpoch`), and `line := None` unless `Engaged(P)` (edit-line-lifetime).
- `After(P,g) = (g.row, if g.col+1 ≥ grid_cols then g.col else g.col+1)`, the cell an echo of `g`
  leaves the cursor in (`fn cell_after`).
- `Past(P,cur,g)` (`fn echoed_past`): `cur = Some((r,c))` with `r = g.row ∧ c > g.col`; or
  `(r,c) = (g.row,g.col)` with `g` in the last column; or `r > g.row` with a guess of `preds` on row `r`,
  or `(r,c) = (g.row+1, 0)` with `g` or a guess of `preds` on `g.row` in the last column.
- `predict_char*` means both `predict_char` and `predict_char_in_grid`.
- `Reach(m)`: the predictors reachable from `Predictor::new(m)` by any sequence of public calls with
  non-decreasing `now` arguments. The public calls are `set_mode`, `predict_char*`,
  `predict_backspace`, `note_line_submit`, `note_unmodelled_input`, `reconcile` with any `observe`
  closure, `reconcile_seen`, `predict_glyph`, `predict_edit`, `set_editing`, `overlay`,
  `overlay_cursor`, `expire_due`, `reset`, `reset_session`, `take_link_estimate`, and
  `restore_link_estimate(e)` where `e` came from `take_link_estimate` or `LinkEstimate::default()`.
- `Mono(P)`: `g.born` does not decrease along `preds`. Every `P ∈ Reach` satisfies it, because `now`
  is non-decreasing.
- `Link(P) = (srtt, slow_samples, fast_samples, adaptive_slow, expiry_backoff, owed_at)`, the fields
  that `LinkEstimate` carries. `Screen(P)` is every other field.
- `Flush(P)`: `owe(max of g.born over preds and of the typed owes' born)` when either is non-empty,
  then `preds := []`, `glyph_owes := []`, `Void(P)`, `anchor_lost := false`,
  `grandfathered_epoch := None`, `declined_tail := 0`, `cancelled_at := None`, `cancelled_pair := None`, `line_end_at := None`,
  `chain := false` (so `Void` drops `line`), `carried_proven := 0`, with every other field kept. This
  is the private `fn flush`. The newest `born` counts every op, edit ops included (`Rule::EditOpsOwe`;
  off: only the Inserts).
- `EndEpoch(P)`: if `confirmed_epoch` then `grandfathered_epoch := Some(epoch)` and
  `carried_proven := proven`; then
  `epoch := epoch.wrapping_add(1)`, `Void(P)`, `anchor_lost := false`, `cancelled_at := None`,
  `cancelled_pair := None`, `line_end_at := None`. This is the private `fn end_epoch`.
- `CancelCursor(P,cur,now)`: when `preds=[]` and `cancelled_at=Some((r,c))`, keep the cell only if
  `cur=Some((r,c))`, or `c+1<grid_cols` and `cur=Some((r,c+1))`, or `c=0`, `grid_cols>0`, `r>0` and
  `cur=Some((r−1,grid_cols−1))`; otherwise `owe(now)` then `Flush(P)`. With guesses still pending or
  no cancelled cell it changes nothing. This is `observe_cancelled_cursor`; it rejects unknown or
  incompatible geometry, and does not prove attribution for unrelated output at a compatible cell.
- `W(P) = glitch_window(P)`. `Exp(P,now,g) ⇔ now.saturating_duration_since(g.born) > W(P)`.
- `owe(t)`: `owed_at := max(owed_at, t)`. Flush owes the newest `born` it drops; `note_line_submit(now)`,
  `note_unmodelled_input(now)`, a declined `predict_char*` and a `predict_backspace` that pops nothing
  owe `now` (one that pops our own guess owes no echo of any key, but the popped glyph's own TYPED,
  `Rule::CancelOwes`: backspace-pops-own); a guess retired without credit owes its `born`, typed when
  it was an append (`Appended`, tail-guard), else as any echo (typed-owes); an expired guess owes its
  `born` (expiry-contract).
  `reset_session` sets `owed_at := None`; `take_link_estimate` carries `max(owed_at, max g.born, max
  typed-owe born)` out and `restore_link_estimate(e)` installs `e`'s. Each armed guess takes
  `g.owed := owed_at` and `g.seq := next_seq`, then `next_seq += 1`.
  `Owed(P,now) ⇔ owed_at=Some(t) ∧ now.saturating_duration_since(t) ≤ W(P)` (the predictor's newest
  owe of any echo, which the model's `owed` projects; typed owes project to nothing). `Attr(P,now,h) ⇔
  ¬(h.owed=Some(t) ∧ now.saturating_duration_since(t) ≤ W(P)) ∧ ¬∃ (seq,ch,t) ∈ glyph_owes: seq < h.seq ∧
  ch = h.ch ∧ now.saturating_duration_since(t) ≤ W(P)` ∧ (h.stale ⇒ ¬Owed(P,now) ∧ ¬∃ pending Insert
  g' with g'.seq > h.seq ∧ g'.ch = h.ch)`, for a guess of any epoch: only an owe from before `h`, a
  typed one only for the same glyph, and for a STALE ANCHOR (stale-anchor) the keys after it too.
- `Vis(P) = visible_count(P)`: `Off → 0`; `Always → |preds|`; `Adaptive ∧ ¬adaptive_slow → 0`;
  otherwise the length of the leading run of `preds` with `op_visible` (edit-class-proof): of the current
  epoch with `confirmed_epoch`, or of `grandfathered_epoch`, and in either case with its class proven
  there. With every op of the glyph class this is the earlier `confirmed_epoch → |preds|`, else the
  carried run.
- `ms(d) = d.as_secs_f32() * 1000`.
- Turn sample: in one `reconcile` call, the latency `now − born` of the FIRST guess that call confirms.
- Epoch boundary `B` is any one of:
  - `note_line_submit` or `note_unmodelled_input`;
  - `predict_backspace` when the back guess is not of the current epoch;
  - a declined `predict_char*`;
  - a `coords` value that differs from `P.coords`, passed to `predict_char*` or `reconcile`;
  - a `predict_char*` with no current-epoch anchor and `cursor ≠ proof_cell`;
  - `reset`, `reset_session`, `take_link_estimate` or `restore_link_estimate`;
  - `set_mode` to a different mode;
  - `reconcile` with `alt_screen`;
  - a divergence or cursor-inconsistency flush inside `reconcile`.
- `Instant` is `aterm_time::Instant` (`std::time::Instant` on native) and `Duration` is std's.
  Every spec that reasons about time needs a model of `saturating_duration_since`,
  `Instant + Duration`, `Duration::from_millis` and `as_secs_f32`. `aterm_time` is a separate crate,
  so citing that model is gap 12.
- **Missing gap row (f32):** `glitch-window`, `rtt-only-from-echo`, `echo-regime-latch` and
  `reconcile-contract` (through `record_rtt`) need an `f32` model: casts, `min`, `clamp`, comparisons
  and the EWMA. The audit's §7 table has no row for floats. The column below writes it as `f32`
  next to the numbered gaps. It does not count in the histogram.

## 1. Specs (TSV)

Columns: `id`, `kind`, `functions`, `statement`, `subsumes` (primary tests; secondary ones are in §2),
`rust_features` (what the proof must handle), and `blocking_gaps` (audit §7 numbers).

```tsv
id	kind	functions	statement	subsumes	rust_features	blocking_gaps
mode-parse	contract	PredictMode::parse	∀ s:&str, let t = s.trim(): parse(s)=Adaptive ⇔ ∃ o∈{"adaptive","auto","on","true"}: t.eq_ignore_ascii_case(o); parse(s)=Always ⇔ ¬(previous) ∧ ∃ o∈{"always","force"}: t.eq_ignore_ascii_case(o); parse(s)=Off otherwise. So it fails safe: any string outside those six names gives Off.	src/lib.rs::mode_parse_is_fail_safe	&str, str::trim (Unicode whitespace), eq_ignore_ascii_case, Iterator::any with a closure over a &[&str], enum return	0,1,3,5,7,13
is-line-submit	contract	is_line_submit	∀ bytes:&[u8]: is_line_submit(bytes) ⇔ bytes is valid UTF-8 ∧ text=from_utf8(bytes) is non-empty ∧ last char of text ∈ {'\r','\n'} ∧ ∀ c ∈ text.chars(): c ∈ {'\r','\n'} ∨ ¬c.is_control(). Corollaries: empty input is false; any other C0/C1/DEL anywhere (BS, ^U, ESC, TAB, DEL) gives false; invalid UTF-8 gives false.	src/lib.rs::only_text_ending_in_a_terminator_is_a_line_submit	&[u8], std::str::from_utf8 (needs a UTF-8 validity model), ends_with with a char predicate, chars().all, char::is_control (Unicode Cc table), closures	0,1,3,5,7,13
off-mode-inert	contract	Predictor::{predict_char,predict_char_in_grid,predict_backspace,set_mode,overlay}	∀ P with mode=Off, ∀ args: predict_char* and predict_backspace return false and leave P unchanged; Vis(P)=0 so overlay(P,now)=[]; from a fresh P, preds stays empty in Off. set_mode(m): m≠P.mode ⇒ mode:=m then Flush; m=P.mode ⇒ P unchanged.	src/lib.rs::off_mode_predicts_nothing	&mut self, enum equality, early return, VecDeque clear	0,1,2,3
predict-placement	contract	Predictor::{predict_char_in_grid,predict_char}	predict_char(ch,cur,g,cols,now) = predict_char_in_grid(ch,cur,g,(cols,0),now). For mode≠Off and cols>0, first observe_coordinates(g), set grid_cols:=cols, and apply CancelCursor(P,Some(cur),now), including on an idle host that skipped reconcile. After that guard and the acceptance filter (predict-accept-decline): let A = the last g∈preds with g.epoch=P.epoch, and C = cancelled_at (taken by the call: backspace-pops-own). base = (A.row, A.col.saturating_add(1)) if A exists; else C if C is set; else (cur.row, cols) if deferred_wrap ∧ cur.col+1=cols; else cur. The proof check compares proof_cell with C when C anchors the guess, else with cur. base.col<cols ⇒ the guess goes at base; otherwise at (base.row+1, 0) when base.row+1<rows, and the call declines (false, preds unchanged) when base.row+1≥rows. On accept it pushes {row,col,ch,born:now,epoch:P.epoch} at the back, returns true, and sets grid_cols:=cols; when A does not exist (the guess anchors on the live cursor) it first takes the run's tail (key-row-sample). predict_glyph(ch, s, now) is the same call on s.cursor, s.coords and s.grid, carrying s.row. Consequences: type-ahead extends the previous guess and does not land on the live cursor; a height-blind host (rows=0) declines at the margin and keeps the armed guesses; a parked deferred-wrap cursor wraps the next glyph; a free last column is predicted in place.	src/lib.rs::types_ahead_extends_from_last_guess_before_echo, src/lib.rs::height_blind_hosts_decline_at_the_margin_without_flushing, src/lib.rs::wraps_to_the_next_row_instead_of_flushing, src/lib.rs::bottom_row_declines_without_flushing, src/lib.rs::the_deferred_wrap_slot_is_not_a_free_cell, src/lib.rs::a_free_last_column_is_still_predicted_in_place, src/lib.rs::a_deferred_wrap_on_the_last_row_declines_without_painting, src/lib.rs::a_cancelled_pair_does_not_anchor_after_an_unrelated_cursor_move	&mut self, VecDeque iter().rev().find(closure), push_back, saturating u16, match with guards on Option, tuple destructuring	0,1,2,3,5,7,13
predict-accept-decline	contract	Predictor::{predict_char_in_grid,decline,set_ambiguous_width_double}, aterm_grapheme::{char_width,char_width_cjk}	For mode≠Off and cols>0, after observe_coordinates and `if preds=[] then anchor_lost:=false`: the call declines ⇔ ch.is_control() ∨ w(ch)≠1 ∨ anchor_lost ∨ |preds|≥MAX_PREDICTIONS ∨ (preds≠[] ∧ no g∈preds has g.epoch=P.epoch, i.e. only a submitted line's guesses are in flight) ∨ the wrap reaches the bottom edge, w being char_width_cjk after set_ambiguous_width_double(true) (the width the grid writes East Asian Ambiguous glyphs with) and char_width otherwise; changing that flag flushes. Every decline owes `now`. A declined control char is Flush. A declined printable char leaves preds unchanged, applies EndEpoch, sets anchor_lost:=true, declined_tail:=declined_tail+1 (saturating) and returns false. An accepted guess is pushed with owed:=owed_at and seq:=next_seq (then next_seq+1), and sets declined_tail:=0. While anchor_lost ∧ preds≠[], every later predict_char* declines with preds unchanged; the first predict after preds drains is judged afresh. cols=0 ⇒ false, P unchanged. While Engaged a glyph is arm_in_chain's instead: armed as an Insert of the glyph class at the chain's cursor when Placeable with that cursor at the chain's text end, the glyph one cell wide and hi+2 ≤ cols (R-ROW, in ℕ: the code checks the sum, so no saturation at u16::MAX admits the last column); owed and Flushed when the chain's cursor is inside its text (a mid-line insert shifts a tail this lane does not move); otherwise declined as above. Not Engaged, a glyph typed with the known line's cursor inside its text Voids first, dropping the line (edit-line-lifetime). A run anchored on the live cursor while Owed carries `stale` (stale-anchor).	src/lib.rs::wide_glyphs_are_declined_but_narrow_accents_are_predicted, src/lib.rs::ambiguous_glyphs_are_measured_as_the_terminal_writes_them, src/lib.rs::changing_the_ambiguous_width_flushes_guesses_placed_under_the_old_one, src/lib.rs::a_declined_glyph_does_not_erase_the_word_to_its_left, src/lib.rs::a_declined_glyph_owes_its_echo, src/lib.rs::an_edit_chain_does_not_saturate_its_last_column_guard, src/lib.rs::nothing_is_armed_behind_a_terminator_while_its_line_is_in_flight	&mut self, char::is_control, cross-crate width table (aterm_grapheme::char_width), VecDeque len	0,1,2,3,4,5,12
queue-cap	invariant	Predictor::predict_char_in_grid; every public &mut method	∀ m, ∀ P∈Reach(m): |P.preds| ≤ MAX_PREDICTIONS (256). In particular predict_char* with |preds|=256 returns false and keeps |preds|=256 whatever epochs the guesses carry. Only predict_char_in_grid (with arm_in_chain, its branch while a chain is in flight) and predict_edit grow preds, by at most 1 and only when |preds|<256; a chain is capped further at EDIT_CHAIN_MAX = 64 ops (R-CAP, edit-key-routing).	src/lib.rs::pending_queue_caps_at_max_predictions	&mut self across every public method, VecDeque length; reconcile/expire have while loops that only pop	0,1,2,5,7
pending-bounds-hull	contract	Predictor::pending_bounds	pending_bounds(P)=None ⇔ preds=[]; otherwise Some((min g.row, min g.col, max g.row+1, max g.col+1)) over g∈preds, computed in usize so nothing saturates. While Engaged the hull also covers line.row from line.lo to one past the furthest text end any state reaches (edit-overlay). It is &self: P, next_deadline and |preds| are unchanged. After reset (or any Flush) it is None.	src/lib.rs::pending_bounds_reserves_wrapped_guesses_without_consuming_them	iterator map + reduce over a VecDeque, usize::from(u16), min/max, Option of a 4-tuple	0,1,3,5,7,13
expiry-contract	contract	Predictor::{expire,expire_due,overlay,next_deadline,is_displaying}	∀ P, now: expire pops the maximal prefix of preds satisfying Exp(P,now,·), with W evaluated once before the loop. For each popped g, in order: owe(g.born); h.owed := max(h.owed, Some(g.born)) for every h left in preds; and if g.epoch = P.epoch, Void (`Rule::ExpiryOwes`: the expired key's late echo lands on the cell the proof names, ahead of anything typed next); and line_end_at := None whatever the rule (its echo, late or never, moves the cursor where no guess says). If it pops ≥1, expiry_backoff' = min(expiry_backoff+1, 3); every other field is untouched (epoch, grandfathered_epoch, srtt, slow/fast_samples, adaptive_slow). With `Rule::ExpiryOwes` off it pops and owes and voids nothing (the pre-2026-10-17 contract). This is the glyph lane; an Engaged chain expires whole (edit-expiry). expire_due(now) = (expire popped ≥1). overlay(now) = expire, then preds[..Vis]. next_deadline(P) = preds.front().map(|g| g.born + W(P)). Under Mono, after expire(now): next_deadline is None or ≥ now (not > now; see §5.1), and a second expire_due(now) is false. is_displaying(P,now) ⇔ preds≠[] ∧ (Exp(P,now,front) ∨ Vis(P)>0), and it is &self. Corollaries: an Adaptive guess hidden by the gate expires with overlay=[] before and after the deadline; after a current-epoch guess expires, the guesses behind it and the next guess at the live cursor are dark until an echo proves the epoch again.	src/lib.rs::expire_due_consumes_the_deadline_without_a_redraw, src/lib.rs::expired_prediction_clears_next_deadline, src/lib.rs::expiry_retires_only_the_oldest_guess_owes_it_and_voids_the_epoch, src/lib.rs::without_expiry_owes_an_expiry_keeps_the_proof_as_it_did, src/lib.rs::adaptive_local_tail_expires_without_a_visible_erase	&mut self, while-let popping a VecDeque front, Instant::saturating_duration_since and Instant+Duration (cross-crate opaque type), Duration compare, saturating u8, make_contiguous + slice get returning a borrow tied to &mut self	0,1,2,3,7,12
glitch-window	contract	Predictor::glitch_window	Let f = min((250 << min(expiry_backoff,3)) as f32, 2000.0). W(P) = Duration::from_millis(x as u64), with x=f when srtt=None and x=clamp(3·s, f, 2000) when srtt=Some(s). f ∈ [250,2000], so the clamp never panics (min ≤ max). ∀ P∈Reach: expiry_backoff ≤ 3 ∧ 250 ms ≤ W(P) ≤ 2000 ms. With srtt fixed, W is non-decreasing in expiry_backoff; with srtt=None it is 250·2^backoff ms.	src/lib.rs::slow_link_expiry_window_grows_until_the_echo_can_confirm	&self, Option<f32>, u64 shift by a u8, u64→f32 and f32→u64 casts, f32 min/clamp (panic precondition), Option::map_or with a closure, Duration::from_millis	0,1,2,3,6,12,13,f32
rtt-only-from-echo	invariant	Predictor::{record_rtt,reconcile,expire,record_echo_regime}	srtt is written only by record_rtt, called only when reconcile confirms a guess: srtt' = ms(d) if srtt=None else 0.7·s + 0.3·ms(d), and expiry_backoff' = 0. slow_samples, fast_samples and adaptive_slow are written only by record_echo_regime (once per reconcile turn that confirms ≥1 guess) and by reset_session / take_link_estimate / restore_link_estimate. Hence ∀ trace from Predictor::new(m) with no confirming reconcile: srtt=None, adaptive_slow=false, confirmed_epoch=false and, in Adaptive, Vis=0. A link that never echoes is never painted, and timeouts never feed the estimate.	src/lib.rs::silence_never_opens_the_gate_or_diverges_the_estimate	trace-level frame condition over every &mut method, f32 EWMA, Duration::as_secs_f32	0,1,2,5,7,f32
echo-regime-latch	contract	Predictor::{record_echo_regime,reconcile}	Each reconcile that confirms ≥1 guess calls record_echo_regime exactly once, on the turn sample (the first confirmed guess's latency, not the newest). record_echo_regime(d), x=ms(d): x ≥ 20 ⇒ fast:=0, slow:=min(slow+1,2), and adaptive_slow:=true if slow≥2. x < 8 ⇒ slow:=0, fast:=min(fast+1,3), and adaptive_slow:=false if fast≥3 ∧ (srtt=None ∨ srtt<8) ∧ preds=[], else unchanged. 8 ≤ x < 20 ⇒ no change. Consequences ∀ P∈Reach: adaptive_slow goes false→true only on the second consecutive decisive slow turn, and true→false only on a turn with ≥3 consecutive decisive fast turns, smoothed srtt<8 ms and nothing pending; a dead-band sample resets neither streak; a burst retired in one turn counts as one sample.	src/lib.rs::sustained_slow_echo_retires_predictions_and_displays_next, src/lib.rs::adaptive_isolated_scheduler_tail_does_not_open_gate, src/lib.rs::adaptive_slow_to_fast_closes_only_after_a_sustained_fast_streak, src/lib.rs::adaptive_gate_does_not_flap_across_a_single_fast_sample, src/lib.rs::adaptive_gate_never_closes_while_a_guess_is_in_flight, src/lib.rs::burst_is_classified_by_the_oldest_retired_guess	&mut self, f32 compare and EWMA, saturating u8 with min, Option::get_or_insert in the reconcile loop, Option::is_none_or closure	0,1,2,3,5,7,13,f32
visibility-gate	contract	Predictor::{overlay,visible_count}	∀ P, now: overlay(P,now) = (expire(P,now); P.preds[..Vis(P)]). Vis(P) ≤ |preds| always, so the get(..) never takes its unwrap_or branch. The result is a leading run of preds in type order. Always mode shows every pending guess at once, with no confirmation needed. While Engaged, overlay returns the chain's ghosts instead (edit-overlay).	-	enum match with guards, take_while + count with a closure, VecDeque::make_contiguous, slice get with a range, a returned borrow	0,1,2,3,7,13
reconcile-contract	contract	Predictor::{reconcile,reconcile_seen,observe_coordinates,observe_cancelled_cursor,confirm,retire_echoed,echoed_past}	reconcile(P,cur,g,alt,now,obs) = reconcile_seen(P,cur,g,alt,now,obs') with obs'(r,c) = obs(r,c).map_or(Blank, |ch| Seen::of_cell(ch,false)) (seen-opacity); reconcile also sets edit_capable:=false and first Flushes an Engaged chain (a host that cannot say which cells are opaque runs none), reconcile_seen sets edit_capable:=true; while Engaged, steps (3) and (4) are edit-reconcile's instead; below, obs(r,c) is obs'(r,c).legacy(). ∀ closures obs. (1) P.coords=Some(c) ∧ c≠g ⇒ P:=reset(P); then coords:=Some(g). (2) alt ⇒ Flush, deferred_wrap:=false, return. (3) Repeatedly, h=front: obs(h.row,h.col)=Some(h.ch) ∧ Past(P,cur,h) (`Rule::CursorAdvance`; off: the glyph alone), or h.ch=' ' ∧ obs(h.row,h.col)=None ∧ cur=Some((h.row,c')) with c'>h.col ⇒ pop; if Attr(P,now,h) then record_rtt(now−h.born) and, iff h.epoch=P.epoch: when Appended(P,h) (tail-guard), line_end_at:=Some(After(h)) if tail_line_end, then confirm(h); otherwise moved_tail_at:=max(moved_tail_at, h.born) and Void. If not Attr: Void when h.epoch=P.epoch (`Rule::UncreditedVoids`); when Appended(P,h), line_end_at:=Some(After(h)) if tail_line_end ∧ h.epoch=P.epoch, else moved_tail_at:=max(moved_tail_at, h.born); and h is owed typed or as any echo (typed-owes: glyph_owes.push((h.seq, h.ch, h.born)) when Appended, else owe(h.born) and g.owed:=max(g.owed,Some(h.born)) for every g left in preds). obs(h.row,h.col)=Some(h.ch) ∧ ¬Past ⇒ stop (held: the glyph was there before its echo, an autosuggestion the typed glyph matches); obs(h.row,h.col)=Some(c≠h.ch) with Some(c) = T?(h.row,h.col).legacy() (tail-guard) ∧ cur=Some((h.row,h.col)) ⇒ stop (the cell as the run's press saw it, not yet echoed: a glyph typed onto a suggestion it diverges from); any other Some(c≠h.ch) ⇒ Flush and stop; otherwise stop. (3b) The standing tail (standing-tail). (4) preds≠[] ∧ cur is neither Some((front.row,front.col)) nor the deferred-wrap slot of a wrapped front (front.col=0 ∧ grid_cols>0 ∧ cur=Some((front.row−1, grid_cols−1))) ⇒ Flush. (5) CancelCursor(P,cur,now), so an empty queue with an incompatible or unknown cursor owes possible late pair output and loses its retained anchor/proof. (6) deferred_wrap := cur=Some((r,c)) ∧ grid_cols>0 ∧ c+1=grid_cols ∧ (obs(r,c).is_some() ∨ (c>0 ∧ obs(r,c−1)=Some(l) ∧ w(l)=2)), w the width of predict-accept-decline: a wide glyph whose lead fills column c−1 parks the cursor on its continuation, which obs reads as blank. confirm(h) sets confirmed_epoch:=true, proof_cell:=Some(After(h)), proof_echo:=Some(h) and proof_line_end:=tail_line_end iff h.epoch=P.epoch, else does nothing. Corollaries: a wrong glyph or the alternate screen empties preds; an echoed space confirms by cursor advance; a glyph already on screen before its key's echo (a matched suggestion) neither retires nor measures until the echo moves the cursor past it, and a cursor moved down by other output (a submit's) is not past a guess whose run does not reach that row; a scroll (changed g) retires every guess before any glyph is compared, so a moved cell never confirms; a cursor parked one row off is not a deferred-wrap slot; a match an earlier key's owed echo may have made neither proves nor measures, and leaves the matched guess's own echo owed: as its glyph to a later guess for the same glyph, or, over a moved tail, to every guess behind it.	src/lib.rs::divergence_flushes_all, src/lib.rs::alt_screen_flushes_predictions, src/lib.rs::space_confirms_by_cursor_advance, src/lib.rs::wrapped_guess_survives_the_deferred_wrap_cursor, src/lib.rs::a_wrap_the_terminal_did_not_defer_keeps_the_wrapped_guess, src/lib.rs::a_scrolled_cursor_does_not_pass_for_the_deferred_wrap_slot, src/lib.rs::a_wide_glyph_filling_the_last_two_columns_parks_the_cursor, src/lib.rs::a_scroll_retires_guesses_instead_of_confirming_them_against_moved_cells, src/lib.rs::a_guess_retired_without_credit_owes_its_own_echo, src/lib.rs::an_owed_echo_neither_proves_nor_measures_a_carried_guess, src/lib.rs::a_glyph_already_on_screen_is_not_its_echo, src/lib.rs::a_cancelled_pair_does_not_anchor_after_an_unrelated_cursor_move, src/lib.rs::an_echo_owed_to_a_flushed_guess_cannot_prove_a_later_one	generic `impl FnMut(u16,u16)->Option<char>` called in a loop (higher-order spec; obs arbitrary but deterministic per call), while-let over a VecDeque, match with guards, Instant arithmetic, f32 via record_rtt, &mut self	0,1,2,3,4,5,7,11,13,f32
submit-carries-proven-guesses	contract	Predictor::{note_line_submit,end_epoch,visible_count}	note_line_submit(P,now) = owe(now), EndEpoch(P), declined_tail:=0, and preds is unchanged, so next_deadline is unchanged. In Always the overlay is unchanged across the call. In Adaptive with adaptive_slow ∧ confirmed_epoch before the call, the ended epoch's guesses (a leading run of preds) stay in Vis, and every guess armed afterwards (epoch ≠ grandfathered) is outside Vis until a guess of the new epoch confirms. A second note_line_submit with no confirmation between keeps grandfathered_epoch (confirmed_epoch is false), so the carry survives any number of terminators. EndEpoch keeps carried_proven:=proven, so a carried chain displays as it did, with its cursor (edit-overlay), and its line stays its base while it is in flight (edit-line-lifetime).	src/lib.rs::submit_does_not_blank_the_tail_of_the_command_line, src/lib.rs::every_untracked_press_owes_its_echo, src/lib.rs::an_owe_is_dated_by_its_press	&mut self, u64 wrapping_add, Option assignment	0,1,2
unmodelled-input-retires	contract	Predictor::{note_unmodelled_input,predict_backspace,decline}	∀ P with mode≠Off: note_unmodelled_input(P,now) = owe(now) then Flush(P); hosts call it for a glyph or Backspace refused at their no-echo gate too. predict_backspace(P) with preds=[] or back.epoch ≠ P.epoch owes `now` and returns false, and is Flush(P) unless declined_tail>0 (backspace-pops-own). predict_char* of a control char is Flush(P) and returns false. Afterwards Vis=0 and overlay=[] in Adaptive and in Always, so neither a guess carried across a submit nor typed-ahead text a backward erase removes leaves a ghost. An edit key the lane arms is not unmodelled input (edit-key-routing), and an arrow it cannot place is DECLINED WITH ITS PIXELS KEPT (owe(now), EndEpoch, anchor_lost:=true, preds unchanged), not Flushed: the far end reads it after every key in flight, and no motion erases text. Until reconcile confirms a new guess, the next predict_char starts unproven (confirmed_epoch=false).	src/lib.rs::a_key_we_did_not_model_retires_the_guesses_in_flight, src/lib.rs::an_erase_after_a_boundary_darkens_the_carried_guesses, src/lib.rs::an_erase_landing_on_typed_ahead_text_leaves_no_ghost, src/lib.rs::an_erase_we_did_not_model_leaves_no_ghost_where_nothing_echoes, src/lib.rs::every_untracked_press_owes_its_echo, src/lib.rs::an_erase_we_did_not_cancel_owes_its_echo	&mut self, VecDeque clear/back, Option::is_some_and closure	0,1,2,3,13
backspace-pops-own	contract	Predictor::predict_backspace	∀ P with mode≠Off: preds.back()=Some(g) ∧ g.epoch=P.epoch ∧ g.op=Insert (only a typed glyph pops; predict_edit routes a Backspace here first) ⇒ predict_backspace pops exactly that element, sets cancelled_at := Some((g.row, g.col)) (`Rule::CancelTransit`; off: unchanged, the pre-2026-10-17 contract), owes the popped glyph TYPED, glyph_owes.push((g.seq, g.ch, g.born)) (`Rule::CancelOwes`; off: nothing, the pre-review contract), sets cancelled_pair := Some((g.row, g.col, g.ch, next_seq)) (tail-guard), returns true and changes no other field: the pair owes no echo of any key (the erase's echo puts no glyph in a guess's cell), but the cancelled glyph's echo still lands, on the very cell the next guess anchors on, where a guess for the same glyph was credited with it. Before a later glyph can use the retained cell, CancelCursor applies at glyph entry and after pending reconciliation: with no guesses left, only the cancelled cell, its within-row next cell, or previous-row last-column parking before a wrapped col0 remain compatible. An incompatible or unknown cursor instead owes now then Flushes; a later glyph anchors on its supplied live cursor and Adaptive starts without the old proof. A glyph that reaches placement takes cancelled_at whatever it anchors on, and anchors on that cell when no current-epoch guess is pending (predict-placement): the cancelled glyph's echo lands before the erase's, the screen a cell ahead, or parked in the deferred-wrap slot at the margin, between them, and the cursor returns to that cell once the erase lands. This is a geometric refusal, not attribution against arbitrary output at the same or next cell. Flush and EndEpoch clear cancelled_at. Otherwise, with declined_tail>0, it owes `now`, sets declined_tail:=declined_tail−1, keeps preds and returns false: it erases the newest declined glyph, typed after every guess in flight. Otherwise see unmodelled-input-retires. Off ⇒ false, P unchanged.	src/lib.rs::backspace_cancels_pending_prediction, src/lib.rs::a_backspace_cancels_the_glyph_we_declined, src/lib.rs::a_cancelled_glyph_anchors_the_next_on_its_cell, src/lib.rs::a_cancelled_glyph_at_the_margin_does_not_wrap_the_next, src/lib.rs::a_cancelled_glyph_s_echo_is_owed_to_the_same_glyph_typed_next, src/lib.rs::a_cancelled_pair_does_not_anchor_after_an_unrelated_cursor_move	&mut self, VecDeque back/pop_back, Option::is_some_and closure	0,1,2,3,13
no-unechoed-flash	invariant	every public Predictor method; Predictor::{confirm,end_epoch,flush,observe_coordinates,predict_char_in_grid,visible_count}	∀ P∈Reach(Adaptive): (a) confirmed_epoch ⇔ proof_cell.is_some() ⇔ proof_echo.is_some(), and proof_line_end ⇒ confirmed_epoch. (b) confirmed_epoch ⇒ since the last boundary B a reconcile confirmed a guess h = proof_echo with h.epoch=P.epoch, Past(P,cur,h), Attr(P,now,h) and Appended(P,h) under coords=P.coords; since then no echo of the epoch retired other than as a credited append, none retired without credit (`Rule::UncreditedVoids`), no guess of it expired, and, unless proof_line_end, every reconcile and every press continuing the proof found TailClean(P,h) (standing-tail); proof_cell = After(h). (c) grandfathered_epoch=Some(e) ⇒ e≠P.epoch, and epoch e had confirmed_epoch=true when note_line_submit ended it. (d) every op displayed (a leading run of Vis ops, the glyph lane's guesses or a chain's ops behind its ghosts) has op_visible (edit-class-proof): (g.epoch=P.epoch ∧ confirmed_epoch) ∨ Some(g.epoch)=grandfathered_epoch, and its class proven there. Corollary: no motion displays in an epoch whose glyph proof is absent, nor before a motion of that epoch echoed exactly; an edit key is armed only while the proof stands (edit-key-routing). Corollary (the password guarantee): ∀ B, ∀ predict_char* after B with no confirming reconcile of a post-B guess in between, the new guess is not in overlay. That covers a new row, a prompt printed on the same row (cursor ≠ proof_cell), a scroll seen at the key seam or at reconcile, a line terminator, a stale pre-submit guess confirming after it (confirm ignores h.epoch≠epoch), a declined glyph, an unpopped Backspace, unmodelled input and an estimate restore.	src/lib.rs::fresh_line_resets_confirmation_no_leak, src/lib.rs::a_stale_guess_confirming_after_submit_cannot_arm_the_new_line, src/lib.rs::a_scroll_ends_the_epoch_on_the_same_physical_row, src/lib.rs::a_line_terminator_makes_what_follows_tentative, src/lib.rs::the_submitted_lines_echo_cannot_prove_the_next_line	trace invariant over every &mut method (history ghost state for clause b), per-element quantification over a VecDeque, reconcile's obs closure, u64 wrapping epochs (needs no wrap-around within one carry, or a statement by epoch inequality), Option<u64>	0,1,2,4,5,7,13
link-estimate-swap	round-trip	Predictor::{reset,reset_session,take_link_estimate,restore_link_estimate}, LinkEstimate::default	reset(P) = Flush(P) with coords:=None and deferred_wrap:=false, keeping Link(P). reset_session(P) = reset(P) with Link := LinkEstimate::default() (srtt=None, 0, 0, false, backoff 0, owed_at=None). take_link_estimate(P) returns E=Link(P), its owed_at raised to the newest born in preds (the guesses its reset flushes) and in the typed owes (folded into an owe of any echo: the estimate carries no glyphs), and leaves reset_session(P). restore_link_estimate(P,e) = reset(P) with Link:=e. Hence restore(take(P)) has the original Link(P) and Screen = reset; restore(default) equals reset_session; after restore, preds=[] (overlay=[], is_displaying=false, pending_bounds=None) and confirmed_epoch=false whatever P was. After reset_session, W = 250 ms.	src/lib.rs::session_switch_forgets_slow_link_rtt, src/lib.rs::a_backgrounded_session_keeps_its_link_estimate, src/lib.rs::restoring_an_estimate_clears_the_outgoing_panes_pixels, src/lib.rs::a_default_estimate_is_the_no_estimate_path, src/lib.rs::the_cold_link_probe_travels_with_the_session_estimate, src/lib.rs::session_reset_restarts_the_cold_link_probe, src/lib.rs::a_returning_sessions_old_echo_cannot_prove_its_new_guess, src/lib.rs::typed_owes_fold_into_any_echo_past_the_cap_and_at_a_take	&mut self, a Copy struct with Option<f32> and private fields, struct construction, call composition (take → reset_session → reset → flush)	0,1,2,4
visibility-model-refinement	refinement	Predictor::{predict_char,reconcile,overlay,note_line_submit,note_unmodelled_input,predict_backspace,reset_session}, aterm_models_core::predictive_echo_visibility_model	There is a projection π(P) → PredictiveEchoVisibility state: pending = preds≠[], visible = Vis>0, confirmed = confirmed_epoch, slow = adaptive_slow, fast_streak = fast_samples, fresh = (Link = default), retracted = this step closed adaptive_slow, owed = Owed(P,now). ∀ P∈Reach(Adaptive), each Predictor step maps to a model action or a stutter: a drained turn with a sample <8 ms → ConfirmFast; the same with preds≠[] → ConfirmFastInFlight; the turn that sets adaptive_slow → ConfirmSlow; predict_char → Key; expiry → Expire (which owes and clears confirmed; the code's owe is dated by the expired press, already past its window, so OwedExpire follows as the next step); confirming the last pending guess → Echo; every boundary B → Submit (which owes, over-approximating the one boundary that owes nothing: the program moving the cursor); the owed window passing → OwedExpire; a match retired while Owed → Echo; reset_session → SwitchSession. Typed owes (typed-owes, and the cancelled glyph's, backspace-pops-own) project to owed = 0: the model has no glyph identity, so the theorem's directed case `a_guess_retired_without_credit_owes_its_own_glyph` and `Rule::TypedOwesSameGlyph` pin them instead. A match retired WITHOUT credit maps to Echo, and when it was refused by an owe of any echo (owed = 1) ProofIsAttributable already has confirmed = 0; when a typed owe refused it (owed = 0) the code also clears confirmed (`Rule::UncreditedVoids`), which the model's Echo does not: that step refines Echo followed by confirmed := 0, a strengthening no model invariant can be broken by (each holds with confirmed = 0 and visible = 0), and the position fact it keeps (no proof on a cell the retired key's echo is still to take) is below the model's abstraction, held by the theorem's FIDELITY clause at its named case `an_uncredited_retirement_leaves_no_proof`. The standing tail's void, a tail refusal and the line-end bookkeeping are likewise below it (they only lower confirmed). Each model invariant (VisibleNeedsProofAndBenefit, RetractOnlyOnSustainedFastEvidence, FreshSessionHasNoInheritedRtt, ProofIsAttributable) then holds of π(P); ProofIsAttributable only while the window has not grown since the owing step (a larger srtt can reopen an Owed the model has already expired). The model's own negative controls (hand-built bad states rejected by check_invariant) are Tier-0 model checks. Needs a Submit with a carried-pending branch first (§5.5). THE EDIT CLASS: π also maps class = (the front op is a motion), edit_proven = Motion ∈ proven. predict_edit arming a motion → KeyEdit; its credited echo with the glyph proof standing → ConfirmEdit; an arrow declined (decline-motion) → Submit; an erase at the end is a glyph-class op (Key, Echo). EditVisibleNeedsClassProof and ClassProofRidesOnGlyphProof hold of π(P) (edit-class-proof: Void clears proven with the glyph proof).	tests/conformance_visibility.rs::adaptive_fast_expiry_is_invisible_and_slow_echo_may_display, tests/conformance_visibility.rs::the_display_gate_closes_only_on_sustained_fast_evidence, tests/conformance_visibility.rs::every_epoch_boundary_is_the_models_submit, tests/conformance_visibility.rs::an_owed_echo_proves_nothing_until_its_window_passes, tests/conformance_visibility.rs::expiry_owes_and_voids_the_proof, tests/conformance_visibility.rs::an_edit_displays_only_after_its_class_is_proven	model→code refinement over a ty_model! (non-scalar projection, stuttering simulation, ConfirmSlow abstracting two concrete turns), closures, &mut self	0,1,2,5,10,13
tail-guard	contract	Predictor::{retire_echoed,appended,tail_clean,tail_at,tail_decorated,take_tail,ended_line_at,moved_tail}	Appended(P,h), for a guess h of the run in flight just popped by reconcile_seen with observe obs and real cursor cur: tail_line_end ∨ TailClean(P,h). TailClean(P,h): Over(T?(h.row,h.col), h.ch) ∧ ∀ c ∈ [h.col+1, grid_cols): if a pending g has (g.row,g.col)=(h.row,c) then Over(T?(h.row,c), g.ch) ∧ (obs(h.row,c) = T(h.row,c) ∨ (obs(h.row,c).legacy() = Seen::of_cell(g.ch,false).legacy() ∧ Past(P,cur,g))), else obs(h.row,c) = T(h.row,c); with `Rule::CursorAdvance` off the Past conjunct is dropped; and a cell at the position of cancelled_pair = Some((r,c,ch,_)) may also hold ch (our own cancelled pair writing and erasing it; cancelled_pair is set by every Backspace that pops our guess, to its cell, glyph and next_seq, and cleared by Flush, EndEpoch and the retirement of any guess with seq ≥ that next_seq). Here T?(r,c) = Some(tail_cells[c−from]) when tail=Some((r,from)) ∧ from ≤ c (Blank past its end) and None otherwise, T = T? or Blank, and Over(s, ch) ⇔ s = None ∨ s.legacy() ∈ {None, Some(ch)}: every guess of the run was typed over nothing (a blank, opaque or not: the blanks of a BiDi-reordered row are Opaque(None)) or that very glyph (a guess typed over other text is mid-line whatever the row shows once the tail has left it). Compared whole otherwise: an Opaque cell equals only the same Opaque. tail_line_end (the run is at the END OF THE LINE) is set by the run's first press (key-row-sample): when it continues a proof, := proof_line_end; else when its anchor = line_end_at, := true; else := the press carried a row ∧ every snapshot cell's legacy() is None ∧ ¬(moved_tail_at = Some(t) ∧ now − t ≤ W(P)). line_end_at is set to After(h) by every append retired in a current-epoch run with tail_line_end (credited or not) and cleared by Flush, EndEpoch and any expiry; moved_tail_at := max(moved_tail_at, h.born) for every retirement that is not Appended and every standing void (standing-tail). Within one reconcile call a row found clean stays clean for every later guess on it (its tail is a suffix of the cells compared, against the same T, with the same pending guesses exempt under the same cur, and its own cell was compared as a pending guess's), so a burst costs one row scan: ≤ grid_cols observe calls. An attributable echo of the current epoch proves (confirm) iff Appended or `Rule::TailGuard` is off; otherwise it still records its RTT sample and VOIDS the epoch's proof (Void), since the guesses behind it were placed as appends on a line that is not appending. Corollaries: a mid-line insert (the echo shifts the text right of it) proves nothing, so the next glyph is not painted over the tail that moved, even once a burst has pushed that tail off the row (onto the next row of a wrapped line), and, with standing-tail, when the far end writes the glyph before the tail; a right prompt or a matched suggestion the echo leaves in place still proves; a run at the end of the line proves whatever the far end draws beside it (an autosuggestion drawn, changed or cleared; a glyph typed onto one), which the comparison alone refused; a blank row read within a window of a moved tail, or by a press continuing a proof not at the end of the line, is no evidence of the end (the tail it hides may still be landing); a run started without a row proves only over a blank tail unless it inherits the end of the line; an overwrite of other text proves nothing either (fail closed). One row is all it sees: a wrapped line whose first row ends in spaces reads blank from a cursor on one of them.	src/lib.rs::a_mid_line_echo_proves_nothing, src/lib.rs::a_right_prompt_the_echo_leaves_in_place_still_proves, src/lib.rs::a_dirty_tail_voids_a_standing_proof, src/lib.rs::a_retired_burst_reads_its_tail_once, src/lib.rs::a_run_typed_over_other_text_never_proves, src/lib.rs::a_suggestion_beside_the_end_of_the_line_darkens_no_key, src/lib.rs::a_right_to_left_line_proves_over_its_opaque_blanks	&mut self, a Vec<Seen> snapshot indexed by u16 arithmetic, generic FnMut observe passed by &mut, a per-call memo, Option<Instant> windows	0,1,2,3,5,7,11,12,13
standing-tail	contract	Predictor::{reconcile_seen,arm_glyph,tail_clean,moved_tail}	THE STANDING TAIL (`Rule::StandingTail`, with `Rule::TailGuard`): a proof not at the end of the line holds only while TailClean(P,proof_echo) holds of the row right of the echo it rests on. (a) At the end of every reconcile_seen turn whose retirements left proof_echo unchanged, with confirmed_epoch ∧ ¬proof_line_end: ¬TailClean(P, proof_echo) under that turn's obs and cur ⇒ moved_tail_at:=max(moved_tail_at, proof_echo.born) and Void. (b) At a predict_glyph that anchors on the live cursor (no current-epoch guess pending, no cancelled cell) with s.row=Some(row) while confirmed_epoch ∧ ¬proof_line_end, BEFORE it takes the new snapshot: ¬TailClean(P, proof_echo) with obs(r,c) = row[c] on the cursor's row (Blank elsewhere) and cur = s.cursor ⇒ the same. Why: comparing only as guesses retire, an echo that writes its glyph FIRST and redraws the tail it shifts after (`e`, then ` cd` and the cursor stepped back) proved on the frame between the two, and the next glyph, anchored on that proof, was painted over the shifted space (`abeicd` on glass, `abei cd` settled). Re-read at the frame after the redraw the tail has moved; a press that comes first compares its own row, since the snapshot it takes next would be of the moved row; and a pending guess typed on the proof is held to its snapshot cell (typed over the tail glyph it would push, Over fails), so it is never painted over the moving tail. A glyph in a pending guess's cell is that guess's echo only with the cursor past it (TailClean's Past conjunct): `o` typed over the last `o` of `abo` by such a far end, `o` and `z` typed on the proof, and the displaced `o` in the second `o`'s cell, the cursor on it, kept the proof, and `z` was painted where the displaced `o` settles. Cost: one row scan per reconcile while a decorated proof stands, none at the end of the line.	src/lib.rs::a_glyph_echoed_before_its_tail_moves_proves_nothing_after, src/lib.rs::a_shifted_glyph_is_not_a_pending_guess_s_echo	&mut self, generic FnMut observe, a press-row closure over Option<&[Seen]>, if-let chains	0,1,2,3,5,7,11,13
seen-opacity	contract	Seen::{of_cell,legacy}, Predictor::{reconcile,reconcile_seen}	Seen::of_cell(ch, opaque) = Opaque(l) if opaque else (Glyph(ch) if ch≠' ' else Blank), with l = (ch≠' ').then_some(ch); legacy(Blank)=None, legacy(Glyph(c))=Some(c), legacy(Opaque(l))=l. The glyph lane (retirement, divergence, the space-by-cursor-advance rule, the cursor check and the deferred-wrap latch) reads every answer through legacy(), so reconcile_seen with an obs whose cells are Opaque(x) retires, flushes and latches exactly as with Glyph/Blank cells carrying the same x; only TailClean compares whole answers. reconcile is reconcile_seen with no Opaque cell.	src/lib.rs::an_opaque_cell_is_its_glyph_to_the_glyph_lane	enum match, Option::then_some, closure adapters	0,1,3,5,13
key-row-sample	contract	Predictor::{wants_key_row,mode,predict_glyph,take_tail}, KeySample	wants_key_row(P) ⇔ preds=[]: it does not ask the mode, because a host asks before its press applies the mode it predicts in (a predictor still Off from a window that never predicted would otherwise answer no to the press that starts its first run), so the mode is the host's to ask first (mode() = P.mode, for a host that keeps none). predict_glyph(ch,s,now): when the guess is accepted and anchors on the live cursor (no current-epoch guess pending) at s.cursor itself, tail := Some((s.cursor.row, s.cursor.col)) and tail_cells := [s.row[c] (Blank past its end) | c ∈ [s.cursor.col, cols)], the cursor's own cell included, refilled in place; when it anchors on the live cursor but s.row=None or the guess wrapped to the next row, tail := None. The same press sets tail_line_end (tail-guard): proof_line_end when it continues a proof (after the standing-tail press check), true when its anchor = line_end_at, else (a snapshot was taken ∧ all its cells' legacy() are None ∧ no moved tail within W of now). A type-ahead guess leaves tail and tail_line_end as they were. So the snapshot is the row from the cursor at the press of the run's first guess, read under the hold that read the cursor, and a host that reads the row only when wants_key_row() loses nothing but proof over a non-blank tail when rows moved between that call and the press.	src/lib.rs::a_mid_line_echo_proves_nothing, src/lib.rs::a_right_prompt_the_echo_leaves_in_place_still_proves, src/lib.rs::a_predictor_still_off_wants_the_row_of_the_press_that_starts_it	&mut self, Option<&[Seen]> borrow, Vec refill with extend over a u16 range, Option<bool> inheritance	0,1,2,3,5,7,12
typed-owes	contract	Predictor::{retire_echoed,owe_glyph,attributable,owed_with_pending,flush}	A guess h retired by reconcile_seen without credit (¬Attr) is owed TYPED, glyph_owes.push((h.seq, h.ch, h.born)), when `Rule::TypedOwes` is on and Appended(P,h) holds (tail-guard: at the end of the line, or over a clean tail) or `Rule::TypedOwesCleanTail` is off; otherwise as any echo: owe(h.born) and g.owed := max(g.owed, Some(h.born)) for every pending g (the pre-2026-10-17 contract, which `Rule::TypedOwes` off restores). A typed owe refuses credit only to a later guess for the same glyph while its window runs (Attr), unless `Rule::TypedOwesSameGlyph` is off. Before each push, entries older than GLITCH_CEILING_MS are dropped (no window is longer); at MAX_GLYPH_OWES (64) the oldest folds into owe() and raises every pending g.owed. Flush, reset, take_link_estimate and reset_session fold the typed owes into owed_at (Flush, link-estimate-swap). Why typed: the retired guess's own echo is its glyph, at the cell after the match that retired it, which is the next guess's, so only a later guess for the same glyph can be fooled by it; owed as any echo, each uncredited retirement refused credit to every guess behind it, and a line typed faster than the window never proved after one owed key. Why only for an append: a guess retired mid-line has an echo that shifts text whose glyphs could stand in any later guess's cell (the cursor check flushes the guesses behind such a cell within one row, and the sixty-seed sweep, rewriting far ends included, finds no counterexample; see the theorem's `rule_clause`). A typed owe beside a proof is why an uncredited retirement voids it (reconcile-contract, `Rule::UncreditedVoids`): the retirement it refuses leaves the retired key's echo still to land, on a cell the proof may name once an erase in flight puts the cursor back.	src/lib.rs::an_uncredited_guess_owes_only_its_own_glyph, src/lib.rs::a_guess_retired_over_a_moved_tail_owes_any_echo, src/lib.rs::typed_owes_fold_into_any_echo_past_the_cap_and_at_a_take, src/lib.rs::a_guess_retired_without_credit_owes_its_own_echo	&mut self, a bounded VecDeque of tuples, u64 press order, Instant arithmetic, iterator any with a closure	0,1,2,3,5,7,12,13
edit-key-routing	contract	Predictor::{predict_edit,decline_motion,placeable,press_row_clear,chain_text_at,set_editing}	For mode≠Off and cols>0, after observe_coordinates, grid_cols:=cols, CancelCursor(P,Some(cur),now), settle_chain and (preds=[] ⇒ anchor_lost:=false): editing_off ⇒ Backspace ≡ predict_backspace(now) (Cancelled iff it popped) and ←/→ ≡ note_unmodelled_input(now), Declined. Otherwise a Backspace with preds.back() a current-epoch Insert pops it (backspace-pops-own), Cancelled. Otherwise E = Placeable(P,cur), kept only if the press row is Some, holds no Opaque cell in [E.lo, min(E.V+1, cols)) (R-OPAQUE), and for a Backspace every cell of [E.hi, min(E.V+1, cols)) is Blank or, below E.V, the chain's own text there (the line's, or a pending Insert's) (`Rule::VacateRefusal`). ← is armed iff E.c > E.lo, → iff E.c < E.hi (`Rule::TextEdges`; off: at the edge too), a Backspace iff E.lo < E.c = E.hi; each only while |preds| < EDIT_CHAIN_MAX (64): push {row:E.row, col:E.c, ch:NUL, op, class: Motion for ←/→, Glyph for the erase, born:now, epoch, owed:owed_at, seq:next_seq, stale:false}, next_seq+1, declined_tail:=0, chain:=true, cancelled_at:=None, Armed. Otherwise ←/→ is DECLINED WITH ITS PIXELS KEPT: owe(now) (`Rule::EditOpsOwe`), EndEpoch, anchor_lost:=true, preds unchanged, Declined; a Backspace is predict_backspace(now). Placeable(P,cur) ⇔ ¬editing_off ∧ edit_capable ∧ ¬anchor_lost ∧ confirmed_epoch ∧ line=Some(l) ∧ l.epoch=P.epoch ∧ (preds=[] ⇒ cur=(l.row,l.cursor)) ∧ every g∈preds has g.epoch=P.epoch, g.row=l.row and g.col = the cursor the ops before it leave, every Insert at the text's end with g.col+2 ≤ cols (R-ROW); E is the fold of preds onto l by lengths and cursor (edit-fold), E.V its furthest text end. set_editing(on) flushes on a real change.	src/lib.rs::a_first_motion_stays_dark_until_its_echo_proves_the_class, src/lib.rs::an_arrow_that_cannot_be_placed_keeps_the_guesses_in_flight, src/lib.rs::an_arrow_at_an_edge_of_the_text_is_declined, src/lib.rs::editing_off_routes_edit_keys_exactly_as_before, src/lib.rs::an_erase_beside_text_past_the_end_is_not_armed, src/lib.rs::a_chain_is_capped	&mut self, enum match with guards, a borrowed &[Seen], VecDeque push_back	0,1,2,3,13
edit-fold	definition	Predictor::{fold_op,text_end,chain_far_end}	A state chain's states: S_0 = (line.text, line.cursor), lo = line.lo; S_j = fold(S_{j−1}, preds[j−1]) with Insert(ch)@c: text.insert(c−lo, ch), c+1; Left@c: c−1; Right@c: c+1; Erase@c: text.remove(c−1−lo), c−1. hi_j = lo + |text_j|; V_j = max(line.vacated, hi_0, …, hi_j), the cells [hi_j, V_j) VACATED in S_j; cells at or past V_j are no state's (a right prompt, a suggestion: never compared, never painted). In this increment an Insert and an Erase are only at the text's end (edit-key-routing, predict-accept-decline). The safety theorem's PROVENANCE oracle implements the same rows independently, on a Vec<char> folded from the keys.	tests/reconciled_view_safety.rs::each_named_case_holds_and_catches_its_mutant	Vec insert/remove, u16 arithmetic	0,1,5,7
edit-reconcile	contract	Predictor::{reconcile_chain,match_chain,state_matches,retire_chain,credit_op,retire_net_null,in_transit}	Engaged(P), after observe_coordinates and the alternate-screen flush: seen := obs(row,c) for c ∈ [lo, min(V+1, cols)), V the furthest text end of any state; an Opaque cell ⇒ Flush (R-OPAQUE). Then, with an Erase pending, the SENTINEL seen[V−lo] (the cell one past every state, which no state compares) not Blank ⇒ Flush before any match (`Rule::VacateRefusal`): a split erase echo may still show the chain's own vacated text, which the transit wait below allows, but nothing past every state is the chain's. Match(j) ⇔ cur=Some((row,S_j.c)) ∧ (`Rule::WholeStateMatch`; off: the cursor alone) every c∈[lo,hi_j) holds text_j[c−lo] (Blank ≡ ' ') ∧ (`Rule::VacatedBlankInMatch`) every c∈[hi_j,V_j) is Blank. j* = min{j∈0..=n : Match(j)} (`Rule::SmallestMatch`; off: the max). With j*: pop ops 1..=j* front first; one not Attr(P,now,g) ⇒ owe(g.born) then Flush, stop; each credited records record_rtt (the turn sample is the first), its seq as credited, clears cancelled_pair once a key typed after it retires, and credit_op: an Insert of P.epoch confirms (confirmed_epoch, proof_cell := its state's cursor, proof_echo := g, proof_line_end := tail_line_end); a motion or erase of P.epoch, only while confirmed_epoch (`Rule::MotionsNeverProve`; off: it confirms too), moves proof_cell to its state's cursor, and a motion sets proven ∋ Motion; line_end_at := None. line := S_{j*}, vacated := V_{j*}. NET-NULL: S_{j*} = S_n with ops left ⇒ all retire without credit, owe(max born), Void when one is of P.epoch (`Rule::NetNullUncredited`; off: credited as above). Then, with an Erase pending, seen[hi−lo..] not all Blank ⇒ Flush (`Rule::VacateRefusal`). No j: cur on line.row and every cell some state compares (c < V_k) Blank or holding S_k(c) for some k comparing it ⇒ wait (the TRANSIT WAIT: an ICH before its glyph, a tail rewrite, a cancelled pair, an erase echoed in pieces); otherwise Flush. Not Engaged: reconcile-contract.	src/lib.rs::a_burst_the_screen_cannot_tell_apart_retires_at_the_smallest_state, src/lib.rs::a_split_erase_echo_matches_no_state_early, src/lib.rs::an_erase_in_transit_refuses_new_text_past_every_state, src/lib.rs::a_chain_matches_whole_states_not_its_cursor, src/lib.rs::a_motion_echo_never_proves_its_epoch, src/lib.rs::an_opaque_cell_on_the_chain_s_line_flushes_it	impl FnMut(u16,u16)->Seen in a loop, Vec compare, VecDeque pop_front	0,1,2,3,4,5,7,11,13,f32
edit-class-proof	invariant	Predictor::{op_visible,visible_count,credit_op,void_proof,end_epoch,flush}	∀ P∈Reach(Adaptive): Motion ∈ proven ⇒ confirmed_epoch, and since the glyph proof was last earned a credited smallest-match retirement of a current-epoch motion occurred: credit_op sets it only under the glyph proof, and Void clears it (`Rule::ProofsEndWithTheEpoch`), so every boundary and every void does. EndEpoch sets carried_proven := proven when it grandfathers; Flush clears carried_proven. op_visible(g) = (g.epoch=P.epoch ∧ confirmed_epoch ∧ ClassOk(g,proven)) ∨ (Some(g.epoch)=grandfathered_epoch ∧ ClassOk(g,carried_proven)), ClassOk(g,s) ⇔ g.class=Glyph ∨ Motion∈s (`Rule::MotionClassProof`; off: always). Vis in Adaptive is the leading run of op_visible behind adaptive_slow; with every op of the glyph class it equals the earlier Vis on every reachable state (no epoch's guesses ever wait behind another's: nothing is armed behind a terminator, a decline or an unmodelled key while the queue holds the earlier epoch's).	src/lib.rs::a_motion_proof_does_not_outlive_its_line	bitset u8, take_while closure	0,1,2,13
edit-overlay	contract	Predictor::{overlay,compose_chain,overlay_cursor,pending_bounds}	Engaged ⇒ overlay(P,now) = (expire; v := Vis; ghosts := for c ∈ [lo, max(hi_0..hi_v)) where S_v(c) (text_v[c−lo], or blank at or past hi_v) is not what seen read there (a cell seen did not read counts as different): {row, col:c, ch: S_v(c) or ' '}, carrying the newest visible op's born, epoch and seq; ghost_cursor := Some((row, S_v.c)) iff v>0, except a carried chain (its ops of another epoch) with `Rule::CarryCursor` off; return &ghosts). Not Engaged ⇒ visibility-gate, ghost_cursor := None. overlay_cursor() = ghost_cursor (&self). A host paints the ghosts all or none, draws overlay_cursor only when it painted all and the cursor's cell is no wide glyph's right half, and counts a drawn cursor as a displayed prediction (the theorem's ATOMICITY and CURSOR LOCALITY).	src/lib.rs::an_erase_at_the_end_paints_its_vacated_cell_blank, src/lib.rs::a_chain_carried_across_enter_keeps_its_cursor, src/lib.rs::pending_bounds_cover_the_chain_s_line	Vec rebuild into a resident buffer, returned borrow	0,1,2,3,7,13
edit-expiry	contract	Predictor::expire	Engaged ∧ Exp(P,now,front) ⇒ (`Rule::StateExpiryFlushes`) Flush (owing the newest press, which dates every op), expiry_backoff := min(expiry_backoff+1, 3): every later state is built on the head's effect, so no head-only expiry applies. Off: an Engaged chain never expires. Not Engaged: expiry-contract.	src/lib.rs::an_expired_chain_head_takes_the_chain_with_it	Instant arithmetic, Option	0,1,2,7,12
edit-custody	invariant	every public Predictor method	SINGLE CUSTODY: every key that reached the PTY since the predictor last forgot its session (reset_session; take_link_estimate carries the custody out as owed_at) and was not credited with its echo is held by a pending op of its seq, a typed owe of its seq, or owed_at ≥ its press. Glyph guesses and chain ops share preds; a pop owes its glyph typed; decline-motion, a Backspace with nothing of ours, unmodelled input, a decline, an expiry and every Flush owe; a chain's unattributable retirement and its net-null tail owe.	src/lib.rs::every_key_is_held_until_its_echo_is_credited, src/lib.rs::an_edit_op_dropped_is_owed	ghost history over a trace	0,1,2,10
edit-line-lifetime	invariant	Predictor::{extend_line,settle_chain,void_proof,end_epoch,flush,arm_glyph}	line=Some(l) ⇒ (l.epoch=P.epoch ∧ confirmed_epoch) ∨ Engaged(P). Only extend_line creates or grows it, when a credited glyph g of P.epoch proves as an append (retire_echoed): appended at (l.row, l.cursor = l.hi), else the line restarts at g; a g at column cols−1 drops it (R-ROW). Only a chain's retirement moves it (edit-reconcile). Void drops it while no chain is in flight (a boundary, a cursor the program moved, a tail refusal, an uncredited retirement, an expiry, the standing tail), Flush drops it, settle_chain drops it when the queue drains without the proof, and a glyph typed while l.cursor < l.hi Voids first. R-OWED is implied: the text grows only from credited echoes, so it exists only once no owe was in the window at a credit, and every owe site of the epoch after that drops it or ends the epoch.	src/lib.rs::the_known_line_lives_with_the_proof_it_came_from, src/lib.rs::a_glyph_typed_inside_the_text_is_not_an_append	Option of a struct holding a Vec	0,1,2
stale-anchor	contract	Predictor::{arm_glyph,attributable}	A glyph-lane run anchored on the live cursor (no current-epoch guess to extend) is STALE iff Owed at its first press (owed_at inside W then); each guess of the run carries stale, no chain op does. For a stale guess h, Attr(P,now,h) also requires (`Rule::StaleAnchor`) ¬Owed(P,now) and no pending Insert g with g.seq > h.seq and g.ch = h.ch: the owed echo may move the cursor the run was placed from, and an erase we did not place moves it back, so a guess's cell then receives the NEXT key's echo. Hence a run placed before such an erase lands is never credited by a repeated glyph (§5.18).	src/lib.rs::a_stale_anchor_is_not_credited_by_the_next_key_s_echo	VecDeque scan, Option<Instant>	0,1,2,12,13
```

Merge notes:
- `glitch-window` gained gaps 1 and 2 here. It is a `&self` method (gap 2) that reads `srtt: Option<f32>`
  (gap 1). The chunk had listed only 3, 6, 12 and 13.
- `every_epoch_boundary_is_the_models_submit` appeared in the subsumes list of both
  `no-unechoed-flash` and `visibility-model-refinement`. It is a conformance test of the model, so its
  primary is `visibility-model-refinement`. `no-unechoed-flash` is its secondary (§2).
- No two specs stated the same property, so nothing was merged away. The building blocks `Flush`,
  `EndEpoch`, `W` and `Vis` are defined once, in §0, and every spec cites them from there.

## 2. Test → spec map (TSV; all 112)

`primary` is the spec that retires the test. `also` lists other specs the same test samples. The
`also` column comes from what each statement names as a consequence, so it is a reading, not a census.

```tsv
file	test	primary	also
src/lib.rs	mode_parse_is_fail_safe	mode-parse	
src/lib.rs	only_text_ending_in_a_terminator_is_a_line_submit	is-line-submit	
src/lib.rs	off_mode_predicts_nothing	off-mode-inert	visibility-gate
src/lib.rs	types_ahead_extends_from_last_guess_before_echo	predict-placement	
src/lib.rs	height_blind_hosts_decline_at_the_margin_without_flushing	predict-placement	predict-accept-decline
src/lib.rs	wraps_to_the_next_row_instead_of_flushing	predict-placement	
src/lib.rs	bottom_row_declines_without_flushing	predict-placement	predict-accept-decline
src/lib.rs	the_deferred_wrap_slot_is_not_a_free_cell	predict-placement	reconcile-contract
src/lib.rs	a_free_last_column_is_still_predicted_in_place	predict-placement	reconcile-contract
src/lib.rs	a_wide_glyph_filling_the_last_two_columns_parks_the_cursor	reconcile-contract	predict-placement
src/lib.rs	a_deferred_wrap_on_the_last_row_declines_without_painting	predict-placement	predict-accept-decline
src/lib.rs	wide_glyphs_are_declined_but_narrow_accents_are_predicted	predict-accept-decline	
src/lib.rs	ambiguous_glyphs_are_measured_as_the_terminal_writes_them	predict-accept-decline	
src/lib.rs	changing_the_ambiguous_width_flushes_guesses_placed_under_the_old_one	predict-accept-decline	
src/lib.rs	a_declined_glyph_does_not_erase_the_word_to_its_left	predict-accept-decline	no-unechoed-flash
src/lib.rs	pending_queue_caps_at_max_predictions	queue-cap	predict-accept-decline
src/lib.rs	nothing_is_armed_behind_a_terminator_while_its_line_is_in_flight	predict-accept-decline	submit-carries-proven-guesses
src/lib.rs	the_submitted_lines_echo_cannot_prove_the_next_line	no-unechoed-flash	predict-accept-decline
src/lib.rs	an_echo_owed_to_a_flushed_guess_cannot_prove_a_later_one	reconcile-contract	no-unechoed-flash
src/lib.rs	every_untracked_press_owes_its_echo	submit-carries-proven-guesses	unmodelled-input-retires
src/lib.rs	a_declined_glyph_owes_its_echo	predict-accept-decline	reconcile-contract
src/lib.rs	an_erase_we_did_not_cancel_owes_its_echo	unmodelled-input-retires	reconcile-contract
src/lib.rs	a_guess_retired_without_credit_owes_its_own_echo	reconcile-contract	no-unechoed-flash, typed-owes
src/lib.rs	a_returning_sessions_old_echo_cannot_prove_its_new_guess	link-estimate-swap	reconcile-contract
src/lib.rs	an_owed_echo_neither_proves_nor_measures_a_carried_guess	reconcile-contract	echo-regime-latch
src/lib.rs	an_owe_is_dated_by_its_press	submit-carries-proven-guesses	reconcile-contract
src/lib.rs	a_backspace_cancels_the_glyph_we_declined	backspace-pops-own	submit-carries-proven-guesses
src/lib.rs	pending_bounds_reserves_wrapped_guesses_without_consuming_them	pending-bounds-hull	predict-placement
src/lib.rs	expire_due_consumes_the_deadline_without_a_redraw	expiry-contract	glitch-window
src/lib.rs	expired_prediction_clears_next_deadline	expiry-contract	visibility-gate
src/lib.rs	expiry_retires_only_the_oldest_guess_owes_it_and_voids_the_epoch	expiry-contract	no-unechoed-flash
src/lib.rs	without_expiry_owes_an_expiry_keeps_the_proof_as_it_did	expiry-contract	
src/lib.rs	a_mid_line_echo_proves_nothing	tail-guard	key-row-sample, reconcile-contract
src/lib.rs	a_right_prompt_the_echo_leaves_in_place_still_proves	tail-guard	key-row-sample
src/lib.rs	a_dirty_tail_voids_a_standing_proof	tail-guard	no-unechoed-flash
src/lib.rs	a_retired_burst_reads_its_tail_once	tail-guard	
src/lib.rs	an_opaque_cell_is_its_glyph_to_the_glyph_lane	seen-opacity	reconcile-contract
src/lib.rs	a_run_typed_over_other_text_never_proves	tail-guard	key-row-sample
src/lib.rs	an_uncredited_guess_owes_only_its_own_glyph	typed-owes	reconcile-contract
src/lib.rs	a_guess_retired_over_a_moved_tail_owes_any_echo	typed-owes	tail-guard
src/lib.rs	typed_owes_fold_into_any_echo_past_the_cap_and_at_a_take	typed-owes	link-estimate-swap
src/lib.rs	a_cancelled_glyph_anchors_the_next_on_its_cell	backspace-pops-own	predict-placement
src/lib.rs	a_cancelled_glyph_at_the_margin_does_not_wrap_the_next	backspace-pops-own	predict-placement
src/lib.rs	a_cancelled_glyph_s_echo_is_owed_to_the_same_glyph_typed_next	backspace-pops-own	typed-owes, reconcile-contract
src/lib.rs	a_cancelled_pair_does_not_anchor_after_an_unrelated_cursor_move	backspace-pops-own	reconcile-contract, tail-guard
src/lib.rs	a_suggestion_beside_the_end_of_the_line_darkens_no_key	tail-guard	key-row-sample, reconcile-contract
src/lib.rs	a_right_to_left_line_proves_over_its_opaque_blanks	tail-guard	seen-opacity
src/lib.rs	a_glyph_echoed_before_its_tail_moves_proves_nothing_after	standing-tail	tail-guard
src/lib.rs	a_shifted_glyph_is_not_a_pending_guess_s_echo	standing-tail	tail-guard
src/lib.rs	a_glyph_already_on_screen_is_not_its_echo	reconcile-contract	echo-regime-latch, glitch-window
src/lib.rs	a_predictor_still_off_wants_the_row_of_the_press_that_starts_it	key-row-sample	off-mode-inert
src/lib.rs	adaptive_local_tail_expires_without_a_visible_erase	expiry-contract	visibility-gate
src/lib.rs	slow_link_expiry_window_grows_until_the_echo_can_confirm	glitch-window	expiry-contract
src/lib.rs	silence_never_opens_the_gate_or_diverges_the_estimate	rtt-only-from-echo	expiry-contract, glitch-window
src/lib.rs	sustained_slow_echo_retires_predictions_and_displays_next	echo-regime-latch	visibility-gate, reconcile-contract
src/lib.rs	adaptive_isolated_scheduler_tail_does_not_open_gate	echo-regime-latch	visibility-gate
src/lib.rs	adaptive_slow_to_fast_closes_only_after_a_sustained_fast_streak	echo-regime-latch	
src/lib.rs	adaptive_gate_does_not_flap_across_a_single_fast_sample	echo-regime-latch	
src/lib.rs	adaptive_gate_never_closes_while_a_guess_is_in_flight	echo-regime-latch	
src/lib.rs	burst_is_classified_by_the_oldest_retired_guess	echo-regime-latch	reconcile-contract
src/lib.rs	divergence_flushes_all	reconcile-contract	
src/lib.rs	alt_screen_flushes_predictions	reconcile-contract	
src/lib.rs	space_confirms_by_cursor_advance	reconcile-contract	
src/lib.rs	wrapped_guess_survives_the_deferred_wrap_cursor	reconcile-contract	predict-placement
src/lib.rs	a_wrap_the_terminal_did_not_defer_keeps_the_wrapped_guess	reconcile-contract	predict-placement
src/lib.rs	a_scrolled_cursor_does_not_pass_for_the_deferred_wrap_slot	reconcile-contract	
src/lib.rs	a_scroll_retires_guesses_instead_of_confirming_them_against_moved_cells	reconcile-contract	no-unechoed-flash
src/lib.rs	submit_does_not_blank_the_tail_of_the_command_line	submit-carries-proven-guesses	visibility-gate
src/lib.rs	a_key_we_did_not_model_retires_the_guesses_in_flight	unmodelled-input-retires	
src/lib.rs	an_erase_after_a_boundary_darkens_the_carried_guesses	unmodelled-input-retires	submit-carries-proven-guesses
src/lib.rs	an_erase_landing_on_typed_ahead_text_leaves_no_ghost	unmodelled-input-retires	backspace-pops-own
src/lib.rs	an_erase_we_did_not_model_leaves_no_ghost_where_nothing_echoes	unmodelled-input-retires	
src/lib.rs	backspace_cancels_pending_prediction	backspace-pops-own	
src/lib.rs	fresh_line_resets_confirmation_no_leak	no-unechoed-flash	
src/lib.rs	a_stale_guess_confirming_after_submit_cannot_arm_the_new_line	no-unechoed-flash	reconcile-contract
src/lib.rs	a_scroll_ends_the_epoch_on_the_same_physical_row	no-unechoed-flash	reconcile-contract
src/lib.rs	a_line_terminator_makes_what_follows_tentative	no-unechoed-flash	submit-carries-proven-guesses
src/lib.rs	session_switch_forgets_slow_link_rtt	link-estimate-swap	glitch-window
src/lib.rs	a_backgrounded_session_keeps_its_link_estimate	link-estimate-swap	
src/lib.rs	restoring_an_estimate_clears_the_outgoing_panes_pixels	link-estimate-swap	visibility-gate
src/lib.rs	a_default_estimate_is_the_no_estimate_path	link-estimate-swap	
src/lib.rs	the_cold_link_probe_travels_with_the_session_estimate	link-estimate-swap	glitch-window
src/lib.rs	session_reset_restarts_the_cold_link_probe	link-estimate-swap	glitch-window
src/lib.rs	a_first_motion_stays_dark_until_its_echo_proves_the_class	edit-key-routing	edit-class-proof, edit-reconcile
src/lib.rs	a_burst_the_screen_cannot_tell_apart_retires_at_the_smallest_state	edit-reconcile	edit-custody
src/lib.rs	an_erase_at_the_end_paints_its_vacated_cell_blank	edit-overlay	edit-key-routing, edit-fold
src/lib.rs	a_split_erase_echo_matches_no_state_early	edit-reconcile	edit-key-routing
src/lib.rs	an_erase_in_transit_refuses_new_text_past_every_state	edit-reconcile	edit-key-routing
src/lib.rs	an_arrow_that_cannot_be_placed_keeps_the_guesses_in_flight	edit-key-routing	unmodelled-input-retires, reconcile-contract
src/lib.rs	an_arrow_at_an_edge_of_the_text_is_declined	edit-key-routing	
src/lib.rs	editing_off_routes_edit_keys_exactly_as_before	edit-key-routing	unmodelled-input-retires, backspace-pops-own
src/lib.rs	a_glyph_typed_inside_the_text_is_not_an_append	edit-line-lifetime	predict-accept-decline
src/lib.rs	an_opaque_cell_on_the_chain_s_line_flushes_it	edit-reconcile	edit-key-routing
src/lib.rs	an_erase_beside_text_past_the_end_is_not_armed	edit-key-routing	edit-reconcile
src/lib.rs	an_expired_chain_head_takes_the_chain_with_it	edit-expiry	
src/lib.rs	a_chain_carried_across_enter_keeps_its_cursor	edit-overlay	submit-carries-proven-guesses
src/lib.rs	a_motion_proof_does_not_outlive_its_line	edit-class-proof	
src/lib.rs	a_chain_matches_whole_states_not_its_cursor	edit-reconcile	
src/lib.rs	a_motion_echo_never_proves_its_epoch	edit-reconcile	standing-tail
src/lib.rs	an_edit_op_dropped_is_owed	edit-custody	edit-key-routing
src/lib.rs	pending_bounds_cover_the_chain_s_line	edit-overlay	pending-bounds-hull
src/lib.rs	a_chain_is_capped	edit-key-routing	queue-cap
src/lib.rs	an_edit_chain_does_not_saturate_its_last_column_guard	predict-accept-decline	edit-key-routing, edit-line-lifetime
src/lib.rs	the_known_line_lives_with_the_proof_it_came_from	edit-line-lifetime	
src/lib.rs	every_key_is_held_until_its_echo_is_credited	edit-custody	
src/lib.rs	a_stale_anchor_is_not_credited_by_the_next_key_s_echo	stale-anchor	reconcile-contract
tests/conformance_visibility.rs	adaptive_fast_expiry_is_invisible_and_slow_echo_may_display	visibility-model-refinement	expiry-contract, echo-regime-latch
tests/conformance_visibility.rs	the_display_gate_closes_only_on_sustained_fast_evidence	visibility-model-refinement	echo-regime-latch
tests/conformance_visibility.rs	every_epoch_boundary_is_the_models_submit	visibility-model-refinement	no-unechoed-flash
tests/conformance_visibility.rs	an_owed_echo_proves_nothing_until_its_window_passes	visibility-model-refinement	reconcile-contract
tests/conformance_visibility.rs	expiry_owes_and_voids_the_proof	visibility-model-refinement	expiry-contract
tests/conformance_visibility.rs	an_edit_displays_only_after_its_class_is_proven	visibility-model-refinement	edit-class-proof
```

## 3. Coverage

- 112 tests, each mapped to one of the 33 specs in §2 (the 2026-10-15 recovery: 59 read, 58 mapped to
  19 specs, 1 deleted as §4 says).
- In that recovery the biggest collapses were `predict-placement`, `echo-regime-latch`, `reconcile-contract`,
  `no-unechoed-flash` and `link-estimate-swap` (7 primary tests each), then `expiry-contract` and
  `unmodelled-input-retires` (4 each).
- Gap histogram over the 19 specs of the 2026-10-15 recovery (the number of specs each gap blocks; a
  spec counts once under every gap it lists). The fourteen added since list their gaps in their rows.

| gap | specs blocked |
|---|---|
| 0 | 19 |
| 1 | 19 |
| 2 | 16 |
| 3 | 13 |
| 4 | 4 |
| 5 | 11 |
| 6 | 1 |
| 7 | 11 |
| 10 | 1 |
| 11 | 1 |
| 12 | 3 |
| 13 | 12 |
| f32 (no §7 row) | 4 |

  Gaps 8, 9 and 14–17 block none of these specs directly. The crate has no fs, process, thread or
  proc-macro code under test.

## 4. Tests that are not spec samples

> Status 2026-10-15: the one test here, `unconfirmed_prediction_self_expires`, repeated
> `expired_prediction_clears_next_deadline` and is deleted from `src/lib.rs`.

## 5. Implied specs that look wrong, or disagree with the code

1. **The `expire_due` doc says "in the future"; the code gives "not in the past".** The doc says
   that after `expire_due`, `next_deadline()` is None or IN THE FUTURE. But `expire` keeps a guess
   with `now − born == W`, because it pops only on `> window`. So at exactly the deadline, the front
   survives with `next_deadline == now`. The provable statement is `≥ now`.
   `expire_due_consumes_the_deadline_without_a_redraw` samples deadline + 1 ms and never reaches the
   equality case. Either pop on `>=` or correct the doc.
2. **The doc says predicting is refused in the alternate screen, but it is not.** The crate docs
   (Alt-screen gate) say that in the alternate screen "predicting is refused". But
   `predict_char`/`predict_char_in_grid` take no alt-screen input and check none. A guess typed in
   the alt screen is armed, and in Always mode painted, until the next `reconcile` flushes it.
   `alt_screen_flushes_predictions` tests only the reconcile flush. Either the doc or the API is
   wrong.
3. **A restore test did not isolate its claim (deleted 2026-10-16).** The `proof_cell` mismatch
   rule alone kept its guess dark. `restoring_an_estimate_clears_the_outgoing_panes_pixels` pins the
   claim: after a restore, the outgoing pane's confirmation is gone.
4. **A stale comment describes an epoch-row mechanism.** The comment in
   `fresh_line_resets_confirmation_no_leak` ("row 1 != epoch row 0 → fresh epoch") describes a
   mechanism that no longer exists. Gating now uses `proof_cell` (the exact cell) plus epochs. The
   test still passes, through the proof_cell rule.
5. **The model's Submit cannot express the submit carry.** The model's `Submit` sets
   `pending = 0, visible = 0`. But `note_line_submit` keeps pending guesses, and keeps a proven
   epoch's guesses visible (`grandfathered_epoch`). `every_epoch_boundary_is_the_models_submit` fires
   Submit only on a drained predictor, which hides the difference. So the model cannot express the
   carry that `submit_does_not_blank_the_tail_of_the_command_line` and
   `a_line_terminator_makes_what_follows_tentative` pin. `visibility-model-refinement` needs a Submit
   with a carried-pending branch before it can be stated truthfully.
6. **One conformance test's negative control mutates no code.** In
   `adaptive_fast_expiry_is_invisible_and_slow_echo_may_display`, the "negative control"
   (`confirmation_only_visible` vs the overlay length) recomputes a hypothetical deleted gate from two
   booleans inside the test. It never runs mutated code, so it fails the `derive.rs` rule that a
   mutated body or model must go red.
7. **The "floor 250 ms" in the docs is not always the floor.** The crate docs give the glitch window
   a "floor 250 ms". With `srtt = Some` and `expiry_backoff > 0` (a timeout after a confirmation),
   the clamp floor is the backed-off probe: 500, 1000 or 2000 ms. The window always lies in
   [250, 2000] ms, which is what `glitch-window` states, but its floor is not always 250 ms.
8. **The deferred-wrap anchor mixes old and new column counts.** In `predict_char_in_grid`, the
   deferred-wrap anchor compares `cursor.1 + 1` with the CURRENT `cols`. But `deferred_wrap` was
   latched in `reconcile` against the previous `grid_cols`. If `cols` changes without a reset, a
   stale park reading can wrap a guess. Hosts are told to reset on resize, but the code does not
   enforce it, and no test covers it. `predict-placement` as stated assumes `cols = grid_cols`
   whenever `deferred_wrap` holds. Either add that `requires` or clear `deferred_wrap` when `cols`
   changes.
9. **The safety theorem's link model was vacuous at RTT ≥ 250 ms (fixed 2026-10-17).** FIDELITY
   called an echo late when it arrived more than a fixed 250 ms after its key, so on every link
   slower than that (the links prediction is for) no displayed guess was ever compared with its
   echo. The deadline is now the predictor's own glitch window at the output's arrival, never less
   than 250 ms (`Sim::late_predecessor`): an echo inside it is one the predictor promised to
   account for. A late echo is excluded for what it can touch: the key it is still arriving after,
   and every key of an epoch in which it landed with a later key already typed (standing in that
   key's cell it may have earned the proof), until an epoch boundary the harness sees for certain.
10. **`expire` owed nothing and kept the proof (fixed 2026-10-17).** The expired key was sent, so
    its echo could still come, and it lands on the cell the epoch's proof names. With the proof
    kept, the next glyph was painted on that cell and echoed one cell on (the theorem's FIDELITY
    clause under the scaled link model, thirteen ops; the named case
    `an_expired_guess_s_late_echo_is_owed`). `expiry-contract` states the fix; `Rule::ExpiryOwes`
    off is the old code.
11. **A mid-line echo proved its epoch (fixed 2026-10-17).** Positional evidence sees a glyph and a
    cursor advance and nothing else, so the echo of a key typed mid-line, which shifts the text right
    of it, proved the epoch like an append, and the next glyph was painted over the tail that moved:
    `ab cd`, three Lefts onto the space, `e`, `i` showed `abeicd` until the screen settled to
    `abei cd`. Only the theorem's whole-frame clause (FRAME FIDELITY) sees it: FIDELITY's one cell was
    right. `tail-guard` states the fix, from a row the press reads (`key-row-sample`), and the named
    case is `mid_line_over_a_space`; `Rule::TailGuard` off is the old code.
12. **One owed key kept a fast typist's line dark (fixed 2026-10-17).** Owed as any echo, each guess
    retired without credit raised the owe of every guess behind it, so after one owed key (an Enter,
    an arrow) nothing on a line typed faster than the glitch window was ever credited. `typed-owes`
    owes it as its own glyph; the reviewed `n`, `n`, `c` interleaving stays green and is the named
    case of `Rule::TypedOwesSameGlyph`. Its append condition has no counterexample in the theorem's
    world (sixty seeds, rewriting far ends included); its unit test pins the branch.
13. **A glyph typed after a cancelling Backspace anchored a cell off (fixed 2026-10-17).** The
    cancelled glyph's echo and the erase's arrive one after the other, the screen a cell ahead between
    them, or parked in the deferred-wrap slot when the cancelled glyph filled the last column. A glyph
    typed then anchored on that live cursor: a cell right, where the next key's echo (a space confirming
    by cursor advance) was credited to it and proved the epoch a cell behind (FIDELITY, the sweep's
    seed 13), or wrapped to the next row while it echoes in the last column (seed 39).
    `backspace-pops-own` now leaves the cancelled cell as the next guess's anchor (`predict-placement`);
    `Rule::CancelTransit` off is the old code. An owe of the erase was tried first: it cured seed 13 but
    not seed 39, and left an owe standing beside a proof, which the model's `ProofIsAttributable` forbids.
    Since review the cancelled glyph's echo is also owed, typed (`Rule::CancelOwes`, found by
    ATTRIBUTION: anchored on the cancelled cell, a guess for the same glyph was credited with it), and
    with that rule and the standing tail in force the theorem finds nothing with `CancelTransit` off:
    its named cases (`a_cancelled_glyph_leaves_its_cell_as_the_anchor`, now `CancelOwes`'s control, and
    `a_cancelled_glyph_at_the_margin_does_not_wrap_the_next`) hold either way, and what the rule still
    buys, a line that keeps proving, is liveness its unit tests pin.
14. **A run typed over text re-proved once its tail left the row (2026-10-17; evidence corrected after
    review).** The forty-seed sweep reported (seed 26) inserts typed back into a wrapped line pushing the
    tail off the row's right edge, so the row right of a later echo read blank and a pending space's
    cell holding a shifted space passed for that space's echo. `tail-guard` then required every guess of
    the run to have been typed over nothing or its own glyph, from a snapshot that starts at the cursor
    cell. But the same change took inserts that push the tail past the margin out of the theorem's scope
    (a real editor rewraps them), so the theorem carries no evidence for that rule: removing it leaves
    every theorem test green across sixty seeds. It stands as the fail-closed reading of "typed over
    other text", and `a_run_typed_over_other_text_never_proves` pins both halves (the guess's own cell,
    and a pending guess's in the same burst).
15. **A glyph echoed before its tail redraw proved (fixed 2026-10-17, after review).** A far end that
    inserts mid-line by writing the glyph, then the tail it shifts, then stepping back (no ICH, or zle's
    tail rewrite) showed `abecd` with the cursor past `e` on a frame between the two: the row right of
    `e` was what the press saw, so `e` proved, and `i`, typed on that proof once the redraw had landed,
    was painted over the shifted space (`abeicd` shown, `abei cd` settled; FRAME FIDELITY). The tail was
    compared only as each guess retired. `standing-tail` re-reads it while the proof stands, and the
    theorem's world now rewrites tails (`Config::rewrite`) and splits calm deliveries; its named case is
    `a_glyph_echoed_before_its_tail_moves_proves_nothing_after`, `Rule::StandingTail` off the old code.
16. **A run typed along an autosuggestion was credited before any echo (fixed 2026-10-17, after
    review).** Its cells already showed the typed glyphs, so each guess was retired on the glyph alone,
    measured at a few milliseconds, and credited; the smoothed RTT fell from 300 to 72 ms and the window
    from 900 to 250, an owed echo on time for the link then lapsed and was credited to a later guess for
    the same glyph, and the glyph after it was painted a cell off. FIDELITY could not see it: its link
    model called the 300 ms echo late against the collapsed window. A glyph is now an echo only with the
    real cursor past it (`reconcile-contract`, `Rule::CursorAdvance`), and the theorem's ATTRIBUTION
    clause holds every credit to its own key's output, independent of the window; its named case is
    `cursor_advance_is_the_echo_s_evidence`.
17. **A guess retired without credit left its epoch's proof standing (fixed 2026-10-17, after review).**
    A space typed on the cell a Backspace had cancelled was retired by the cancelled space's echo, without
    credit (owed, typed); the proof stayed on that cell, the erase then put the cursor back there, and `e`
    was painted on it while the space's own echo took it (FIDELITY, seed 43 of the sweep once ATTRIBUTION
    and the rewriting far end were in the world). `reconcile-contract` voids the proof at every uncredited
    retirement of the current epoch (`Rule::UncreditedVoids`, named case
    `an_uncredited_retirement_leaves_no_proof`). The model's `Echo` keeps `confirmed`, so this is a
    strengthening of the refinement below the model's abstraction (`visibility-model-refinement`).
18. **A run placed before an erase we did not place had landed was credited by the next key's echo
    (fixed 2026-10-17, predictive editing increment 2).** A Backspace that cancels nothing of ours is owed and
    ends the epoch, but its echo steps the cursor back; `c`, `c`, `c` typed before it lands were placed on the
    live cursor, a cell right of where they echo. Each guess's cell then received the NEXT key's echo, glyph and
    cursor advance alike; with a repeated glyph, and the Backspace's owe lapsed by then, the first guess was
    credited, proved the line a cell behind, and the third `c` was painted where the second echoed (FIDELITY,
    seed 76 of the eighty-seed sweep of the edit lane's world). Typed owes ask only about keys typed before a
    guess. The edit lane made it reachable: an arrow declined with its pixels kept now keeps, and later credits,
    the guess in flight, which opened the Adaptive gate. `stale-anchor` refuses credit to a run anchored while an
    echo was owed whenever an owe is in flight or a later guess for the same glyph is pending
    (`Rule::StaleAnchor`, named case `a_run_placed_before_an_erase_lands_is_not_credited_by_the_next_key`); with
    it the faithful predictor holds on 120 seeds.
19. **ATTRIBUTION's scope claimed a credit made inside unprompted output (corrected 2026-10-17).** A frame
    caught inside an unprompted autosuggestion, its text drawn and the cursor not yet stepped back over it, shows
    a typed glyph with the cursor past it exactly as an echo does, and the guess under it was credited (seed 33 of
    the same sweep, a guess anchored on a cancelled glyph's cell). ATTRIBUTION excluded unprompted output only
    after the press; it now also excludes a credit made while an unprompted segment is part-way onto the screen,
    as FRAME FIDELITY already excluded a frame inside one (`Sim::unprompted_in_transit`). Nothing positional can
    tell that glyph from an echo; no predictor rule changed.
20. **Four edit-lane rules have no counterexample in the theorem's world (2026-10-17).** Each is a `Rule` with
    `rule_clause` `None`, pinned by its unit test, and the sweep over the committed seed with each switched off
    finds nothing: `VacatedBlankInMatch` (an erase matched early, mid-echo, is the far end's own transit, a key
    whose output has begun: no frame on a settled base shows it); `MotionsNeverProve` (an edit key is armed only
    on a proven line, so its credit could re-prove only a proof voided while it was in flight, which only the
    standing tail does without ending the epoch, on a line that still echoes); `VacateRefusal` (the text past the
    end of the line here is decoration, which an erase leaves or clears and the decoration rule counts; an unseen
    buffer tail needs a line known only up to the middle of its buffer, and the known line grows only from proved
    appends); `EditOpsOwe` (an arrow moves only the cursor, and a guess typed after one with nothing placed is
    a glyph-lane guess the tail guard, the cursor advance and the stale anchor already refuse). ATOMICITY and
    CURSOR LOCALITY are likewise unreachable in the world (a chain flushes on any opaque cell of its line), so
    their controls run on a hand-built frame (`the_chain_painter_paints_all_or_none`), and the host that hides
    opaque cells from the edit lane waits for increment 3, whose span painter moves cells.

## 6. Proof order (easiest high-value first)

No spec is blocked by gap 0 alone. Every one also needs at least gap 1 (struct, `Option` or
`VecDeque` carriers), and all but two need gap 2 (`&mut self`/`&self` views). So the order puts the
smallest gap sets first and, within those, the specs that retire the most tests or supply lemmas for
later ones.

1. **`link-estimate-swap`** (gaps 0, 1, 2, 4). This is field assignment on a `Copy` struct plus the
   composition take → reset_session → reset → flush, with no arithmetic and no loops. It retires
   7 tests. Its `Flush` and `reset` lemmas are reused by almost every spec after it.
2. **`submit-carries-proven-guesses`** (gaps 0, 1, 2). It has the smallest gap set in the crate: one
   `wrapping_add` and `Option` assignments. It proves `EndEpoch`, the lemma that clauses (c) and (d)
   of `no-unechoed-flash` are built on.
3. **`off-mode-inert`, then `backspace-pops-own`** (gaps 0–3, plus 13 for `is_some_and`). These are
   early returns and one `pop_back`. With `Flush` already proved, `unmodelled-input-retires` (4 tests)
   follows at the same cost.
4. **`glitch-window`** (gaps 0–3, 6, 12, 13, f32). It has no loops, only bounded shift, cast and clamp
   arithmetic, so once a float model exists the bounds are an interval proof. It is the lemma that
   `expiry-contract` (4 tests) and the link specs cite for `W`.
5. **`echo-regime-latch`** (7 tests). `record_echo_regime` is straight-line saturating `u8` with `f32`
   comparisons. Its `reconcile` half (one call per turn, on the first sample) has to wait for
   `reconcile-contract`.

After these: `expiry-contract` and `visibility-gate` (loop gap 7), `predict-placement` and
`predict-accept-decline`, then `reconcile-contract` (the higher-order `obs` closure, gaps 11 and 13).
Next is `no-unechoed-flash`. It is the password guarantee and the crate's most valuable spec, but it
needs ghost history state over a whole trace, so it comes after the per-method contracts it is
assembled from. `visibility-model-refinement` comes last: it needs gap 10 and the model fix in §5.5.
`mode-parse` and `is-line-submit` are independent pure functions. They become cheap once gap 1 gives
`&str`/`&[u8]` content models with a Unicode `is_control`/whitespace table, and they can be done at
any point after that.
