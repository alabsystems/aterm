<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-predict: spec recovery from its 59 unit tests

> Status 2026-10-15: this is the crate's migration worklist. Each spec below is proved
> through Trust/Clean about the compiled Rust function, and then the tests it subsumes are
> deleted. The one test in §4 that samples no spec can be deleted now. Nothing has been
> proved yet.

Source: `crates/aterm-predict` (read only). `src/lib.rs` has 56 `#[test]` fns and
`tests/conformance_visibility.rs` has 3, for 59 in all. I read every test and the code it reaches.
Nothing was compiled or run, so every claim below comes from reading the code.

Result: **19 specs** cover **58 tests**. **1 test is not a spec sample** (§4).

Gap numbers refer to the gap table in
the historical development audit `docs/AUDIT-trust-for-test-migration-2026-10-15.md` §7.
Gap 0 (an installed seal that publishes Clean rows) is a prerequisite for every spec, so the column
lists it on every row. No spec here is blocked by gap 0 alone.

## 0. Notation used in the statements

- Test ids are crate-relative: `src/lib.rs::name` and `tests/conformance_visibility.rs::name`.
- `P` is a `Predictor`. Its fields are as in `src/lib.rs`: `mode`, `preds` (a `VecDeque<Prediction>`
  with the oldest at the front), `srtt`, `expiry_backoff`, `slow_samples`, `fast_samples`,
  `adaptive_slow`, `confirmed_epoch`, `proof_cell`, `epoch`, `grid_cols`, `deferred_wrap`,
  `anchor_lost`, `grandfathered_epoch` and `coords`. A guess `g` has `row`, `col`, `ch`, `born` and
  `epoch`.
- `predict_char*` means both `predict_char` and `predict_char_in_grid`.
- `Reach(m)`: the predictors reachable from `Predictor::new(m)` by any sequence of public calls with
  non-decreasing `now` arguments. The public calls are `set_mode`, `predict_char*`,
  `predict_backspace`, `note_line_submit`, `note_unmodelled_input`, `reconcile` with any `observe`
  closure, `overlay`, `expire_due`, `reset`, `reset_session`, `take_link_estimate`, and
  `restore_link_estimate(e)` where `e` came from `take_link_estimate` or `LinkEstimate::default()`.
- `Mono(P)`: `g.born` does not decrease along `preds`. Every `P ∈ Reach` satisfies it, because `now`
  is non-decreasing.
- `Link(P) = (srtt, slow_samples, fast_samples, adaptive_slow, expiry_backoff)`, the fields that
  `LinkEstimate` carries. `Screen(P)` is every other field.
- `Flush(P)`: `preds := []`, `confirmed_epoch := false`, `proof_cell := None`, `anchor_lost := false`,
  `grandfathered_epoch := None`, with every other field kept. This is the private `fn flush`.
- `EndEpoch(P)`: if `confirmed_epoch` then `grandfathered_epoch := Some(epoch)`; then
  `epoch := epoch.wrapping_add(1)`, `confirmed_epoch := false`, `proof_cell := None`,
  `anchor_lost := false`. This is the private `fn end_epoch`.
- `W(P) = glitch_window(P)`. `Exp(P,now,g) ⇔ now.saturating_duration_since(g.born) > W(P)`.
- `Vis(P) = visible_count(P)`: `Off → 0`; `Always → |preds|`; `Adaptive ∧ ¬adaptive_slow → 0`;
  `Adaptive ∧ adaptive_slow ∧ confirmed_epoch → |preds|`; otherwise the length of the leading run of
  `preds` with `Some(g.epoch) = grandfathered_epoch`.
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
predict-placement	contract	Predictor::{predict_char_in_grid,predict_char}	predict_char(ch,cur,g,cols,now) = predict_char_in_grid(ch,cur,g,(cols,0),now). For mode≠Off, cols>0 and the acceptance filter passed (predict-accept-decline): let A = the last g∈preds with g.epoch=P.epoch. base = (A.row, A.col.saturating_add(1)) if A exists; else (cur.row, cols) if deferred_wrap ∧ cur.col+1=cols; else cur. base.col<cols ⇒ the guess goes at base; otherwise at (base.row+1, 0) when base.row+1<rows, and the call declines (false, preds unchanged) when base.row+1≥rows. On accept it pushes {row,col,ch,born:now,epoch:P.epoch} at the back, returns true, and sets grid_cols:=cols. Consequences: type-ahead extends the previous guess and does not land on the live cursor; a height-blind host (rows=0) declines at the margin and keeps the armed guesses; a parked deferred-wrap cursor wraps the next glyph; a free last column is predicted in place.	src/lib.rs::types_ahead_extends_from_last_guess_before_echo, src/lib.rs::height_blind_hosts_decline_at_the_margin_without_flushing, src/lib.rs::wraps_to_the_next_row_instead_of_flushing, src/lib.rs::bottom_row_declines_without_flushing, src/lib.rs::the_deferred_wrap_slot_is_not_a_free_cell, src/lib.rs::a_free_last_column_is_still_predicted_in_place, src/lib.rs::a_deferred_wrap_on_the_last_row_declines_without_painting	&mut self, VecDeque iter().rev().find(closure), push_back, saturating u16, match with guards on Option, tuple destructuring	0,1,2,3,5,7,13
predict-accept-decline	contract	Predictor::{predict_char_in_grid,decline}, aterm_grapheme::char_width	For mode≠Off and cols>0, after observe_coordinates and `if preds=[] then anchor_lost:=false`: the call declines ⇔ ch.is_control() ∨ char_width(ch)≠1 ∨ anchor_lost ∨ |preds|≥MAX_PREDICTIONS ∨ the wrap reaches the bottom edge. A declined control char is Flush (= note_unmodelled_input). A declined printable char leaves preds unchanged, applies EndEpoch, sets anchor_lost:=true and returns false. While anchor_lost ∧ preds≠[], every later predict_char* declines with preds unchanged; the first predict after preds drains is judged afresh. cols=0 ⇒ false, P unchanged.	src/lib.rs::wide_glyphs_are_declined_but_narrow_accents_are_predicted, src/lib.rs::a_declined_glyph_does_not_erase_the_word_to_its_left	&mut self, char::is_control, cross-crate width table (aterm_grapheme::char_width), VecDeque len	0,1,2,3,4,5,12
queue-cap	invariant	Predictor::predict_char_in_grid; every public &mut method	∀ m, ∀ P∈Reach(m): |P.preds| ≤ MAX_PREDICTIONS (256). In particular predict_char* with |preds|=256 returns false and keeps |preds|=256 whatever epochs the guesses carry. Only predict_char_in_grid grows preds, by at most 1 and only when |preds|<256.	src/lib.rs::pending_queue_caps_at_max_predictions_across_line_submits	&mut self across every public method, VecDeque length; reconcile/expire have while loops that only pop	0,1,2,5,7
pending-bounds-hull	contract	Predictor::pending_bounds	pending_bounds(P)=None ⇔ preds=[]; otherwise Some((min g.row, min g.col, max g.row+1, max g.col+1)) over g∈preds, computed in usize so nothing saturates. It is &self: P, next_deadline and |preds| are unchanged. After reset (or any Flush) it is None.	src/lib.rs::pending_bounds_reserves_wrapped_guesses_without_consuming_them	iterator map + reduce over a VecDeque, usize::from(u16), min/max, Option of a 4-tuple	0,1,3,5,7,13
expiry-contract	contract	Predictor::{expire,expire_due,overlay,next_deadline,is_displaying}	∀ P, now: expire pops the maximal prefix of preds satisfying Exp(P,now,·), with W evaluated once before the loop. If it pops ≥1, expiry_backoff' = min(expiry_backoff+1, 3) and every other field is untouched (confirmed_epoch, proof_cell, epoch, grandfathered_epoch, srtt, slow/fast_samples, adaptive_slow). expire_due(now) = (expire popped ≥1). overlay(now) = expire, then preds[..Vis]. next_deadline(P) = preds.front().map(|g| g.born + W(P)). Under Mono, after expire(now): next_deadline is None or ≥ now (not > now; see §5.1), and a second expire_due(now) is false. is_displaying(P,now) ⇔ preds≠[] ∧ (Exp(P,now,front) ∨ Vis(P)>0), and it is &self. Corollary: an Adaptive guess hidden by the gate expires with overlay=[] before and after the deadline.	src/lib.rs::expire_due_consumes_the_deadline_without_a_redraw, src/lib.rs::expired_prediction_clears_next_deadline, src/lib.rs::expiry_retires_only_the_oldest_guess_and_keeps_the_epoch, src/lib.rs::adaptive_local_tail_expires_without_a_visible_erase	&mut self, while-let popping a VecDeque front, Instant::saturating_duration_since and Instant+Duration (cross-crate opaque type), Duration compare, saturating u8, make_contiguous + slice get returning a borrow tied to &mut self	0,1,2,3,7,12
glitch-window	contract	Predictor::glitch_window	Let f = min((250 << min(expiry_backoff,3)) as f32, 2000.0). W(P) = Duration::from_millis(x as u64), with x=f when srtt=None and x=clamp(3·s, f, 2000) when srtt=Some(s). f ∈ [250,2000], so the clamp never panics (min ≤ max). ∀ P∈Reach: expiry_backoff ≤ 3 ∧ 250 ms ≤ W(P) ≤ 2000 ms. With srtt fixed, W is non-decreasing in expiry_backoff; with srtt=None it is 250·2^backoff ms.	src/lib.rs::slow_link_expiry_window_grows_until_the_echo_can_confirm	&self, Option<f32>, u64 shift by a u8, u64→f32 and f32→u64 casts, f32 min/clamp (panic precondition), Option::map_or with a closure, Duration::from_millis	0,1,2,3,6,12,13,f32
rtt-only-from-echo	invariant	Predictor::{record_rtt,reconcile,expire,record_echo_regime}	srtt is written only by record_rtt, called only when reconcile confirms a guess: srtt' = ms(d) if srtt=None else 0.7·s + 0.3·ms(d), and expiry_backoff' = 0. slow_samples, fast_samples and adaptive_slow are written only by record_echo_regime (once per reconcile turn that confirms ≥1 guess) and by reset_session / take_link_estimate / restore_link_estimate. Hence ∀ trace from Predictor::new(m) with no confirming reconcile: srtt=None, adaptive_slow=false, confirmed_epoch=false and, in Adaptive, Vis=0. A link that never echoes is never painted, and timeouts never feed the estimate.	src/lib.rs::silence_never_opens_the_gate_or_diverges_the_estimate	trace-level frame condition over every &mut method, f32 EWMA, Duration::as_secs_f32	0,1,2,5,7,f32
echo-regime-latch	contract	Predictor::{record_echo_regime,reconcile}	Each reconcile that confirms ≥1 guess calls record_echo_regime exactly once, on the turn sample (the first confirmed guess's latency, not the newest). record_echo_regime(d), x=ms(d): x ≥ 20 ⇒ fast:=0, slow:=min(slow+1,2), and adaptive_slow:=true if slow≥2. x < 8 ⇒ slow:=0, fast:=min(fast+1,3), and adaptive_slow:=false if fast≥3 ∧ (srtt=None ∨ srtt<8) ∧ preds=[], else unchanged. 8 ≤ x < 20 ⇒ no change. Consequences ∀ P∈Reach: adaptive_slow goes false→true only on the second consecutive decisive slow turn, and true→false only on a turn with ≥3 consecutive decisive fast turns, smoothed srtt<8 ms and nothing pending; a dead-band sample resets neither streak; a burst retired in one turn counts as one sample.	src/lib.rs::sustained_slow_echo_retires_predictions_and_displays_next, src/lib.rs::adaptive_hides_on_fast_local_echo, src/lib.rs::adaptive_isolated_scheduler_tail_does_not_open_gate, src/lib.rs::adaptive_slow_to_fast_closes_only_after_a_sustained_fast_streak, src/lib.rs::adaptive_gate_does_not_flap_across_a_single_fast_sample, src/lib.rs::adaptive_gate_never_closes_while_a_guess_is_in_flight, src/lib.rs::burst_is_classified_by_the_oldest_retired_guess	&mut self, f32 compare and EWMA, saturating u8 with min, Option::get_or_insert in the reconcile loop, Option::is_none_or closure	0,1,2,3,5,7,13,f32
visibility-gate	contract	Predictor::{overlay,visible_count}	∀ P, now: overlay(P,now) = (expire(P,now); P.preds[..Vis(P)]). Vis(P) ≤ |preds| always, so the get(..) never takes its unwrap_or branch. The result is a leading run of preds in type order. Always mode shows every pending guess at once, with no confirmation needed.	src/lib.rs::always_mode_shows_immediately_without_confirmation	enum match with guards, take_while + count with a closure, VecDeque::make_contiguous, slice get with a range, a returned borrow	0,1,2,3,7,13
reconcile-contract	contract	Predictor::{reconcile,observe_coordinates,confirm}	reconcile(P,cur,g,alt,now,obs), ∀ closures obs. (1) P.coords=Some(c) ∧ c≠g ⇒ P:=reset(P); then coords:=Some(g). (2) alt ⇒ Flush, deferred_wrap:=false, return. (3) Repeatedly, h=front: obs(h.row,h.col)=Some(h.ch), or h.ch=' ' ∧ obs(h.row,h.col)=None ∧ cur=Some((h.row,c')) with c'>h.col ⇒ record_rtt(now−h.born), confirm(h), pop; obs(h.row,h.col)=Some(c≠h.ch) ⇒ Flush and stop; otherwise stop. (4) preds≠[] ∧ cur is neither Some((front.row,front.col)) nor the deferred-wrap slot of a wrapped front (front.col=0 ∧ grid_cols>0 ∧ cur=Some((front.row−1, grid_cols−1))) ⇒ Flush. (5) deferred_wrap := cur=Some((r,c)) ∧ grid_cols>0 ∧ c+1=grid_cols ∧ obs(r,c).is_some(). confirm(h) sets confirmed_epoch:=true and proof_cell:=Some((h.row, if h.col+1 ≥ grid_cols then h.col else h.col+1)) iff h.epoch=P.epoch, else does nothing. Corollaries: a wrong glyph or the alternate screen empties preds; an echoed space confirms by cursor advance; a scroll (changed g) retires every guess before any glyph is compared, so a moved cell never confirms; a cursor parked one row off is not a deferred-wrap slot.	src/lib.rs::divergence_flushes_all, src/lib.rs::alt_screen_flushes_predictions, src/lib.rs::space_confirms_by_cursor_advance, src/lib.rs::wrapped_guess_survives_the_deferred_wrap_cursor, src/lib.rs::a_wrap_the_terminal_did_not_defer_keeps_the_wrapped_guess, src/lib.rs::a_scrolled_cursor_does_not_pass_for_the_deferred_wrap_slot, src/lib.rs::a_scroll_retires_guesses_instead_of_confirming_them_against_moved_cells	generic `impl FnMut(u16,u16)->Option<char>` called in a loop (higher-order spec; obs arbitrary but deterministic per call), while-let over a VecDeque, match with guards, Instant arithmetic, f32 via record_rtt, &mut self	0,1,2,3,4,5,7,11,13,f32
submit-carries-proven-guesses	contract	Predictor::{note_line_submit,end_epoch,visible_count}	note_line_submit(P) = EndEpoch(P), and preds is unchanged, so next_deadline is unchanged. In Always the overlay is unchanged across the call. In Adaptive with adaptive_slow ∧ confirmed_epoch before the call, the ended epoch's guesses (a leading run of preds) stay in Vis, and every guess armed afterwards (epoch ≠ grandfathered) is outside Vis until a guess of the new epoch confirms. A second note_line_submit with no confirmation between keeps grandfathered_epoch (confirmed_epoch is false), so the carry survives any number of terminators.	src/lib.rs::submit_does_not_blank_the_tail_of_the_command_line	&mut self, u64 wrapping_add, Option assignment	0,1,2
unmodelled-input-retires	contract	Predictor::{note_unmodelled_input,predict_backspace,decline}	∀ P with mode≠Off: note_unmodelled_input(P) = Flush(P). predict_backspace(P) with preds=[] or back.epoch ≠ P.epoch is also Flush(P) and returns false. predict_char* of a control char is Flush(P) and returns false. Afterwards Vis=0 and overlay=[] in Adaptive and in Always, so neither a guess carried across a submit nor typed-ahead text a backward erase removes leaves a ghost. Until reconcile confirms a new guess, the next predict_char starts unproven (confirmed_epoch=false).	src/lib.rs::a_key_we_did_not_model_retires_the_guesses_in_flight, src/lib.rs::an_erase_after_a_boundary_darkens_the_carried_guesses, src/lib.rs::an_erase_landing_on_typed_ahead_text_leaves_no_ghost, src/lib.rs::an_erase_we_did_not_model_leaves_no_ghost_where_nothing_echoes	&mut self, VecDeque clear/back, Option::is_some_and closure	0,1,2,3,13
backspace-pops-own	contract	Predictor::predict_backspace	∀ P with mode≠Off: preds.back()=Some(g) ∧ g.epoch=P.epoch ⇒ predict_backspace pops exactly that element, returns true and changes no other field. Otherwise see unmodelled-input-retires. Off ⇒ false, P unchanged.	src/lib.rs::backspace_cancels_pending_prediction	&mut self, VecDeque back/pop_back, Option::is_some_and closure	0,1,2,3,13
no-unechoed-flash	invariant	every public Predictor method; Predictor::{confirm,end_epoch,flush,observe_coordinates,predict_char_in_grid,visible_count}	∀ P∈Reach(Adaptive): (a) confirmed_epoch ⇔ proof_cell.is_some(). (b) confirmed_epoch ⇒ since the last boundary B a reconcile confirmed a guess h with h.epoch=P.epoch under coords=P.coords, and proof_cell is the cell after h (h's own cell at the right margin). (c) grandfathered_epoch=Some(e) ⇒ e≠P.epoch, and epoch e had confirmed_epoch=true when note_line_submit ended it. (d) ∀ g∈overlay(P,now): (g.epoch=P.epoch ∧ confirmed_epoch) ∨ Some(g.epoch)=grandfathered_epoch. Corollary (the password guarantee): ∀ B, ∀ predict_char* after B with no confirming reconcile of a post-B guess in between, the new guess is not in overlay. That covers a new row, a prompt printed on the same row (cursor ≠ proof_cell), a scroll seen at the key seam or at reconcile, a line terminator, a stale pre-submit guess confirming after it (confirm ignores h.epoch≠epoch), a declined glyph, an unpopped Backspace, unmodelled input and an estimate restore.	src/lib.rs::fresh_line_resets_confirmation_no_leak, src/lib.rs::submit_resets_epoch_same_row_no_password_leak, src/lib.rs::a_stale_guess_confirming_after_submit_cannot_arm_the_new_line, src/lib.rs::a_scroll_ends_the_epoch_on_the_same_physical_row, src/lib.rs::a_line_terminator_makes_what_follows_tentative, src/lib.rs::a_prompt_printed_after_the_proof_on_the_same_row_voids_it, src/lib.rs::the_key_seam_sees_a_scroll_the_reconcile_never_did	trace invariant over every &mut method (history ghost state for clause b), per-element quantification over a VecDeque, reconcile's obs closure, u64 wrapping epochs (needs no wrap-around within one carry, or a statement by epoch inequality), Option<u64>	0,1,2,4,5,7,13
link-estimate-swap	round-trip	Predictor::{reset,reset_session,take_link_estimate,restore_link_estimate}, LinkEstimate::default	reset(P) = Flush(P) with coords:=None and deferred_wrap:=false, keeping Link(P). reset_session(P) = reset(P) with Link := LinkEstimate::default() (srtt=None, 0, 0, false, backoff 0). take_link_estimate(P) returns E=Link(P) and leaves reset_session(P). restore_link_estimate(P,e) = reset(P) with Link:=e. Hence restore(take(P)) has the original Link(P) and Screen = reset; restore(default) equals reset_session; after restore, preds=[] (overlay=[], is_displaying=false, pending_bounds=None) and confirmed_epoch=false whatever P was. After reset_session, W = 250 ms.	src/lib.rs::session_switch_forgets_slow_link_rtt, src/lib.rs::a_backgrounded_session_keeps_its_link_estimate, src/lib.rs::restoring_an_estimate_clears_the_outgoing_panes_pixels, src/lib.rs::a_restored_guess_never_inherits_the_other_pane_s_confirmation, src/lib.rs::a_default_estimate_is_the_no_estimate_path, src/lib.rs::the_cold_link_probe_travels_with_the_session_estimate, src/lib.rs::session_reset_restarts_the_cold_link_probe	&mut self, a Copy struct with Option<f32> and private fields, struct construction, call composition (take → reset_session → reset → flush)	0,1,2,4
visibility-model-refinement	refinement	Predictor::{predict_char,reconcile,overlay,note_line_submit,note_unmodelled_input,predict_backspace,reset_session}, aterm_spec::derive::predictive_echo_visibility_model	There is a projection π(P) → PredictiveEchoVisibility state: pending = preds≠[], visible = Vis>0, confirmed = confirmed_epoch, slow = adaptive_slow, fast_streak = fast_samples, fresh = (Link = default), retracted = this step closed adaptive_slow. ∀ P∈Reach(Adaptive), each Predictor step maps to a model action or a stutter: a drained turn with a sample <8 ms → ConfirmFast; the same with preds≠[] → ConfirmFastInFlight; the turn that sets adaptive_slow → ConfirmSlow; predict_char → Key; expiry → Expire; confirming the last pending guess → Echo; every boundary B → Submit; reset_session → SwitchSession. Each model invariant (VisibleNeedsProofAndBenefit, RetractOnlyOnSustainedFastEvidence, FreshSessionHasNoInheritedRtt) then holds of π(P). The model's own negative controls (hand-built bad states rejected by check_invariant) are Tier-0 model checks. Needs a Submit with a carried-pending branch first (§5.5).	tests/conformance_visibility.rs::adaptive_fast_expiry_is_invisible_and_slow_echo_may_display, tests/conformance_visibility.rs::the_display_gate_closes_only_on_sustained_fast_evidence, tests/conformance_visibility.rs::every_epoch_boundary_is_the_models_submit	model→code refinement over a ty_model! (non-scalar projection, stuttering simulation, ConfirmSlow abstracting two concrete turns), closures, &mut self	0,1,2,5,10,13
```

Merge notes:
- `glitch-window` gained gaps 1 and 2 here. It is a `&self` method (gap 2) that reads `srtt: Option<f32>`
  (gap 1). The chunk had listed only 3, 6, 12 and 13.
- `every_epoch_boundary_is_the_models_submit` appeared in the subsumes list of both
  `no-unechoed-flash` and `visibility-model-refinement`. It is a conformance test of the model, so its
  primary is `visibility-model-refinement`. `no-unechoed-flash` is its secondary (§2).
- No two specs stated the same property, so nothing was merged away. The building blocks `Flush`,
  `EndEpoch`, `W` and `Vis` are defined once, in §0, and every spec cites them from there.

## 2. Test → spec map (TSV; all 59)

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
src/lib.rs	a_deferred_wrap_on_the_last_row_declines_without_painting	predict-placement	predict-accept-decline
src/lib.rs	wide_glyphs_are_declined_but_narrow_accents_are_predicted	predict-accept-decline	
src/lib.rs	a_declined_glyph_does_not_erase_the_word_to_its_left	predict-accept-decline	no-unechoed-flash
src/lib.rs	pending_queue_caps_at_max_predictions_across_line_submits	queue-cap	submit-carries-proven-guesses
src/lib.rs	pending_bounds_reserves_wrapped_guesses_without_consuming_them	pending-bounds-hull	predict-placement
src/lib.rs	expire_due_consumes_the_deadline_without_a_redraw	expiry-contract	glitch-window
src/lib.rs	expired_prediction_clears_next_deadline	expiry-contract	visibility-gate
src/lib.rs	expiry_retires_only_the_oldest_guess_and_keeps_the_epoch	expiry-contract	
src/lib.rs	adaptive_local_tail_expires_without_a_visible_erase	expiry-contract	visibility-gate
src/lib.rs	slow_link_expiry_window_grows_until_the_echo_can_confirm	glitch-window	expiry-contract
src/lib.rs	silence_never_opens_the_gate_or_diverges_the_estimate	rtt-only-from-echo	expiry-contract, glitch-window
src/lib.rs	sustained_slow_echo_retires_predictions_and_displays_next	echo-regime-latch	visibility-gate, reconcile-contract
src/lib.rs	adaptive_hides_on_fast_local_echo	echo-regime-latch	visibility-gate
src/lib.rs	adaptive_isolated_scheduler_tail_does_not_open_gate	echo-regime-latch	visibility-gate
src/lib.rs	adaptive_slow_to_fast_closes_only_after_a_sustained_fast_streak	echo-regime-latch	
src/lib.rs	adaptive_gate_does_not_flap_across_a_single_fast_sample	echo-regime-latch	
src/lib.rs	adaptive_gate_never_closes_while_a_guess_is_in_flight	echo-regime-latch	
src/lib.rs	burst_is_classified_by_the_oldest_retired_guess	echo-regime-latch	reconcile-contract
src/lib.rs	always_mode_shows_immediately_without_confirmation	visibility-gate	
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
src/lib.rs	submit_resets_epoch_same_row_no_password_leak	no-unechoed-flash	submit-carries-proven-guesses
src/lib.rs	a_stale_guess_confirming_after_submit_cannot_arm_the_new_line	no-unechoed-flash	reconcile-contract
src/lib.rs	a_scroll_ends_the_epoch_on_the_same_physical_row	no-unechoed-flash	reconcile-contract
src/lib.rs	a_line_terminator_makes_what_follows_tentative	no-unechoed-flash	submit-carries-proven-guesses
src/lib.rs	a_prompt_printed_after_the_proof_on_the_same_row_voids_it	no-unechoed-flash	
src/lib.rs	the_key_seam_sees_a_scroll_the_reconcile_never_did	no-unechoed-flash	
src/lib.rs	session_switch_forgets_slow_link_rtt	link-estimate-swap	glitch-window
src/lib.rs	a_backgrounded_session_keeps_its_link_estimate	link-estimate-swap	
src/lib.rs	restoring_an_estimate_clears_the_outgoing_panes_pixels	link-estimate-swap	visibility-gate
src/lib.rs	a_restored_guess_never_inherits_the_other_pane_s_confirmation	link-estimate-swap	no-unechoed-flash
src/lib.rs	a_default_estimate_is_the_no_estimate_path	link-estimate-swap	
src/lib.rs	the_cold_link_probe_travels_with_the_session_estimate	link-estimate-swap	glitch-window
src/lib.rs	session_reset_restarts_the_cold_link_probe	link-estimate-swap	glitch-window
src/lib.rs	unconfirmed_prediction_self_expires	(none: §4, delete)	
tests/conformance_visibility.rs	adaptive_fast_expiry_is_invisible_and_slow_echo_may_display	visibility-model-refinement	expiry-contract, echo-regime-latch
tests/conformance_visibility.rs	the_display_gate_closes_only_on_sustained_fast_evidence	visibility-model-refinement	echo-regime-latch
tests/conformance_visibility.rs	every_epoch_boundary_is_the_models_submit	visibility-model-refinement	no-unechoed-flash
```

## 3. Coverage

- 59 tests read. 58 map to one of the 19 specs, and 1 is not a spec sample (§4).
- The biggest collapses are `predict-placement`, `echo-regime-latch`, `reconcile-contract`,
  `no-unechoed-flash` and `link-estimate-swap` (7 primary tests each), then `expiry-contract` and
  `unmodelled-input-retires` (4 each).
- Gap histogram (the number of specs each gap blocks; a spec counts once under every gap it lists):

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

> Status 2026-10-15: deleted. `unconfirmed_prediction_self_expires` is removed from `src/lib.rs`.

| test | why | action |
|---|---|---|
| src/lib.rs::unconfirmed_prediction_self_expires | A duplicate. Its setup and steps match `expired_prediction_clears_next_deadline`: Always mode, one guess at (0,0), an overlay at GLITCH_FLOOR_MS+50, then idle. That test also checks the deadline. This one's only extra assertion (overlay(now) has length 1) is `always_mode_shows_immediately_without_confirmation`. `expiry-contract` subsumes both, so this copy adds no sample point. | **delete** (1) |

Action count: delete 1, become-a-spec 0, keep 0.

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
3. **`a_restored_guess_never_inherits_the_other_pane_s_confirmation` does not isolate its claim.**
   It types the guess at (9,0) while `proof_cell` is (0,2). The proof_cell mismatch rule alone keeps
   that guess dark, with or without `restore_link_estimate`'s reset. The sibling test
   `restoring_an_estimate_clears_the_outgoing_panes_pixels` is the real negative control. To pin the
   claim, the test should type at the proven cell (0,2). This does not matter once
   `link-estimate-swap` is proved.
4. **Stale comments describe an epoch-row mechanism.** The comments in
   `fresh_line_resets_confirmation_no_leak` ("row 1 != epoch row 0 → fresh epoch") and
   `submit_resets_epoch_same_row_no_password_leak` ("the epoch-row reset never fires") describe a
   mechanism that no longer exists. Gating now uses `proof_cell` (the exact cell) plus epochs. Both
   tests still pass, through the proof_cell rule.
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
