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
>
> 2026-10-20, predictive editing increment 2's hosts: the GUI and the web hand edit keys to `predict_edit`
> and paint a chain from `overlay_frame` (`edit-overlay`: the overlay and its cursor in one borrow). Wiring
> the web host found that a frame composed after an edit key and before any reconcile of its chain painted
> the whole unchanged line as ghosts (a chain clears `seen` as it starts, and a cell it had not read counted
> as different); such a cell is now taken as the line's verified base shows it, so that frame paints what
> the state changes. A flush now clears the chain's cursor too (`Flush`, `edit-overlay`): a web host
> asking `predict_overlay_cursor` after switching to `off` or a resize, before its next overlay, was
> handed the flushed chain's. Main's `glitch_window_ewma_and_ghost_wire_codec_round_trip` (the integer twins of
> `glitch-window`'s W and of the SRTT step, and a ghost wire codec no spec states) was mapped nowhere; it is
> mapped under `glitch-window`.
>
> 2026-10-20, after review of increment 2 (§5.21): the sweep run over more seeds than the committed one
> found seven breaks, each now a named case of the theorem with its control, and this round's own sweep
> of 800 seeds, run after their fixes, found five more. The predictor gains seven rules: a cursor advance
> is an echo only as far as the run's echoes reach (`CursorReach`), a park in the last column counts only
> at the end of the line (`ParkedAtTheEnd`), a chain is credited whole or not at all
> (`ChainAttributedWhole`), an echo whose cell comes back opaque is no append and voids a standing proof
> (`MarkedEcho`), the transit wait holds only a cursor inside the chain's text (`TransitReach`), an edit op
> is credited only with no later key echoing (`EditOwedAfter`), and a space proves no line by itself
> (`SpaceNeverProves`); `StaleAnchor` also asks the later keys a Backspace cancelled, a cancelled glyph's
> typed owe is dated by the Backspace, a run started inside the known text proves nothing, a carried epoch's
> proof is held to its tail and its mark while a carried guess is pending, and the theorem's decoration rule
> counts a vacated blank under decoration the far end drew. The rows `reconcile-contract`,
> `backspace-pops-own`, `stale-anchor`, `tail-guard`, `standing-tail` and `edit-reconcile`, and §0's
> `Past`, `Attr` and `owe`, state them. No test of `src/lib.rs` was added or removed; one was rewritten
> (`a_cancelled_glyph_anchors_the_next_on_its_cell`, its control on a repeated letter, since two spaces
> now prove nothing) and five moved to presses that hand the row (the park needs the end of the line).
>
> 2026-10-20, predictive editing increment 3 (edits that move the tail): a glyph typed inside the known
> text, and a Backspace there, are SHIFTS, ops of a state chain whose states move the text right of the
> cursor (`shift-ops`, `edit-fold`), each shown only once a shift of its kind has echoed telling itself
> apart from an edit in place (`edit-class-proof`); a shift no edit in place can be told from leaves the
> line unknown. A chain's overlay names each cell's kind (`GhostCell`: typed, moved from a column, vacated)
> for aterm-core's span painter (`edit-overlay`). A chain on a line proven beside text past its end holds
> that text where it was (`standing-tail` (d)). A glyph that makes its row reorder, or one typed on such a
> row, is declined (`right-to-left-rows`). The theorem's world gains readline's overwrite mode, zle with an
> autosuggestion plugin, a right-to-left letter and the SHIFT motif; two hundred seeds found two breaks
> (§5.22, §5.23), and the same seeds hold after their fixes. Nine unit tests are added and one renamed. The
> plan's tail adoption is not implemented: no evidence the predictor reads tells a tail from a suggestion
> (§5.24).
>
> 2026-10-20, after review of increment 3 (§5.25, §5.26): a shift's kind is proven, and a chain's
> retirement credited, by the IN-PLACE WORLDS (`shift-ops`, `edit-reconcile`): the chain's keys folded
> from the verified line with each unproven kind edited in place, as overwrite mode edits, which
> replaces the per-op `Evident` test (a burst whose intermediate state no frame showed proved both kinds
> on overwrite mode's echo) and refuses credit where an in-place world shows the screen with fewer keys
> read (`Rule::InPlaceReadsFewer`). A glyph and a Backspace cancel only with no shift of an unproven
> kind in flight (`backspace-pops-own`, `Rule::CancelsOnlyKnownShifts`). Every press that starts a chain
> holds the standing proof to its row first, whatever is pending (`standing-tail` (b)). A declined
> arrow and a key not modelled make a blank row no evidence of the line's end for a window
> (`tail-guard`, `unmodelled-input-retires`). The run_inside guard is `Rule::InsideRunsNeverProve`. A
> sweep of two thousand seeds then found two breaks on main before the increment, fixed with their
> named cases: a proof stands only while its echo's cell holds its glyph (`standing-tail` (e),
> `Rule::StandingGlyph`), and a cancelled glyph's erase owes its echo as a space
> (`backspace-pops-own`, `Rule::CancelOwesItsBlank`), and run whole those cases found a state an
> earlier op's echo shows part-way taken for a later state (`edit-reconcile`, `Rule::PartWayEchoes`);
> and two places the theorem's world claimed too much: its unprompted right prompt and suggestion drawn
> without the decoration tag, and its no-echo read redrawing the secret buffer. The edit-overlay row now
> says what `seen` holds when a chain starts (its press's row, not nothing). Twelve unit tests are added.
>
> 2026-10-20, predictive editing increment 4 (line-edge keys at an authenticated prompt): Home, End, Ctrl-A,
> Ctrl-E and Delete are LEARNED keys, ops of a state chain placed only on a WITNESSED line, one started at the
> very cell the shell's authenticated OSC 133 `B` names as its buffer's start (`line-edge-witness`,
> `KeySample::input_start`, aterm-core's `Terminal::input_region_start`), and shown only once two credited
> echoes of the key's canonical effect have taught its binding in the editor context (`learned-bindings`,
> `EditorContext`, carried with the session in its `LinkEstimate`): until then each is a dark PROBE, and a
> divergence or an expiry it heads unlearns it, two banning it. Ctrl-A and Ctrl-E under a Kitty mode that
> sends them as `CSI u` are not modelled (R-KITTY), and End at the end of the text is declined with its
> pixels kept (`Rule::EndRefusal`: an autosuggestion is accepted there). The rows `edit-key-routing`,
> `edit-fold`, `edit-reconcile`, `edit-expiry`, `edit-class-proof`, `visibility-gate` and
> `link-estimate-swap`, and §0's `Link`, `Vis` and the op list, state them. Nine unit tests are added.
> The safety theorem's world gains aterm's shell integration (signed marks, the nonce gate on), three
> keymaps, a shell re-executing under a new nonce, a forged mark, Kitty's mode pushed and popped, and a
> program's prompt where the shell's stood; each new rule and four new host mutants are caught at named
> cases, and on its seed FRAME FIDELITY compared 4813 whole frames (1568 before; the 1614 first written here was
> no count any tree gives), 144 of them showing a learned key (180 frames showed one and held the chain's clauses;
> the rest repeated a frame or fell out of the clause's scope).
> A sweep of three hundred seeds then found two breaks, each a named case with its control: a line edge's
> echo caught part-way (End reprints the line a cell at a time) matched a later `←` unread (`edit-reconcile`,
> `Rule::PartWayEchoes`), and, on main before this increment, a zero-width mark a forced wrap carried onto
> the next guess's cell left that guess painted plain where it settled marked (`seen-opacity`,
> `reconcile-contract`, `Rule::OpaqueHead`). Three more were the world's own over-claims, corrected there (a
> key read by a Kitty mode pushed after its press, a zero-width mark stepped over with `ESC [ 0 C`, an ICH
> in front of a mark). Four hundred seeds after the fixes found no break. The model gains the witness and
> the learned binding (`visibility-model-refinement`), bound by one conformance test.
>
> 2026-10-20, after review of increment 4 (§5.27): a `B` vouches for the buffer it found empty, and only the
> FIRST KEY typed after it meets that buffer (`line-edge-witness`, `prompt_gate`, `Rule::WitnessFirstKey`): a line
> terminator pressed at an authenticated prompt opens the gate (`note_line_submit_at`, the prompt from aterm-core's
> `Terminal::prompt_in_front`), every key shuts it, and a run started at the `B`'s cell is witnessed only as the
> first key at another prompt (`KeySample::input_start` names its prompt, `InputStart`). aterm-core names a start
> only where the `B` says it stands where the buffer starts (`aterm_input=start`), which zsh's, sent after zle drew a
> line pre-filled from its buffer stack, does not. A control chord the keyboard mode escapes is refused whether
> Kitty's `CSI u` or modifyOtherKeys level 2's `CSI 27` (R-KITTY, `KeySample::escaped_chords`). The model gains
> the gate (`SubmitAtPrompt`, `WitnessIsTheFirstKey`). Four unit tests are added and one rewritten (it now tests
> `reset_session` itself, DECCKM and the escaped chords as contexts).
>
> 2026-10-20, the same review's sweep of two thousand seeds (§5.27 (e)–(i)): THE FLOOR (`standing-tail` (f),
> `Rule::FloorBelow`, `KeySample::below`), DARK BEHIND A WAIT (`visibility-gate`, `Rule::DarkBehindAWait`), THE
> CREDIT TAKEN BACK (`reconcile-contract`, `Rule::CreditTakenBack`) and the engine's wrap flag (`predict-placement`,
> `Rule::EngineWrapFlag`, `KeySample::wrap_pending`) close five breaks found on main before the increment and one
> its world made reachable; a sixth was the world's own (a prompt drawn over a wrapped line's row). A second sweep
> (seed indices 0 to 2400) found three more: the floor beside a wrapped typed-ahead guess, an engine bug (a marked
> blank lost from its row by EL, fixed in aterm-grid) and the world's zle erasing before a mark. A third
> (the same indices) found one more, the floor excusing any text on a cell a wrapped guess names (§5.27 (m)): a
> guess excuses its own glyph there now, never another; a fourth found none. Six unit tests are added and the Delete test rewritten to
> show the shift; `CancelOwesItsBlank` keeps no clause of its own in the theorem now (§5.27 (g)).
>
> 2026-10-20, predictive editing increment 5 (the measurement, `tests/editing_win.rs`, numbers in
> `docs/DESIGN-predictive-editing-2026-10-20.md`): the region design's 300-line corpus, ported and checked
> against its recorded size, typed through the safety theorem's world (`tests/common/world.rs`, now mounted
> once by the suite for both files) over a split-read link, with the theorem's checker on for every variant
> with every rule in force. It found that THE FIRST KEY's gate shut at every coordinate move, so no line was
> witnessed at a shell on the bottom row, whose Enter scrolls the screen before the next prompt's first key
> (11 of 300 lines witnessed at 1 to 160 ms, 10 at 320 ms and 7 at 600 ms, none after the screen filled,
> signed, Adaptive): a move keeps the gate as it stood (`line-edge-witness`, `Rule::ScrollKeepsFirstKey`,
> `a_scroll_after_a_line_terminator_keeps_the_first_key_s_gate`), and 282 of 300 are witnessed at 1 to
> 160 ms, 260 at 320 ms and 153 at 600 ms (the measurement prints both, a run each, beside each other);
> the theorem's committed sweep and a sweep of seed indices 0 to 2400 hold with it. `PredictEvents` gains
> four counts that decide nothing (`ops_folded`, `flushes`, `epoch_ends`, `expiries`), which the
> measurement reads as the cost and the churn; `ops_folded` counts the walk a press makes to place its key
> (`placeable`) too. One unit test is added; the measurement's three tests are mapped in §2 and its corpus
> check is in §4.
>
> Its review (2026-10-20) found the formal model still said a scroll shuts the gate (its `Submit`, to which
> the conformance binding mapped the rows moving): the model gains `RowsMoved`, which keeps it, and
> `a_scroll_keeps_the_first_key_s_gate` binds it with `Rule::ScrollKeepsFirstKey` off as its control
> (`visibility-model-refinement`, `reconcile-contract` (1)). Its sweep past seed index 2400 found three
> breaks on main, none of the increment's making, each now a rule, a named case (`review_5_cases`) and a
> unit test: the transit wait took any blank for an echo part-way, and zsh's `reset-prompt` redrawing the
> prompt narrower had a chain painted twice (FRAME FIDELITY, seed index 2482, `Rule::TransitBlanks`,
> `edit-reconcile`); a Delete's echo split on its step back stood the cursor on End's state, then Home's,
> each credited unread (ATTRIBUTION, seed index 2748, `Rule::PartWayStepBack`, `edit-reconcile`); and a
> Backspace cancelling a stale guess left its cell as a fresh anchor a cell right of the erase's return
> (FIDELITY, seed index 3942, `Rule::StaleCancel`, `stale-anchor`). `Rule::CancelsOnlyKnownShifts` keeps no
> clause of its own in the theorem since (`Rule::TransitBlanks` closes its named case too); its unit test
> switches both off. The world's press reads a row for an edit key only with the edit lane on, as
> aterm-gui does (`Sim::set_editing`), so the measurement's row cost of the glyph lane alone is 40.9 cells
> a press, not 50.9. Three unit tests and one conformance test are added.
>
> 2026-10-20, the comparison at 30, 100 and 250 ms (the docs' §3.1): the measurement gains those links and
> counts mispredicted cells and their lifetimes. A cross-check driving the predictor at 3d4c211fbb, the
> commit before the plan's first landing (feb341a9ea), through the measurement's world found `Baseline`
> (every rule off) showing what that predictor never did: a glyph typed over text (`sudo ` after Home or
> Ctrl-A) waited where it flushed, 22 wrong episodes at 80 ms, Adaptive, where it showed 9. The wait, added
> beside a suggestion by the review of phase 0 (0376b47967), was the one change to what the glyph lane shows
> that no rule switched off: it is `Rule::ScreenAsItWas` now (`reconcile-contract` (3)), keeping no clause,
> and with it off `Baseline` prints every metric the cross-check printed, in all 64 of its runs. One unit
> test is added.

Source: `crates/aterm-predict` (read only). As of 2026-10-20 `src/lib.rs` has 155 `#[test]` fns,
`tests/conformance_visibility.rs` has 8 and `tests/editing_win.rs` 4, for 167 in all (the 2026-10-15
recovery read 56 and 3). The recovery read every test and the code it reaches; the rows added since were
written with the code they state, and the tests named in them ran.

Result: **37 specs** cover **162 tests**. The one test §4 found to sample no spec is deleted; the one §4
lists now pins the measurement's harness, not the crate.

Gap numbers refer to the gap table in
the historical development audit `docs/AUDIT-trust-for-test-migration-2026-10-15.md` §7.
Gap 0 (an installed seal that publishes Clean rows) is a prerequisite for every spec, so the column
lists it on every row. No spec here is blocked by gap 0 alone.

## 0. Notation used in the statements

- Test ids are crate-relative: `src/lib.rs::name`, `tests/conformance_visibility.rs::name` and
  `tests/editing_win.rs::name`.
- `P` is a `Predictor`. Its fields are as in `src/lib.rs`: `mode`, `preds` (a `VecDeque<Prediction>`
  with the oldest at the front), `srtt`, `expiry_backoff`, `slow_samples`, `fast_samples`,
  `adaptive_slow`, `confirmed_epoch`, `proof_cell`, `proof_echo`, `proof_line_end`, `epoch`,
  `grid_cols`, `deferred_wrap`, `anchor_lost`, `grandfathered_epoch`, `coords`, `owed_at`,
  `declined_tail`, `next_seq`, `cancelled_at`, `cancelled_pair`, `glyph_owes`, `tail`, `tail_cells`, `tail_line_end`,
  `line_end_at`, `moved_tail_at`, `line` (the known line: `row`, `lo`, `epoch`, `text`, `cursor`,
  `vacated`, `end_known`, `fresh`, with `hi = lo + |text|`), `chain`, `proven`, `carried_proven`, `carried_proof`, `editing_off`, `edit_capable`
  `run_stale`, `run_inside`, `run_witness`, `prompt_gate`, `bindings` and `proof_opaque`. A guess `g` has `row`, `col`, `ch`, `born`, `epoch`, `owed`, `seq`, `op` (Insert,
  Left, Right, Erase, LineStart(k), LineEnd(k) or Delete, k a learned key), `class` (Glyph, Motion, InsertShift, EraseShift, LineEdge
  or DeleteFwd), `stale`, `ghost` (Typed for every
  pending op; a chain's overlay cell is Typed, `Moved { from }` or Vacated, edit-overlay) and `probe` (learned-bindings). `Engaged(P) ⇔ chain ∧
  preds ≠ []` (a state chain is in flight; `fn engaged`).
- `Void(P)`: `confirmed_epoch := false`, `proof_cell := None`, `proof_echo := None`,
  `proof_line_end := false` (the private `fn void_proof`; the four move together), `proven := 0`
  (`Rule::ProofsEndWithTheEpoch`), and `line := None` unless `Engaged(P)` (edit-line-lifetime).
- `After(P,g) = (g.row, if g.col+1 ≥ grid_cols then g.col else g.col+1)`, the cell an echo of `g`
  leaves the cursor in (`fn cell_after`).
- `Past(P,cur,g)` (`fn echoed_past`): `cur = Some((r,c))` with `r = g.row ∧ c > g.col ∧ Reach(r,c)`; or
  `(r,c) = (g.row,g.col)` with `g` in the last column and `tail_line_end` (`Rule::ParkedAtTheEnd`; off:
  any run); or `r > g.row` with a guess of `preds` on row `r` and `Reach(r,c)`, or `(r,c) = (g.row+1, 0)`
  with `g` or a guess of `preds` on `g.row` in the last column. `Reach(r,c)` (`fn within_reach`,
  `Rule::CursorReach`; off: true): `c ≤ After(h).col` for some `h ∈ preds` on row `r`, or for the
  `cancelled_pair`'s cell on row `r` (its glyph's echo lands before its erase's).
- `predict_char*` means both `predict_char` and `predict_char_in_grid`.
- `Reach(m)`: the predictors reachable from `Predictor::new(m)` by any sequence of public calls with
  non-decreasing `now` arguments. The public calls are `set_mode`, `predict_char*`,
  `predict_backspace`, `note_line_submit`, `note_unmodelled_input`, `reconcile` with any `observe`
  closure, `reconcile_seen`, `predict_glyph`, `predict_edit`, `set_editing`, `overlay`,
  `overlay_cursor`, `expire_due`, `reset`, `reset_session`, `take_link_estimate`, and
  `restore_link_estimate(e)` where `e` came from `take_link_estimate` or `LinkEstimate::default()`.
- `Mono(P)`: `g.born` does not decrease along `preds`. Every `P ∈ Reach` satisfies it, because `now`
  is non-decreasing.
- `Link(P) = (srtt, slow_samples, fast_samples, adaptive_slow, expiry_backoff, owed_at, bindings)`, the
  fields that `LinkEstimate` carries. `Screen(P)` is every other field.
- `Flush(P)`: `owe(max of g.born over preds and of the typed owes' born)` when either is non-empty,
  then `preds := []`, `glyph_owes := []`, `Void(P)`, `anchor_lost := false`,
  `grandfathered_epoch := None`, `declined_tail := 0`, `cancelled_at := None`, `cancelled_pair := None`, `line_end_at := None`,
  `chain := false` (so `Void` drops `line`), `carried_proven := 0`, `ghost_cursor := None`, with every other field kept. This
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
  owe `now` (one that pops our own guess owes no echo of any key, but the popped glyph's own TYPED and
  dated by the Backspace, `now`, `Rule::CancelOwes`: backspace-pops-own); a guess retired without credit owes its `born`, typed when
  it was an append (`Appended`, tail-guard), else as any echo (typed-owes); an expired guess owes its
  `born` (expiry-contract).
  `reset_session` sets `owed_at := None`; `take_link_estimate` carries `max(owed_at, max g.born, max
  typed-owe born)` out and `restore_link_estimate(e)` installs `e`'s. Each armed guess takes
  `g.owed := owed_at` and `g.seq := next_seq`, then `next_seq += 1`.
  `Owed(P,now) ⇔ owed_at=Some(t) ∧ now.saturating_duration_since(t) ≤ W(P)` (the predictor's newest
  owe of any echo, which the model's `owed` projects; typed owes project to nothing). `Attr(P,now,h) ⇔
  ¬(h.owed=Some(t) ∧ now.saturating_duration_since(t) ≤ W(P)) ∧ ¬∃ (seq,ch,t) ∈ glyph_owes: seq < h.seq ∧
  ch = h.ch ∧ now.saturating_duration_since(t) ≤ W(P)` ∧ (h.stale ⇒ ¬Owed(P,now) ∧ ¬∃ pending Insert
  g' with g'.seq > h.seq ∧ g'.ch = h.ch ∧ ¬∃ (seq,ch,t) ∈ glyph_owes: seq > h.seq ∧ ch = h.ch ∧ t inside
  W(P)) ∧ (h.op ≠ Insert ⇒ ¬(owed_at=Some(t) ∧ t inside W(P) ∧ (h.owed=None ∨ t > h.owed)))`, for a guess
  of any epoch: only an owe from before `h`, a typed one only for the same glyph, for a STALE ANCHOR
  (stale-anchor) the keys after it too, pending or cancelled, and for an edit op (`Rule::EditOwedAfter`)
  any key after it still echoing, whose echo can make its cursor and blanks by itself.
- `Vis(P) = visible_count(P)`: `Off → 0`; `Always →` the length of the leading run of `preds` with `¬probe`; `Adaptive ∧ ¬adaptive_slow → 0`;
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
predict-placement	contract	Predictor::{predict_char_in_grid,predict_char}	predict_char(ch,cur,g,cols,now) = predict_char_in_grid(ch,cur,g,(cols,0),now). For mode≠Off and cols>0, first observe_coordinates(g), set grid_cols:=cols, and apply CancelCursor(P,Some(cur),now), including on an idle host that skipped reconcile. After that guard and the acceptance filter (predict-accept-decline): let A = the last g∈preds with g.epoch=P.epoch, and C = cancelled_at (taken by the call: backspace-pops-own). base = (A.row, A.col.saturating_add(1)) if A exists; else C if C is set; else (cur.row, cols) if deferred_wrap ∧ cur.col+1=cols; else cur. The proof check compares proof_cell with C when C anchors the guess, else with cur. base.col<cols ⇒ the guess goes at base; otherwise at (base.row+1, 0) when base.row+1<rows, and the call declines (false, preds unchanged) when base.row+1≥rows. On accept it pushes {row,col,ch,born:now,epoch:P.epoch} at the back, returns true, and sets grid_cols:=cols; when A does not exist (the guess anchors on the live cursor) it first takes the run's tail (key-row-sample). predict_glyph(ch, s, now) is the same call on s.cursor, s.coords and s.grid, carrying s.row. Consequences: type-ahead extends the previous guess and does not land on the live cursor; a height-blind host (rows=0) declines at the margin and keeps the armed guesses; a parked deferred-wrap cursor wraps the next glyph; a free last column is predicted in place. The deferred-wrap park at a press is the engine's own flag s.wrap_pending when the host hands it (`Rule::EngineWrapFlag`; off, or None: deferred_wrap, the last reconcile's reading of a filled last column): a line editor that moved its cursor onto its own text in the last column (overwrite mode, `→`) has wrapped nothing, and the next glyph replaces that cell.	src/lib.rs::types_ahead_extends_from_last_guess_before_echo, src/lib.rs::height_blind_hosts_decline_at_the_margin_without_flushing, src/lib.rs::wraps_to_the_next_row_instead_of_flushing, src/lib.rs::bottom_row_declines_without_flushing, src/lib.rs::the_deferred_wrap_slot_is_not_a_free_cell, src/lib.rs::a_free_last_column_is_still_predicted_in_place, src/lib.rs::a_deferred_wrap_on_the_last_row_declines_without_painting, src/lib.rs::a_cancelled_pair_does_not_anchor_after_an_unrelated_cursor_move, src/lib.rs::a_backspace_cancels_no_glyph_of_an_unproven_shift, src/lib.rs::a_cancelled_glyph_s_erase_is_no_space_s_echo, src/lib.rs::a_cursor_on_text_in_the_last_column_is_not_parked	&mut self, VecDeque iter().rev().find(closure), push_back, saturating u16, match with guards on Option, tuple destructuring	0,1,2,3,5,7,13
predict-accept-decline	contract	Predictor::{predict_char_in_grid,decline,set_ambiguous_width_double}, aterm_grapheme::{char_width,char_width_cjk}	For mode≠Off and cols>0, after observe_coordinates and `if preds=[] then anchor_lost:=false`: the call declines ⇔ ch.is_control() ∨ w(ch)≠1 ∨ anchor_lost ∨ |preds|≥MAX_PREDICTIONS ∨ (preds≠[] ∧ no g∈preds has g.epoch=P.epoch, i.e. only a submitted line's guesses are in flight) ∨ the wrap reaches the bottom edge, w being char_width_cjk after set_ambiguous_width_double(true) (the width the grid writes East Asian Ambiguous glyphs with) and char_width otherwise; changing that flag flushes. Every decline owes `now`. A declined control char is Flush. A declined printable char leaves preds unchanged, applies EndEpoch, sets anchor_lost:=true, declined_tail:=declined_tail+1 (saturating) and returns false. An accepted guess is pushed with owed:=owed_at and seq:=next_seq (then next_seq+1), and sets declined_tail:=0. While anchor_lost ∧ preds≠[], every later predict_char* declines with preds unchanged; the first predict after preds drains is judged afresh. cols=0 ⇒ false, P unchanged. While Engaged a glyph is arm_in_chain's instead: armed as an Insert at the chain's cursor when Placeable, the glyph one cell wide and making no row reorder, hi+2 ≤ cols (R-ROW, in ℕ: the code checks the sum, so no saturation at u16::MAX admits the last column) and |preds| < 64: of the glyph class at the chain's text end, a SHIFT inside it (shift-ops); one inside the text the chain cannot place is owed and Flushed (`Rule::MidLineDeclineFlushes`); otherwise declined as above. Not Engaged, a glyph typed with the known line's cursor inside its text is a shift where arm_inside places one (shift-ops), else it Voids first, dropping the line (edit-line-lifetime). A glyph that makes its row reorder, or one whose press row holds such a glyph, is declined before either (right-to-left-rows). A run anchored on the live cursor while Owed carries `stale` (stale-anchor).	src/lib.rs::wide_glyphs_are_declined_but_narrow_accents_are_predicted, src/lib.rs::ambiguous_glyphs_are_measured_as_the_terminal_writes_them, src/lib.rs::changing_the_ambiguous_width_flushes_guesses_placed_under_the_old_one, src/lib.rs::a_declined_glyph_does_not_erase_the_word_to_its_left, src/lib.rs::a_declined_glyph_owes_its_echo, src/lib.rs::an_edit_chain_does_not_saturate_its_last_column_guard, src/lib.rs::nothing_is_armed_behind_a_terminator_while_its_line_is_in_flight	&mut self, char::is_control, cross-crate width table (aterm_grapheme::char_width), VecDeque len	0,1,2,3,4,5,12
queue-cap	invariant	Predictor::predict_char_in_grid; every public &mut method	∀ m, ∀ P∈Reach(m): |P.preds| ≤ MAX_PREDICTIONS (256). In particular predict_char* with |preds|=256 returns false and keeps |preds|=256 whatever epochs the guesses carry. Only predict_char_in_grid (with arm_in_chain, its branch while a chain is in flight) and predict_edit grow preds, by at most 1 and only when |preds|<256; a chain is capped further at EDIT_CHAIN_MAX = 64 ops (R-CAP, edit-key-routing).	src/lib.rs::pending_queue_caps_at_max_predictions	&mut self across every public method, VecDeque length; reconcile/expire have while loops that only pop	0,1,2,5,7
pending-bounds-hull	contract	Predictor::pending_bounds	pending_bounds(P)=None ⇔ preds=[]; otherwise Some((min g.row, min g.col, max g.row+1, max g.col+1)) over g∈preds, computed in usize so nothing saturates. While Engaged the hull also covers line.row from line.lo to one past the furthest text end any state reaches (edit-overlay). It is &self: P, next_deadline and |preds| are unchanged. After reset (or any Flush) it is None.	src/lib.rs::pending_bounds_reserves_wrapped_guesses_without_consuming_them	iterator map + reduce over a VecDeque, usize::from(u16), min/max, Option of a 4-tuple	0,1,3,5,7,13
expiry-contract	contract	Predictor::{expire,expire_due,overlay,next_deadline,is_displaying}	∀ P, now: expire pops the maximal prefix of preds satisfying Exp(P,now,·), with W evaluated once before the loop. For each popped g, in order: owe(g.born); h.owed := max(h.owed, Some(g.born)) for every h left in preds; and if g.epoch = P.epoch, Void (`Rule::ExpiryOwes`: the expired key's late echo lands on the cell the proof names, ahead of anything typed next); and line_end_at := None whatever the rule (its echo, late or never, moves the cursor where no guess says). If it pops ≥1, expiry_backoff' = min(expiry_backoff+1, 3); every other field is untouched (epoch, grandfathered_epoch, srtt, slow/fast_samples, adaptive_slow). With `Rule::ExpiryOwes` off it pops and owes and voids nothing (the pre-2026-10-17 contract). This is the glyph lane; an Engaged chain expires whole (edit-expiry). expire_due(now) = (expire popped ≥1). overlay(now) = expire, then preds[..Vis]. next_deadline(P) = preds.front().map(|g| g.born + W(P)). Under Mono, after expire(now): next_deadline is None or ≥ now (not > now; see §5.1), and a second expire_due(now) is false. is_displaying(P,now) ⇔ preds≠[] ∧ (Exp(P,now,front) ∨ Vis(P)>0), and it is &self. Corollaries: an Adaptive guess hidden by the gate expires with overlay=[] before and after the deadline; after a current-epoch guess expires, the guesses behind it and the next guess at the live cursor are dark until an echo proves the epoch again.	src/lib.rs::expire_due_consumes_the_deadline_without_a_redraw, src/lib.rs::expired_prediction_clears_next_deadline, src/lib.rs::expiry_retires_only_the_oldest_guess_owes_it_and_voids_the_epoch, src/lib.rs::without_expiry_owes_an_expiry_keeps_the_proof_as_it_did, src/lib.rs::adaptive_local_tail_expires_without_a_visible_erase	&mut self, while-let popping a VecDeque front, Instant::saturating_duration_since and Instant+Duration (cross-crate opaque type), Duration compare, saturating u8, make_contiguous + slice get returning a borrow tied to &mut self	0,1,2,3,7,12
glitch-window	contract	Predictor::glitch_window	Let f = min((250 << min(expiry_backoff,3)) as f32, 2000.0). W(P) = Duration::from_millis(x as u64), with x=f when srtt=None and x=clamp(3·s, f, 2000) when srtt=Some(s). f ∈ [250,2000], so the clamp never panics (min ≤ max). ∀ P∈Reach: expiry_backoff ≤ 3 ∧ 250 ms ≤ W(P) ≤ 2000 ms. With srtt fixed, W is non-decreasing in expiry_backoff; with srtt=None it is 250·2^backoff ms.	src/lib.rs::slow_link_expiry_window_grows_until_the_echo_can_confirm	&self, Option<f32>, u64 shift by a u8, u64→f32 and f32→u64 casts, f32 min/clamp (panic precondition), Option::map_or with a closure, Duration::from_millis	0,1,2,3,6,12,13,f32
rtt-only-from-echo	invariant	Predictor::{record_rtt,reconcile,expire,record_echo_regime}	srtt is written only by record_rtt, called only when reconcile confirms a guess: srtt' = ms(d) if srtt=None else 0.7·s + 0.3·ms(d), and expiry_backoff' = 0. slow_samples, fast_samples and adaptive_slow are written only by record_echo_regime (once per reconcile turn that confirms ≥1 guess) and by reset_session / take_link_estimate / restore_link_estimate. Hence ∀ trace from Predictor::new(m) with no confirming reconcile: srtt=None, adaptive_slow=false, confirmed_epoch=false and, in Adaptive, Vis=0. A link that never echoes is never painted, and timeouts never feed the estimate.	src/lib.rs::silence_never_opens_the_gate_or_diverges_the_estimate	trace-level frame condition over every &mut method, f32 EWMA, Duration::as_secs_f32	0,1,2,5,7,f32
echo-regime-latch	contract	Predictor::{record_echo_regime,reconcile}	Each reconcile that confirms ≥1 guess calls record_echo_regime exactly once, on the turn sample (the first confirmed guess's latency, not the newest). record_echo_regime(d), x=ms(d): x ≥ 20 ⇒ fast:=0, slow:=min(slow+1,2), and adaptive_slow:=true if slow≥2. x < 8 ⇒ slow:=0, fast:=min(fast+1,3), and adaptive_slow:=false if fast≥3 ∧ (srtt=None ∨ srtt<8) ∧ preds=[], else unchanged. 8 ≤ x < 20 ⇒ no change. Consequences ∀ P∈Reach: adaptive_slow goes false→true only on the second consecutive decisive slow turn, and true→false only on a turn with ≥3 consecutive decisive fast turns, smoothed srtt<8 ms and nothing pending; a dead-band sample resets neither streak; a burst retired in one turn counts as one sample.	src/lib.rs::sustained_slow_echo_retires_predictions_and_displays_next, src/lib.rs::adaptive_isolated_scheduler_tail_does_not_open_gate, src/lib.rs::adaptive_slow_to_fast_closes_only_after_a_sustained_fast_streak, src/lib.rs::adaptive_gate_does_not_flap_across_a_single_fast_sample, src/lib.rs::adaptive_gate_never_closes_while_a_guess_is_in_flight, src/lib.rs::burst_is_classified_by_the_oldest_retired_guess	&mut self, f32 compare and EWMA, saturating u8 with min, Option::get_or_insert in the reconcile loop, Option::is_none_or closure	0,1,2,3,5,7,13,f32
visibility-gate	contract	Predictor::{overlay,visible_count}	∀ P, now: overlay(P,now) = (expire(P,now); P.preds[..Vis(P)]). Vis(P) ≤ |preds| always, so the get(..) never takes its unwrap_or branch. The result is a leading run of preds in type order. Always mode shows every pending guess at once, with no confirmation needed, up to the first PROBE (learned-bindings: a binding nobody has seen is no guess). While Engaged, overlay returns the chain's ghosts instead (edit-overlay). DARK BEHIND A WAIT (`Rule::DarkBehindAWait`; off: as before): in Adaptive, with held_wait = Some(s) (reconcile-contract) and the guess of seq s pending at index i, Vis := min(Vis, i+1): nothing typed after a guess held so shows until it retires; held_wait is recomputed by every glyph-lane turn.	-, src/lib.rs::nothing_shows_behind_a_guess_held_on_its_own_glyph	enum match with guards, take_while + count with a closure, VecDeque::make_contiguous, slice get with a range, a returned borrow	0,1,2,3,7,13
reconcile-contract	contract	Predictor::{reconcile,reconcile_seen,observe_coordinates,observe_cancelled_cursor,confirm,retire_echoed,echoed_past}	reconcile(P,cur,g,alt,now,obs) = reconcile_seen(P,cur,g,alt,now,obs') with obs'(r,c) = obs(r,c).map_or(Blank, |ch| Seen::of_cell(ch,false)) (seen-opacity); reconcile also sets edit_capable:=false and first Flushes an Engaged chain (a host that cannot say which cells are opaque runs none), reconcile_seen sets edit_capable:=true; while Engaged, steps (3) and (4) are edit-reconcile's instead; below, obs(r,c) is obs'(r,c).legacy(). ∀ closures obs. (1) P.coords=Some(c) ∧ c≠g ⇒ P:=reset(P) with prompt_gate kept as it stood (`Rule::ScrollKeepsFirstKey`; off: reset's Shut; line-edge-witness); then coords:=Some(g). (2) alt ⇒ Flush, deferred_wrap:=false, return. (3) Repeatedly, h=front: obs'(h.row,h.col) = Opaque(None) ⇒ (`Rule::OpaqueHead`) opaque_refusals+1, Flush, stop (seen-opacity); obs(h.row,h.col)=Some(h.ch) ∧ Past(P,cur,h) (`Rule::CursorAdvance`; off: the glyph alone), or h.ch=' ' ∧ obs(h.row,h.col)=None ∧ cur=Some((h.row,c')) with c'>h.col ∧ Reach(h.row,c') ⇒ pop; if Attr(P,now,h) then record_rtt(now−h.born) and, iff h.epoch=P.epoch: when Appended(P,h) (tail-guard), line_end_at:=Some(After(h)) if tail_line_end, then, unless h.ch=' ' ∧ ¬confirmed_epoch (`Rule::SpaceNeverProves`: a space's echo, a blank with the cursor past it, is what any output stepping over a blank draws, a prompt's trailing space among them, so it proves no line by itself and only carries a proof on), confirm(h) with proof_opaque := (obs'(h.row,h.col) is Opaque); otherwise moved_tail_at:=max(moved_tail_at, h.born) and Void. If not Attr: Void when h.epoch=P.epoch (`Rule::UncreditedVoids`); when Appended(P,h), line_end_at:=Some(After(h)) if tail_line_end ∧ h.epoch=P.epoch, else moved_tail_at:=max(moved_tail_at, h.born); and h is owed typed or as any echo (typed-owes: glyph_owes.push((h.seq, h.ch, h.born)) when Appended, else owe(h.born) and g.owed:=max(g.owed,Some(h.born)) for every g left in preds). obs(h.row,h.col)=Some(h.ch) ∧ ¬Past ⇒ stop (held: the glyph was there before its echo, an autosuggestion the typed glyph matches); obs(h.row,h.col)=Some(c≠h.ch) with Some(c) = T?(h.row,h.col).legacy() (tail-guard) ∧ cur=Some((h.row,h.col)) ⇒ stop (`Rule::ScreenAsItWas`; off: Flush and stop, as before the review of phase 0) (the cell as the run's press saw it, not yet echoed: a glyph typed onto a suggestion it diverges from); any other Some(c≠h.ch) ⇒ Flush and stop; otherwise stop. (3b) THE STANDING MARK (`Rule::MarkedEcho`): confirmed_epoch ∧ ¬proof_opaque ∧ proof_echo unchanged by this turn ∧ obs'(proof_echo's cell) Opaque, the cell read once a turn before anything retires (shared with the standing glyph, standing-tail (e)) ⇒ moved_tail_at := max(·, its born), Void (a zero-width mark the far end attached to the proof's echo after the frame that proved says the run was typed before it); a press continuing the proof reads its own row the same way. Then the standing tail (standing-tail). (4) preds≠[] ∧ cur is neither Some((front.row,front.col)) nor the deferred-wrap slot of a wrapped front (front.col=0 ∧ grid_cols>0 ∧ cur=Some((front.row−1, grid_cols−1))) ⇒ Flush. (5) CancelCursor(P,cur,now), so an empty queue with an incompatible or unknown cursor owes possible late pair output and loses its retained anchor/proof. (6) deferred_wrap := cur=Some((r,c)) ∧ grid_cols>0 ∧ c+1=grid_cols ∧ (obs(r,c).is_some() ∨ (c>0 ∧ obs(r,c−1)=Some(l) ∧ w(l)=2)), w the width of predict-accept-decline: a wide glyph whose lead fills column c−1 parks the cursor on its continuation, which obs reads as blank. confirm(h) sets confirmed_epoch:=true, proof_cell:=Some(After(h)), proof_echo:=Some(h) and proof_line_end:=tail_line_end iff h.epoch=P.epoch, else does nothing. Corollaries: a wrong glyph or the alternate screen empties preds; an echoed space confirms by cursor advance; a glyph already on screen before its key's echo (a matched suggestion) neither retires nor measures until the echo moves the cursor past it, and a cursor moved down by other output (a submit's) is not past a guess whose run does not reach that row, nor one moved right of every echo of the run (an autosuggestion drawn with its cursor not yet stepped back, which the cursor check then flushes); a cursor standing on a mid-line run's last-column guess (the line's own tail pushed there) is no park; a scroll (changed g) retires every guess before any glyph is compared, so a moved cell never confirms; a cursor parked one row off is not a deferred-wrap slot; a match an earlier key's owed echo may have made neither proves nor measures, and leaves the matched guess's own echo owed: as its glyph to a later guess for the same glyph, or, over a moved tail, to every guess behind it. (2026-10-20, review of increment 4) Every glyph-lane turn starts with held_wait := None; a head g that waits for the cursor advance (`Rule::CursorAdvance`) with real_cursor = (g.row, g.col) and tail_at(g.row, g.col).legacy() ≠ Some(g.ch) (its glyph is on its cell, which the run's press saw otherwise) sets held_wait := g.seq (`Rule::DarkBehindAWait`, visibility-gate). THE CREDIT TAKEN BACK (`Rule::CreditTakenBack`): every glyph-lane credit (retire_echoed with Attr) sets last_credit := (g.row, g.col, g.born); flush, EndEpoch and start_chain clear it; at the end of every turn not Engaged, with last_credit = (r, c, b) and real_cursor = (r, k) with k < c, or k = c where c+1 ≠ grid_cols (a park in the last column is the echo's), last_credit := None, owe(b), moved_tail(b), Void: an echo never steps back over itself, so what was credited was output nobody typed for (an autosuggestion drawn part-way, its first glyph the typed one with the cursor past it), and the key's own echo is still to land, where the guess typed next would be credited with it.	src/lib.rs::divergence_flushes_all, src/lib.rs::alt_screen_flushes_predictions, src/lib.rs::space_confirms_by_cursor_advance, src/lib.rs::wrapped_guess_survives_the_deferred_wrap_cursor, src/lib.rs::a_wrap_the_terminal_did_not_defer_keeps_the_wrapped_guess, src/lib.rs::a_scrolled_cursor_does_not_pass_for_the_deferred_wrap_slot, src/lib.rs::a_wide_glyph_filling_the_last_two_columns_parks_the_cursor, src/lib.rs::a_scroll_retires_guesses_instead_of_confirming_them_against_moved_cells, src/lib.rs::a_guess_retired_without_credit_owes_its_own_echo, src/lib.rs::an_owed_echo_neither_proves_nor_measures_a_carried_guess, src/lib.rs::a_glyph_already_on_screen_is_not_its_echo, src/lib.rs::a_cancelled_pair_does_not_anchor_after_an_unrelated_cursor_move, src/lib.rs::an_echo_owed_to_a_flushed_guess_cannot_prove_a_later_one, src/lib.rs::a_cursor_back_on_a_credited_cell_takes_the_credit_back, src/lib.rs::a_glyph_typed_over_the_text_its_press_saw_waits_for_its_echo	generic `impl FnMut(u16,u16)->Option<char>` called in a loop (higher-order spec; obs arbitrary but deterministic per call), while-let over a VecDeque, match with guards, Instant arithmetic, f32 via record_rtt, &mut self	0,1,2,3,4,5,7,11,13,f32
submit-carries-proven-guesses	contract	Predictor::{note_line_submit,end_epoch,visible_count}	note_line_submit(P,now) = owe(now), EndEpoch(P), declined_tail:=0, and preds is unchanged, so next_deadline is unchanged. In Always the overlay is unchanged across the call. In Adaptive with adaptive_slow ∧ confirmed_epoch before the call, the ended epoch's guesses (a leading run of preds) stay in Vis, and every guess armed afterwards (epoch ≠ grandfathered) is outside Vis until a guess of the new epoch confirms. A second note_line_submit with no confirmation between keeps grandfathered_epoch (confirmed_epoch is false), so the carry survives any number of terminators. EndEpoch keeps carried_proven:=proven, so a carried chain displays as it did, with its cursor (edit-overlay), and its line stays its base while it is in flight (edit-line-lifetime).	src/lib.rs::submit_does_not_blank_the_tail_of_the_command_line, src/lib.rs::every_untracked_press_owes_its_echo, src/lib.rs::an_owe_is_dated_by_its_press	&mut self, u64 wrapping_add, Option assignment	0,1,2
unmodelled-input-retires	contract	Predictor::{note_unmodelled_input,predict_backspace,decline}	∀ P with mode≠Off: note_unmodelled_input(P,now) = owe(now), moved_tail_at := max(moved_tail_at, now) (tail-guard: Home, a word motion or a completion moves the far end's cursor where the live one does not show, so for a window a blank row is no evidence of the line's end), then Flush(P); hosts call it for a glyph or Backspace refused at their no-echo gate too. predict_backspace(P) with preds=[] or back.epoch ≠ P.epoch owes `now` and returns false, and is Flush(P) unless declined_tail>0 (backspace-pops-own). predict_char* of a control char is Flush(P) and returns false. Afterwards Vis=0 and overlay=[] in Adaptive and in Always, so neither a guess carried across a submit nor typed-ahead text a backward erase removes leaves a ghost. An edit key the lane arms is not unmodelled input (edit-key-routing), and an arrow it cannot place is DECLINED WITH ITS PIXELS KEPT (owe(now), moved_tail_at := max(·, now), EndEpoch, anchor_lost:=true, preds unchanged), not Flushed: the far end reads it after every key in flight, and no motion erases text. Until reconcile confirms a new guess, the next predict_char starts unproven (confirmed_epoch=false).	src/lib.rs::a_key_we_did_not_model_retires_the_guesses_in_flight, src/lib.rs::an_erase_after_a_boundary_darkens_the_carried_guesses, src/lib.rs::an_erase_landing_on_typed_ahead_text_leaves_no_ghost, src/lib.rs::an_erase_we_did_not_model_leaves_no_ghost_where_nothing_echoes, src/lib.rs::every_untracked_press_owes_its_echo, src/lib.rs::an_erase_we_did_not_cancel_owes_its_echo	&mut self, VecDeque clear/back, Option::is_some_and closure	0,1,2,3,13
backspace-pops-own	contract	Predictor::predict_backspace	∀ P with mode≠Off: preds.back()=Some(g) ∧ g.epoch=P.epoch ∧ g.op=Insert (only a typed glyph pops; predict_edit routes a Backspace here first) ∧ PairCancels(P) (`Rule::CancelsOnlyKnownShifts`; off: true; PairCancels ⇔ no g′∈preds of class InsertShift or EraseShift with ClassProven(g′)=0, pair_cancels: overwrite mode's glyph inside the text and its rubout replace a glyph and then blank or erase it, no pair that cancels, §5.25) ⇒ predict_backspace pops exactly that element, sets cancelled_at := Some((g.row, g.col)) (`Rule::CancelTransit`; off: unchanged, the pre-2026-10-17 contract), owes the popped glyph TYPED and dated by the Backspace, glyph_owes.push((g.seq, g.ch, now)) (`Rule::CancelOwes`; off: nothing, the pre-review contract; dated by g.born before 2026-10-20, it lapsed while the erase's echo, which takes the cancelled glyph off the screen, was still on time), sets cancelled_pair := Some((g.row, g.col, g.ch, next_seq)) (tail-guard), returns true and changes no other field: the pair owes no echo of any key (the erase's echo puts no glyph in a guess's cell), but the cancelled glyph's echo still lands, and with `Rule::CancelOwesItsBlank` (off: not owed) the erase's echo is owed typed as a space, glyph_owes.push((g.seq, ' ', now)) when g.ch ≠ ' ': part-way, as `\b \b`, it is a blank on the cancelled cell with the cursor past it, a space's echo exactly, which credited a space typed on that cell before the far end read it (§5.26), on the very cell the next guess anchors on, where a guess for the same glyph was credited with it. Before a later glyph can use the retained cell, CancelCursor applies at glyph entry and after pending reconciliation: with no guesses left, only the cancelled cell, its within-row next cell, or previous-row last-column parking before a wrapped col0 remain compatible. An incompatible or unknown cursor instead owes now then Flushes; a later glyph anchors on its supplied live cursor and Adaptive starts without the old proof. A glyph that reaches placement takes cancelled_at whatever it anchors on, and anchors on that cell when no current-epoch guess is pending (predict-placement): the cancelled glyph's echo lands before the erase's, the screen a cell ahead, or parked in the deferred-wrap slot at the margin, between them, and the cursor returns to that cell once the erase lands. This is a geometric refusal, not attribution against arbitrary output at the same or next cell. Flush and EndEpoch clear cancelled_at. Otherwise, with declined_tail>0, it owes `now`, sets declined_tail:=declined_tail−1, keeps preds and returns false: it erases the newest declined glyph, typed after every guess in flight. Otherwise see unmodelled-input-retires. Off ⇒ false, P unchanged.	src/lib.rs::backspace_cancels_pending_prediction, src/lib.rs::a_backspace_cancels_the_glyph_we_declined, src/lib.rs::a_cancelled_glyph_anchors_the_next_on_its_cell, src/lib.rs::a_cancelled_glyph_at_the_margin_does_not_wrap_the_next, src/lib.rs::a_cancelled_glyph_s_echo_is_owed_to_the_same_glyph_typed_next, src/lib.rs::a_cancelled_pair_does_not_anchor_after_an_unrelated_cursor_move	&mut self, VecDeque back/pop_back, Option::is_some_and closure	0,1,2,3,13
no-unechoed-flash	invariant	every public Predictor method; Predictor::{confirm,end_epoch,flush,observe_coordinates,predict_char_in_grid,visible_count}	∀ P∈Reach(Adaptive): (a) confirmed_epoch ⇔ proof_cell.is_some() ⇔ proof_echo.is_some(), and proof_line_end ⇒ confirmed_epoch. (b) confirmed_epoch ⇒ since the last boundary B a reconcile confirmed a guess h = proof_echo with h.epoch=P.epoch, Past(P,cur,h), Attr(P,now,h) and Appended(P,h) under coords=P.coords; since then no echo of the epoch retired other than as a credited append, none retired without credit (`Rule::UncreditedVoids`), no guess of it expired, and, unless proof_line_end, every reconcile and every press continuing the proof found TailClean(P,h) (standing-tail); proof_cell = After(h). (c) grandfathered_epoch=Some(e) ⇒ e≠P.epoch, and epoch e had confirmed_epoch=true when note_line_submit ended it. (d) every op displayed (a leading run of Vis ops, the glyph lane's guesses or a chain's ops behind its ghosts) has op_visible (edit-class-proof): (g.epoch=P.epoch ∧ confirmed_epoch) ∨ Some(g.epoch)=grandfathered_epoch, and its class proven there. Corollary: no motion displays in an epoch whose glyph proof is absent, nor before a motion of that epoch echoed exactly; an edit key is armed only while the proof stands (edit-key-routing). Corollary (the password guarantee): ∀ B, ∀ predict_char* after B with no confirming reconcile of a post-B guess in between, the new guess is not in overlay. That covers a new row, a prompt printed on the same row (cursor ≠ proof_cell), a scroll seen at the key seam or at reconcile, a line terminator, a stale pre-submit guess confirming after it (confirm ignores h.epoch≠epoch), a declined glyph, an unpopped Backspace, unmodelled input and an estimate restore.	src/lib.rs::fresh_line_resets_confirmation_no_leak, src/lib.rs::a_stale_guess_confirming_after_submit_cannot_arm_the_new_line, src/lib.rs::a_scroll_ends_the_epoch_on_the_same_physical_row, src/lib.rs::a_line_terminator_makes_what_follows_tentative, src/lib.rs::the_submitted_lines_echo_cannot_prove_the_next_line	trace invariant over every &mut method (history ghost state for clause b), per-element quantification over a VecDeque, reconcile's obs closure, u64 wrapping epochs (needs no wrap-around within one carry, or a statement by epoch inequality), Option<u64>	0,1,2,4,5,7,13
link-estimate-swap	round-trip	Predictor::{reset,reset_session,take_link_estimate,restore_link_estimate}, LinkEstimate::default	reset(P) = Flush(P) with coords:=None and deferred_wrap:=false, keeping Link(P). reset_session(P) = reset(P) with Link := LinkEstimate::default() (srtt=None, 0, 0, false, backoff 0, owed_at=None). take_link_estimate(P) returns E=Link(P), its owed_at raised to the newest born in preds (the guesses its reset flushes) and in the typed owes (folded into an owe of any echo: the estimate carries no glyphs), and leaves reset_session(P). restore_link_estimate(P,e) = reset(P) with Link:=e. Hence restore(take(P)) has the original Link(P) and Screen = reset; restore(default) equals reset_session; after restore, preds=[] (overlay=[], is_displaying=false, pending_bounds=None) and confirmed_epoch=false whatever P was. After reset_session, W = 250 ms. Link carries the learned bindings too (learned-bindings): reset keeps them, reset_session clears them, take carries them out and restore installs them, so a swapped session keeps what its line editor taught and an unknown one starts with none.	src/lib.rs::session_switch_forgets_slow_link_rtt, src/lib.rs::a_backgrounded_session_keeps_its_link_estimate, src/lib.rs::restoring_an_estimate_clears_the_outgoing_panes_pixels, src/lib.rs::a_default_estimate_is_the_no_estimate_path, src/lib.rs::the_cold_link_probe_travels_with_the_session_estimate, src/lib.rs::session_reset_restarts_the_cold_link_probe, src/lib.rs::a_returning_sessions_old_echo_cannot_prove_its_new_guess, src/lib.rs::typed_owes_fold_into_any_echo_past_the_cap_and_at_a_take	&mut self, a Copy struct with Option<f32> and private fields, struct construction, call composition (take → reset_session → reset → flush)	0,1,2,4
visibility-model-refinement	refinement	Predictor::{predict_char,reconcile,overlay,note_line_submit,note_unmodelled_input,predict_backspace,reset_session}, aterm_models_core::predictive_echo_visibility_model	There is a projection π(P) → PredictiveEchoVisibility state: pending = preds≠[], visible = Vis>0, confirmed = confirmed_epoch, slow = adaptive_slow, fast_streak = fast_samples, fresh = (Link = default), retracted = this step closed adaptive_slow, owed = Owed(P,now). ∀ P∈Reach(Adaptive), each Predictor step maps to a model action or a stutter: a drained turn with a sample <8 ms → ConfirmFast; the same with preds≠[] → ConfirmFastInFlight; the turn that sets adaptive_slow → ConfirmSlow; predict_char → Key; expiry → Expire (which owes and clears confirmed; the code's owe is dated by the expired press, already past its window, so OwedExpire follows as the next step); confirming the last pending guess → Echo; every boundary B → Submit (which owes, over-approximating the one boundary that owes nothing: the program moving the cursor), but the rows moving under the screen (observe_coordinates) → RowsMoved, which owes and ends the proof as Submit does and keeps gate and first; the owed window passing → OwedExpire; a match retired while Owed → Echo; reset_session → SwitchSession. Typed owes (typed-owes, and the cancelled glyph's, backspace-pops-own) project to owed = 0: the model has no glyph identity, so the theorem's directed case `a_guess_retired_without_credit_owes_its_own_glyph` and `Rule::TypedOwesSameGlyph` pin them instead. A match retired WITHOUT credit maps to Echo, and when it was refused by an owe of any echo (owed = 1) ProofIsAttributable already has confirmed = 0; when a typed owe refused it (owed = 0) the code also clears confirmed (`Rule::UncreditedVoids`), which the model's Echo does not: that step refines Echo followed by confirmed := 0, a strengthening no model invariant can be broken by (each holds with confirmed = 0 and visible = 0), and the position fact it keeps (no proof on a cell the retired key's echo is still to take) is below the model's abstraction, held by the theorem's FIDELITY clause at its named case `an_uncredited_retirement_leaves_no_proof`. The standing tail's void, a tail refusal and the line-end bookkeeping are likewise below it (they only lower confirmed). So are the floor's void and the credit taken back (2026-10-20: they lower confirmed, the second owing too, which the model's Submit over-approximates), DARK BEHIND A WAIT (it only lowers Vis) and the engine's wrap flag (placement). Each model invariant (VisibleNeedsProofAndBenefit, RetractOnlyOnSustainedFastEvidence, FreshSessionHasNoInheritedRtt, ProofIsAttributable) then holds of π(P); ProofIsAttributable only while the window has not grown since the owing step (a larger srtt can reopen an Owed the model has already expired). The model's own negative controls (hand-built bad states rejected by check_invariant) are Tier-0 model checks. Needs a Submit with a carried-pending branch first (§5.5). THE EDIT CLASS: π also maps class = (the front op is a motion), edit_proven = Motion ∈ proven. predict_edit arming a motion, and a shift armed by predict_edit or a glyph press (shift-ops), → KeyEdit, each class projected on its own (the model's one abstract edit class: the binding drives Motion, InsertShift and EraseShift in turn, each with its rule as the control); its credited echo whose kind every in-place world refutes, with the glyph proof standing → ConfirmEdit; an arrow declined (decline-motion) → Submit; a retirement an in-place world also shows, its kind unproven, → Submit (its Flush ends the proof); an erase at the end is a glyph-class op (Key, Echo). EditVisibleNeedsClassProof and ClassProofRidesOnGlyphProof hold of π(P) (edit-class-proof: Void clears proven with the glyph proof). THE LINE EDGE: π also maps witness = (line = Some(l) ∧ l.fresh ≠ None), learned = Learned(k) for the key the binding drives, class = 2 while the front op is a learned key's, gate = (prompt_gate = Open), first = (run_witness ≠ None) and witness_first = witness. note_line_submit_at with a prompt → SubmitAtPrompt (any other boundary → Submit, which shuts the gate, but a coordinate move → RowsMoved, which keeps it: a scroll lets no key past, `Rule::ScrollKeepsFirstKey`); a credited glyph that starts a witnessed line → Witness, which needs first; predict_line_edge arming the key → KeyLineEdge; its credited echo that makes the binding learned → ConfirmLineEdge, an earlier one (the code needs two, the model one) → Echo; a divergence or expiry it heads → Contradict; every boundary drops the line → Submit or RowsMoved, which end the witness; reset_session clears the bindings → SwitchSession. LineEdgeNeedsWitnessAndBinding, FreshContextHasNoBinding and WitnessIsTheFirstKey hold of π(P) (learned-bindings, line-edge-witness).	tests/conformance_visibility.rs::adaptive_fast_expiry_is_invisible_and_slow_echo_may_display, tests/conformance_visibility.rs::the_display_gate_closes_only_on_sustained_fast_evidence, tests/conformance_visibility.rs::every_epoch_boundary_is_the_models_submit, tests/conformance_visibility.rs::an_owed_echo_proves_nothing_until_its_window_passes, tests/conformance_visibility.rs::expiry_owes_and_voids_the_proof, tests/conformance_visibility.rs::an_edit_displays_only_after_its_class_is_proven, tests/conformance_visibility.rs::a_line_edge_key_displays_only_when_learned_at_a_witnessed_prompt, tests/conformance_visibility.rs::a_scroll_keeps_the_first_key_s_gate	model→code refinement over a ty_model! (non-scalar projection, stuttering simulation, ConfirmSlow abstracting two concrete turns), closures, &mut self	0,1,2,5,10,13
tail-guard	contract	Predictor::{retire_echoed,appended,tail_clean,tail_at,tail_decorated,take_tail,ended_line_at,moved_tail}	Appended(P,h), for a guess h of the run in flight just popped by reconcile_seen with observe obs and real cursor cur: ¬(run_inside ∧ `Rule::InsideRunsNeverProve`) ∧ Unmarked(P,h) ∧ (tail_line_end ∨ TailClean(P,h)). run_inside is set by the run's first press: the known line of P.epoch had its cursor left of its text's end with nothing pending (a chain's motion retired there), so the run is a mid-line insert by knowledge, which the inference below misses on a space of the line (a typed space reads blank: typed over nothing). Unmarked(P,h) (`Rule::MarkedEcho`; off: true): obs(h.row,h.col) is not Opaque, or T?(h.row,h.col) is Opaque too (a right-to-left row's blanks): a zero-width mark the far end's buffer held after its cursor attaches to a glyph inserted before it, and the echo's own cell is the one place that hidden tail shows. TailClean(P,h): Over(T?(h.row,h.col), h.ch) ∧ ∀ c ∈ [h.col+1, grid_cols): if a pending g has (g.row,g.col)=(h.row,c) then Over(T?(h.row,c), g.ch) ∧ (obs(h.row,c) = T(h.row,c) ∨ (obs(h.row,c).legacy() = Seen::of_cell(g.ch,false).legacy() ∧ Past(P,cur,g))), else obs(h.row,c) = T(h.row,c); with `Rule::CursorAdvance` off the Past conjunct is dropped; and a cell at the position of cancelled_pair = Some((r,c,ch,_)) may also hold ch (our own cancelled pair writing and erasing it; cancelled_pair is set by every Backspace that pops our guess, to its cell, glyph and next_seq, and cleared by Flush, EndEpoch and the retirement of any guess with seq ≥ that next_seq). Here T?(r,c) = Some(tail_cells[c−from]) when tail=Some((r,from)) ∧ from ≤ c (Blank past its end) and None otherwise, T = T? or Blank, and Over(s, ch) ⇔ s = None ∨ s.legacy() ∈ {None, Some(ch)}: every guess of the run was typed over nothing (a blank, opaque or not: the blanks of a BiDi-reordered row are Opaque(None)) or that very glyph (a guess typed over other text is mid-line whatever the row shows once the tail has left it). Compared whole otherwise: an Opaque cell equals only the same Opaque. tail_line_end (the run is at the END OF THE LINE) is set by the run's first press (key-row-sample): when it continues a proof, := proof_line_end; else when its anchor = line_end_at, := true; else := the press carried a row ∧ every snapshot cell's legacy() is None ∧ ¬(moved_tail_at = Some(t) ∧ now − t ≤ W(P)). line_end_at is set to After(h) by every append retired in a current-epoch run with tail_line_end (credited or not) and cleared by Flush, EndEpoch and any expiry; moved_tail_at := max(moved_tail_at, h.born) for every retirement that is not Appended and every standing void (standing-tail), and := max(·, now) at every arrow declined (edit-key-routing) and every key not modelled (unmodelled-input-retires), whose echo moves the far end's cursor where the live one does not show. Within one reconcile call a row found clean stays clean for every later guess on it (its tail is a suffix of the cells compared, against the same T, with the same pending guesses exempt under the same cur, and its own cell was compared as a pending guess's), so a burst costs one row scan: ≤ grid_cols observe calls. An attributable echo of the current epoch proves (confirm) iff Appended or `Rule::TailGuard` is off; otherwise it still records its RTT sample and VOIDS the epoch's proof (Void), since the guesses behind it were placed as appends on a line that is not appending. Corollaries: a mid-line insert (the echo shifts the text right of it) proves nothing, so the next glyph is not painted over the tail that moved, even once a burst has pushed that tail off the row (onto the next row of a wrapped line), and, with standing-tail, when the far end writes the glyph before the tail; a right prompt or a matched suggestion the echo leaves in place still proves; a run at the end of the line proves whatever the far end draws beside it (an autosuggestion drawn, changed or cleared; a glyph typed onto one), which the comparison alone refused; a blank row read within a window of a moved tail, of an arrow declined or of a key not modelled, or by a press continuing a proof not at the end of the line, is no evidence of the end (the tail it hides, or the motion, may still be landing: a run typed on the live cursor before a declined `←` landed was inserted before the line's last glyph, proved as an append, and the glyph typed next was painted over that glyph, §5.25); a run started without a row proves only over a blank tail unless it inherits the end of the line; an overwrite of other text proves nothing either (fail closed). One row is all it sees: a wrapped line whose first row ends in spaces reads blank from a cursor on one of them.	src/lib.rs::a_mid_line_echo_proves_nothing, src/lib.rs::a_right_prompt_the_echo_leaves_in_place_still_proves, src/lib.rs::a_dirty_tail_voids_a_standing_proof, src/lib.rs::a_retired_burst_reads_its_tail_once, src/lib.rs::a_run_typed_over_other_text_never_proves, src/lib.rs::a_suggestion_beside_the_end_of_the_line_darkens_no_key, src/lib.rs::a_blank_row_behind_a_declined_arrow_is_no_line_end, src/lib.rs::a_run_started_inside_the_text_proves_nothing	&mut self, a Vec<Seen> snapshot indexed by u16 arithmetic, generic FnMut observe passed by &mut, a per-call memo, Option<Instant> windows	0,1,2,3,5,7,11,12,13
standing-tail	contract	Predictor::{reconcile_seen,arm_glyph,tail_clean,moved_tail,standing_at_press,far_tail_moved,start_chain,rebase_proof}	THE STANDING TAIL (`Rule::StandingTail`, with `Rule::TailGuard`): a proof not at the end of the line holds only while TailClean(P,proof_echo) holds of the row right of the echo it rests on. (a) At the end of every reconcile_seen turn whose retirements left proof_echo unchanged, with confirmed_epoch ∧ ¬proof_line_end: ¬TailClean(P, proof_echo) under that turn's obs and cur ⇒ moved_tail_at:=max(moved_tail_at, proof_echo.born) and Void. (b) At a predict_glyph that anchors on the live cursor (no current-epoch guess pending, no cancelled cell) with s.row=Some(row) while confirmed_epoch ∧ ¬proof_line_end, BEFORE it takes the new snapshot: ¬TailClean(P, proof_echo) with obs(r,c) = row[c] on the cursor's row (Blank elsewhere) and cur = s.cursor ⇒ the same. (c) THE CARRIED STANDING TAIL: EndEpoch with confirmed_epoch keeps carried_proof := (proof_echo, proof_line_end, proof_opaque); at every reconcile_seen while a guess of grandfathered_epoch is pending, ¬carried line end ∧ ¬TailClean(P, carried echo), or (`Rule::MarkedEcho`) a carried echo that proved plain reading Opaque now ⇒ moved_tail_at := max(·, its born), grandfathered_epoch := None, carried_proof := None: the carried guesses go dark, still tracked (Flush clears carried_proof). Ending the epoch voided the proof and with it (a) and (b), so a guess typed on a proof earned between a glyph and its tail redraw, carried by an arrow declined with its pixels kept or by an Enter, stood on glass over the tail once it moved (§5.21). Why: comparing only as guesses retire, an echo that writes its glyph FIRST and redraws the tail it shifts after (`e`, then ` cd` and the cursor stepped back) proved on the frame between the two, and the next glyph, anchored on that proof, was painted over the shifted space (`abeicd` on glass, `abei cd` settled). Re-read at the frame after the redraw the tail has moved; a press that comes first compares its own row, since the snapshot it takes next would be of the moved row; and a pending guess typed on the proof is held to its snapshot cell (typed over the tail glyph it would push, Over fails), so it is never painted over the moving tail. A glyph in a pending guess's cell is that guess's echo only with the cursor past it (TailClean's Past conjunct): `o` typed over the last `o` of `abo` by such a far end, `o` and `z` typed on the proof, and the displaced `o` in the second `o`'s cell, the cursor on it, kept the proof, and `z` was painted where the displaced `o` settles. (d) THE CHAIN'S STANDING TAIL: while a chain is in flight on a line with ¬end_known (end_known: every glyph that built the line proved at the end of the row, tail_line_end, kept by extend_line as their conjunction), the row past every state of it is held to the snapshot its base was verified with (start_chain at the chain's first press, rebase_proof at a credited retirement, edit-reconcile): a cell from max(V, the snapshot's start) to the margin holding a glyph the snapshot did not, or another, ⇒ moved_tail(newest born) and Flush (far_tail_moved; a cell that cleared, a right prompt hidden, moves nothing); (a) and (c) are skipped while Engaged, whose states own the line. Such a line ends at hi only because the text its echoes left in place stayed in place, and text appearing past every state may be the buffer's own tail drawn late. (b) holds at every press that starts a chain too, never skipped for a cancelled pair's transit (standing_at_press): predict_edit not Engaged, whatever glyph-lane run is pending, and arm_inside. The chain's reconciles hold only the row past its states, against the snapshot that press takes: skipped with a run in flight, a proof earned between a glyph and its tail redraw, an arrow pressed after the redraw and before the next frame, re-snapshotted the moved row, and the run's next glyph was shown over it (§5.25). (e) THE STANDING GLYPH (`Rule::StandingGlyph`; off: not read): at the start of every reconcile_seen turn, before anything retires, not Engaged, with confirmed_epoch: proof_echo's cell read once (shared with the standing mark) whose legacy() ≠ Seen::of_cell(proof_echo.ch).legacy() ⇒ moved_tail(its born) and Void; the same at a press that re-snapshots the row, from its own row (standing_at_press); and a carried proof's echo cell that no longer holds its glyph ends the carry as (c) does. The proof names the cell after its echo, where the cursor stands while only our own keys have moved it; output nobody typed for can erase or redraw the line and bring the cursor back to that cell, which no check of the row right of a proof earned at the end of the line sees: a password prompt printed on the same row and a clear that redrew the shell prompt put the cursor on a pending space's cell, the redraw's trailing space credited it, and the proof carried on to paint the secret typed next (§5.26). Read before the turn retires anything, so the redraw and the next echo landing in one frame are read as they are. Cost: one row scan per reconcile while a decorated proof stands, none at the end of the line, and one cell read per turn while any proof stands. (f) THE FLOOR (`Rule::FloorBelow`; off: not read): a run's first press (take_floor) keeps floor := cursor row + 1 and its cells from s.below when the guess is placed on the cursor's row (else none); at the end of every turn not Engaged with confirmed_epoch, the proof's echo on row floor − 1, the real cursor above the floor (on it, the line wrapped there and the editor writes it) and a cell the press read blank holding text other than the glyph a pending guess names there (a glyph typed ahead wraps onto the floor, and its own echo landing early is no prompt) (floor_moved) ⇒ moved_tail(proof_echo.born), Void, floor_voids+1. A password prompt printed on the next row and a reverse index back onto the proof's cell, in the burst that earned the proof, showed the secret typed next; text that changed or cleared (a completion list under the line) moves nothing.	src/lib.rs::a_glyph_echoed_before_its_tail_moves_proves_nothing_after, src/lib.rs::a_shifted_glyph_is_not_a_pending_guess_s_echo, src/lib.rs::a_chain_beside_a_right_prompt_holds_the_row_past_it, src/lib.rs::a_credited_shift_re_bases_the_standing_proof, src/lib.rs::an_edit_press_holds_the_standing_proof_to_its_row, src/lib.rs::an_arrow_with_a_run_in_flight_holds_the_standing_tail, src/lib.rs::a_glyph_typed_inside_the_text_holds_the_standing_proof_to_its_row, src/lib.rs::a_proof_whose_echo_was_redrawn_stands_no_more, src/lib.rs::text_drawn_under_the_line_voids_its_proof, src/lib.rs::text_over_a_wrapped_guess_s_cell_voids_its_proof	&mut self, generic FnMut observe, a press-row closure over Option<&[Seen]>, if-let chains	0,1,2,3,5,7,11,13
seen-opacity	contract	Seen::{of_cell,legacy}, Predictor::{reconcile,reconcile_seen}	Seen::of_cell(ch, opaque) = Opaque(l) if opaque else (Glyph(ch) if ch≠' ' else Blank), with l = (ch≠' ').then_some(ch); legacy(Blank)=None, legacy(Glyph(c))=Some(c), legacy(Opaque(l))=l. The glyph lane (retirement, divergence, the space-by-cursor-advance rule, the cursor check and the deferred-wrap latch) reads every answer through legacy(), so reconcile_seen with an obs whose cells are Opaque(x) retires, flushes and latches exactly as with Glyph/Blank cells carrying the same x, but for the head guess's own cell read Opaque(None), which Flushes it (`Rule::OpaqueHead`: a zero-width mark the far end's buffer held after the cursor, or a wide glyph's half, is there, which a plain ghost cannot show; off: read as the blank it carries); only TailClean compares whole answers. reconcile is reconcile_seen with no Opaque cell.	src/lib.rs::an_opaque_cell_is_its_glyph_to_the_glyph_lane	enum match, Option::then_some, closure adapters	0,1,3,5,13
key-row-sample	contract	Predictor::{wants_key_row,mode,predict_glyph,take_tail}, KeySample	wants_key_row(P) ⇔ preds=[]: it does not ask the mode, because a host asks before its press applies the mode it predicts in (a predictor still Off from a window that never predicted would otherwise answer no to the press that starts its first run), so the mode is the host's to ask first (mode() = P.mode, for a host that keeps none). predict_glyph(ch,s,now): when the guess is accepted and anchors on the live cursor (no current-epoch guess pending) at s.cursor itself, tail := Some((s.cursor.row, s.cursor.col)) and tail_cells := [s.row[c] (Blank past its end) | c ∈ [s.cursor.col, cols)], the cursor's own cell included, refilled in place; when it anchors on the live cursor but s.row=None or the guess wrapped to the next row, tail := None. The same press sets tail_line_end (tail-guard): proof_line_end when it continues a proof (after the standing-tail press check), true when its anchor = line_end_at, else (a snapshot was taken ∧ all its cells' legacy() are None ∧ no moved tail within W of now). A type-ahead guess leaves tail and tail_line_end as they were. So the snapshot is the row from the cursor at the press of the run's first guess, read under the hold that read the cursor, and a host that reads the row only when wants_key_row() loses nothing but proof over a non-blank tail when rows moved between that call and the press. KeySample::below is the row under the cursor row, read under the same hold whenever row is and the cursor row is not the last (THE FLOOR, standing-tail (f)); KeySample::wrap_pending is the engine's deferred-wrap flag (predict-placement); KeySample::input_start, context and escaped_chords are line-edge-witness's and learned-bindings'.	src/lib.rs::a_mid_line_echo_proves_nothing, src/lib.rs::a_right_prompt_the_echo_leaves_in_place_still_proves, src/lib.rs::a_predictor_still_off_wants_the_row_of_the_press_that_starts_it	&mut self, Option<&[Seen]> borrow, Vec refill with extend over a u16 range, Option<bool> inheritance	0,1,2,3,5,7,12
typed-owes	contract	Predictor::{retire_echoed,owe_glyph,attributable,owed_with_pending,flush}	A guess h retired by reconcile_seen without credit (¬Attr) is owed TYPED, glyph_owes.push((h.seq, h.ch, h.born)), when `Rule::TypedOwes` is on and Appended(P,h) holds (tail-guard: at the end of the line, or over a clean tail) or `Rule::TypedOwesCleanTail` is off; otherwise as any echo: owe(h.born) and g.owed := max(g.owed, Some(h.born)) for every pending g (the pre-2026-10-17 contract, which `Rule::TypedOwes` off restores). A typed owe refuses credit only to a later guess for the same glyph while its window runs (Attr), unless `Rule::TypedOwesSameGlyph` is off. Before each push, entries older than GLITCH_CEILING_MS are dropped (no window is longer); at MAX_GLYPH_OWES (64) the oldest folds into owe() and raises every pending g.owed. Flush, reset, take_link_estimate and reset_session fold the typed owes into owed_at (Flush, link-estimate-swap). Why typed: the retired guess's own echo is its glyph, at the cell after the match that retired it, which is the next guess's, so only a later guess for the same glyph can be fooled by it; owed as any echo, each uncredited retirement refused credit to every guess behind it, and a line typed faster than the window never proved after one owed key. Why only for an append: a guess retired mid-line has an echo that shifts text whose glyphs could stand in any later guess's cell (the cursor check flushes the guesses behind such a cell within one row, and the sixty-seed sweep, rewriting far ends included, finds no counterexample; see the theorem's `rule_clause`). A typed owe beside a proof is why an uncredited retirement voids it (reconcile-contract, `Rule::UncreditedVoids`): the retirement it refuses leaves the retired key's echo still to land, on a cell the proof may name once an erase in flight puts the cursor back.	src/lib.rs::an_uncredited_guess_owes_only_its_own_glyph, src/lib.rs::a_guess_retired_over_a_moved_tail_owes_any_echo, src/lib.rs::typed_owes_fold_into_any_echo_past_the_cap_and_at_a_take, src/lib.rs::a_guess_retired_without_credit_owes_its_own_echo	&mut self, a bounded VecDeque of tuples, u64 press order, Instant arithmetic, iterator any with a closure	0,1,2,3,5,7,12,13
edit-key-routing	contract	Predictor::{predict_edit,decline_motion,placeable,press_row_clear,chain_text_at,set_editing}	For mode≠Off and cols>0, after observe_coordinates, grid_cols:=cols, CancelCursor(P,Some(cur),now), settle_chain and (preds=[] ⇒ anchor_lost:=false): a learned key (Home, End, CtrlA, CtrlE, Delete) is predict_line_edge's (line-edge-witness); otherwise editing_off ⇒ Backspace ≡ predict_backspace(now) (Cancelled iff it popped) and ←/→ ≡ note_unmodelled_input(now), Declined. Otherwise a Backspace with preds.back() a current-epoch Insert and PairCancels(P) pops it (backspace-pops-own), Cancelled; without PairCancels it is armed below as any Backspace is. Not Engaged, the press first holds the standing proof to its row (standing-tail (b)), whatever glyph-lane run is pending: the chain it starts re-snapshots the row. Otherwise E = Placeable(P,cur), kept only if the press row is Some, holds nothing that makes it reorder (right-to-left-rows), holds no Opaque cell in [E.lo, min(E.V+1, cols)) (R-OPAQUE), and for a Backspace every cell of [E.hi, min(E.V+1, cols)) is Blank or, below E.V, the chain's own text there (the line's, or a pending Insert's) (`Rule::VacateRefusal`). ← is armed iff E.c > E.lo, → iff E.c < E.hi (`Rule::TextEdges`; off: at the edge too), a Backspace iff E.lo < E.c ≤ E.hi; each only while |preds| < EDIT_CHAIN_MAX (64): a chain that starts (not Engaged) takes the press row from lo to the margin as seen and as its tail snapshot (start_chain), then push {row:E.row, col:E.c, ch:NUL, op, class: Motion for ←/→, Glyph for an erase at E.hi, EraseShift for one inside the text (shift-ops), born:now, epoch, owed:owed_at, seq:next_seq, stale:false}, next_seq+1, declined_tail:=0, chain:=true, cancelled_at:=None, Armed. Otherwise ←/→ is DECLINED WITH ITS PIXELS KEPT: owe(now) (`Rule::EditOpsOwe`), EndEpoch, anchor_lost:=true, preds unchanged, Declined; a Backspace is predict_backspace(now). Placeable(P,cur) ⇔ ¬editing_off ∧ edit_capable ∧ ¬anchor_lost ∧ confirmed_epoch ∧ line=Some(l) ∧ l.epoch=P.epoch ∧ (preds=[] ⇒ cur=(l.row,l.cursor)) ∧ every g∈preds has g.epoch=P.epoch, g.row=l.row and g.col = the cursor the ops before it leave, every Insert at or inside the text with hi+2 ≤ cols before it (R-ROW: the text's end, one cell on, short of the last column); E is the fold of preds onto l by lengths and cursor (edit-fold), E.V its furthest text end. set_editing(on) flushes on a real change.	src/lib.rs::a_first_motion_stays_dark_until_its_echo_proves_the_class, src/lib.rs::an_arrow_that_cannot_be_placed_keeps_the_guesses_in_flight, src/lib.rs::an_arrow_at_an_edge_of_the_text_is_declined, src/lib.rs::editing_off_routes_edit_keys_exactly_as_before, src/lib.rs::an_erase_beside_text_past_the_end_is_not_armed, src/lib.rs::a_chain_is_capped	&mut self, enum match with guards, a borrowed &[Seen], VecDeque push_back	0,1,2,3,13
edit-fold	definition	Predictor::{fold_op,text_end,chain_far_end,fold_cells}	A state chain's states: S_0 = (line.text, line.cursor), lo = line.lo; S_j = fold(S_{j−1}, preds[j−1]) with Insert(ch)@c: text.insert(c−lo, ch), c+1; Left@c: c−1; Right@c: c+1; Erase@c: text.remove(c−1−lo), c−1; LineStart@c: lo; LineEnd@c: lo + |text|; Delete@c: text.remove(c−lo), c. hi_j = lo + |text_j|; V_j = max(line.vacated, hi_0, …, hi_j), the cells [hi_j, V_j) VACATED in S_j; cells at or past V_j are no state's (a right prompt, a suggestion: never compared, never painted). An Insert at c < hi_{j−1}, or an Erase at lo < c < hi_{j−1}, is a SHIFT (shift-ops): the text from c moves one cell right, or left. A Delete moves the text right of the cursor one cell left too, of its own class (DeleteFwd, learned-bindings). compose_chain folds the same rows over (glyph, source column) pairs (fold_cells, the one body every fold runs), each glyph of line.text carrying its column and a typed glyph none, so the overlay names the column a moved glyph came from (edit-overlay). The safety theorem's PROVENANCE oracle implements the same rows independently, on a Vec<char> folded from the keys.	tests/reconciled_view_safety.rs::each_named_case_holds_and_catches_its_mutant	Vec insert/remove, u16 arithmetic	0,1,5,7
edit-reconcile	contract	Predictor::{reconcile_chain,match_chain,part_way_before,state_matches,retire_chain,credit_op,retire_net_null,in_transit,far_tail_moved,rebase_proof,in_place_worlds,world_step}	Engaged(P), after observe_coordinates and the alternate-screen flush: seen := obs(row,c) for c ∈ [lo, cols) (the row past every state is where a moved glyph's source may stand, and what a credited retirement re-bases the standing proof on), V the furthest text end of any state; an Opaque cell in [lo, min(V+1, cols)) ⇒ Flush (R-OPAQUE). On a line with ¬end_known (proven beside text its echoes left in place) the cells from max(V, the snapshot's start) to the margin must each hold what the tail snapshot holds there, or a blank (a right prompt cleared), else moved_tail(newest born) and Flush (standing-tail (d)). Then, with an erase pending (an Erase or a Delete), the SENTINEL seen[V−lo] (the cell one past every state, which no state compares) not Blank ⇒ Flush before any match (`Rule::VacateRefusal`): a split erase echo may still show the chain's own vacated text, which the transit wait below allows, but nothing past every state is the chain's. Match(j) ⇔ cur=Some((row,S_j.c)) ∧ (`Rule::WholeStateMatch`; off: the cursor alone) every c∈[lo,hi_j) holds text_j[c−lo] (Blank ≡ ' ') ∧ (`Rule::VacatedBlankInMatch`) every c∈[hi_j,V_j) is Blank. j* = min{j∈0..=n : Match(j)} (`Rule::SmallestMatch`; off: the max). A j* the echo of an earlier op shows PART-WAY is no match (`Rule::PartWayEchoes`; off: matched; part_way_before): for an op e < j*, an Insert inside the text with the cursor at the end of S_e's text, or an Erase or a Delete with the cursor at the end of S_{e−1}'s text and the cell it vacated blank (zle's delete-char rewrites the tail and a blank before it steps back), or (`Rule::PartWayStepBack`; off: at that end alone) either with the cursor anywhere on its step back, strictly right of S_e's cursor up to that end (a `\b` a cell, a read ending after any: a Delete's step back passes End's state and, at `lo`, ends on Home's; 2026-10-20, the review of increment 5, seed index 2748), or a LineStart or LineEnd with the cursor strictly between S_{e−1}'s cursor and S_e's (its echo reprints the text or steps over it a cell at a time), S_e's text shown and that cursor not S_e's; zle's erase writes a blank on the cell it vacates before it steps back, and readline's insert its tail, and caught there a Backspace and a space typed after it matched as both read, the space credited unread (§5.26). With j*: every op 1..=j* is asked Attr(P,now,g) first, under the window as it stood before the turn (`Rule::ChainAttributedWhole`; off: each as it is popped, so the ops ahead of an unattributable one were credited first), and one not Attr ⇒ owe(g.born) then Flush, stop, nothing credited; then, with a shift of an unproven kind pending, the IN-PLACE WORLDS (shift-ops): Fewer ⇒ Flush, stop, nothing credited (`Rule::InPlaceReadsFewer`; off: credited as below; the smallest match counts the keys an editor of the chain's own kind has read, and overwrite mode's screen after k keys can be the chain's state after more); then pop them front first, each credited records record_rtt (the turn sample is the first), its seq as credited, clears cancelled_pair once a key typed after it retires, and credit_op: an Insert of P.epoch confirms (confirmed_epoch, proof_cell := its state's cursor, proof_echo := g, proof_line_end := tail_line_end); a motion, erase, line edge or delete of P.epoch, only while confirmed_epoch (`Rule::MotionsNeverProve`; off: it confirms too), moves proof_cell to its state's cursor; a motion sets proven ∋ Motion, and a shift its own bit when its kind is in Proves (shift-ops); line_end_at := None. With Other, the retirement ends in Flush once its ops are credited (shift-ops); otherwise each credited op of a learned key is one echo of its binding (learned-bindings). line := S_{j*}, vacated := V_{j*}; with a current-epoch op credited under the glyph proof, the standing proof is RE-BASED on that line (rebase_proof: proof_echo := its last glyph, or its first cell when empty, proof_line_end := end_known, the tail snapshot := seen), so the shift's own state, which the screen matched whole, is not a moved tail (standing-tail (d)). NET-NULL: S_{j*} = S_n with ops left ⇒ all retire without credit, owe(max born), Void when one is of P.epoch (`Rule::NetNullUncredited`; off: credited as above). Then, with an erase pending, seen[hi−lo..] not all Blank ⇒ Flush (`Rule::VacateRefusal`). No j: cur on line.row with lo ≤ cur.col ≤ V (`Rule::TransitReach`; off: anywhere on the row, where a password prompt printed on the next row and a reverse index back up left a chain waiting, displayed, and the glyph typed at that prompt joined it) and every cell some state compares (c < V_k) holding S_k(c) for some k comparing it (a cell in [hi_k, V_k) holds Blank), or Blank at a column ≥ W, the leftmost cell any pending Insert, Erase or Delete writes at (min of its state's cursor and the one before) (`Rule::TransitBlanks`; off: any Blank cell; no echo of the chain's keys writes left of W and a motion's writes no blank, so a blank there is text that moved or went: zsh's reset-prompt redrawing the prompt narrower left a `←` waiting and the line painted twice, 2026-10-20, the review of increment 5, seed index 2482) ⇒ wait (the TRANSIT WAIT: an ICH before its glyph, a tail rewrite, a cancelled pair, an erase echoed in pieces); otherwise the head's learned key, if any, is contradicted (learned-bindings) and Flush. Not Engaged: reconcile-contract.	src/lib.rs::a_burst_the_screen_cannot_tell_apart_retires_at_the_smallest_state, src/lib.rs::a_split_erase_echo_matches_no_state_early, src/lib.rs::an_erase_in_transit_refuses_new_text_past_every_state, src/lib.rs::a_chain_matches_whole_states_not_its_cursor, src/lib.rs::a_motion_echo_never_proves_its_epoch, src/lib.rs::an_opaque_cell_on_the_chain_s_line_flushes_it, src/lib.rs::an_erase_caught_part_way_is_no_later_state, src/lib.rs::a_delete_s_step_back_is_no_later_state, src/lib.rs::a_narrower_redraw_is_no_echo_in_transit	impl FnMut(u16,u16)->Seen in a loop, Vec compare, VecDeque pop_front	0,1,2,3,4,5,7,11,13,f32
edit-class-proof	invariant	Predictor::{op_visible,visible_count,credit_op,void_proof,end_epoch,flush}	∀ P∈Reach(Adaptive), for each class K ∈ {Motion, InsertShift, EraseShift}: K ∈ proven ⇒ confirmed_epoch, and since the glyph proof was last earned a credited smallest-match retirement of a current-epoch op of class K occurred (its kind refuted in every in-place world, for a shift): credit_op sets it only under the glyph proof, and Void clears it (`Rule::ProofsEndWithTheEpoch`), so every boundary and every void does. EndEpoch sets carried_proven := proven when it grandfathers; Flush clears carried_proven. op_visible(g) = (g.epoch=P.epoch ∧ confirmed_epoch ∧ ClassOk(g,proven)) ∨ (Some(g.epoch)=grandfathered_epoch ∧ ClassOk(g,carried_proven)), ClassOk(g,s) ⇔ g.class=Glyph ∨ (g.class ∈ {LineEdge, DeleteFwd} ∧ ¬g.probe, learned-bindings) ∨ g.class∈s, each class under its own rule (`Rule::MotionClassProof`, `Rule::InsertShiftProof`, `Rule::EraseShiftProof`; off: always). A shift's bit is set only by a credited shift whose kind every in-place world refutes (shift-ops): readline's overwrite mode never sets either, so it never shows a shift. Vis in Adaptive is the leading run of op_visible behind adaptive_slow; with every op of the glyph class it equals the earlier Vis on every reachable state (no epoch's guesses ever wait behind another's: nothing is armed behind a terminator, a decline or an unmodelled key while the queue holds the earlier epoch's).	src/lib.rs::a_motion_proof_does_not_outlive_its_line, tests/editing_win.rs::predictive_editing_wins_and_never_shows_a_wrong_line, tests/editing_win.rs::a_cooked_tty_shows_no_motion_and_overwrite_mode_no_shift, tests/editing_win.rs::class_proofs_end_with_their_line	bitset u8, take_while closure	0,1,2,13
edit-overlay	contract	Predictor::{overlay,overlay_frame,compose_chain,overlay_cursor,pending_bounds}	Engaged ⇒ overlay(P,now) = (expire; v := Vis; ghosts := for c ∈ [lo, max(hi_0..hi_v)) where S_v(c) (text_v[c−lo], or blank at or past hi_v) is not what seen read there: {row, col:c, ch: S_v(c) or ' ', ghost}, carrying the newest visible op's born, epoch and seq, where ghost is Vacated at or past hi_v, `Moved { from }` for a glyph of line.text at column from ≠ c whose cell seen read holding that glyph (a shift moved it: a host may paint it as that cell), and Typed otherwise; ghost_cursor := Some((row, S_v.c)) iff v>0, except a carried chain (its ops of another epoch) with `Rule::CarryCursor` off; return &ghosts). seen is the row as the chain last read it: start_chain fills it from the starting press's row, lo to the margin, and every reconcile_chain re-reads it, so a frame composed after the key and before any output is compared with what that press saw: it paints what the state changes (a motion: no cell; an erase: its vacated blank), never the unchanged line, and leaves a moved glyph already standing where the state puts it (a suggestion's same glyph past the line) as it stands. A cell past what was read, which no state reaches (R-ROW), is taken as line.text shows it. Not Engaged ⇒ visibility-gate, ghost_cursor := None. overlay_cursor() = ghost_cursor (&self), which Flush (so reset too) also clears: nothing flushed is displayed, so a host asking before its next overlay draws the engine's cursor. overlay_frame(P,now) = (overlay(P,now), overlay_cursor()) in one borrow (the cells are the same slice overlay returns). A host paints the ghosts as ONE SPAN all or none (aterm-core's paint_prediction_span: Typed a dimmed ghost, Moved the source cell in its own style, Vacated the blank; a host that paints every cell as a ghost still shows the state's text, each cell's ch being its glyph), draws overlay_cursor only when it painted all and the cursor's cell is no wide glyph's right half, and counts a drawn cursor as a displayed prediction (the theorem's ATOMICITY and CURSOR LOCALITY).	src/lib.rs::an_erase_at_the_end_paints_its_vacated_cell_blank, src/lib.rs::a_chain_carried_across_enter_keeps_its_cursor, src/lib.rs::pending_bounds_cover_the_chain_s_line, src/lib.rs::a_chain_composed_before_any_reconcile_paints_only_what_it_changes, src/lib.rs::overlay_frame_is_the_overlay_and_its_cursor, src/lib.rs::a_flush_takes_the_chain_s_cursor_with_it	Vec rebuild into a resident buffer, returned borrow	0,1,2,3,7,13
edit-expiry	contract	Predictor::expire	Engaged ∧ Exp(P,now,front) ⇒ (`Rule::StateExpiryFlushes`) Flush (owing the newest press, which dates every op), expiry_backoff := min(expiry_backoff+1, 3), the head's learned key, if any, contradicted first (learned-bindings: a key bound to nothing waits out its window): every later state is built on the head's effect, so no head-only expiry applies. Off: an Engaged chain never expires. Not Engaged: expiry-contract.	src/lib.rs::an_expired_chain_head_takes_the_chain_with_it	Instant arithmetic, Option	0,1,2,7,12
edit-custody	invariant	every public Predictor method	SINGLE CUSTODY: every key that reached the PTY since the predictor last forgot its session (reset_session; take_link_estimate carries the custody out as owed_at) and was not credited with its echo is held by a pending op of its seq, a typed owe of its seq, or owed_at ≥ its press. Glyph guesses and chain ops share preds; a pop owes its glyph typed; decline-motion, a Backspace with nothing of ours, unmodelled input, a decline, an expiry and every Flush owe; a chain's unattributable retirement and its net-null tail owe.	src/lib.rs::every_key_is_held_until_its_echo_is_credited, src/lib.rs::an_edit_op_dropped_is_owed	ghost history over a trace	0,1,2,10
edit-line-lifetime	invariant	Predictor::{extend_line,settle_chain,void_proof,end_epoch,flush,arm_glyph}	line=Some(l) ⇒ (l.epoch=P.epoch ∧ confirmed_epoch) ∨ Engaged(P). Only extend_line creates or grows it, when a credited glyph g of P.epoch proves as an append (retire_echoed): appended at (l.row, l.cursor = l.hi), else the line restarts at g; a g at column cols−1 drops it (R-ROW). Only a chain's retirement moves it (edit-reconcile). Void drops it while no chain is in flight (a boundary, a cursor the program moved, a tail refusal, an uncredited retirement, an expiry, the standing tail), Flush drops it, settle_chain drops it when the queue drains without the proof, a glyph typed while l.cursor < l.hi that no shift places Voids first (shift-ops), and a shift no edit in place can be told from, of a class unproven, Flushes (shift-ops). R-OWED is implied: the text grows only from credited echoes, so it exists only once no owe was in the window at a credit, and every owe site of the epoch after that drops it or ends the epoch.	src/lib.rs::the_known_line_lives_with_the_proof_it_came_from, src/lib.rs::a_glyph_typed_inside_the_text_is_not_an_append	Option of a struct holding a Vec	0,1,2
stale-anchor	contract	Predictor::{arm_glyph,attributable}	A glyph-lane run anchored on the live cursor (no current-epoch guess to extend) is STALE iff Owed at its first press (owed_at inside W then), or (`Rule::StaleCancel`) it anchors on the cell of a guess a Backspace cancelled (cancelled_at) and that guess was stale (cancelled_stale, set with cancelled_at): its cell was placed from a cursor the owed echo could move, so it is no better an anchor (2026-10-20, the review of increment 5, seed index 3942: the owed echo put the cancelled guess's glyph a cell left of its cell, and `aac` typed on that cell was credited a cell behind); with such a cell, arm_inside places no shift there. Each guess of the run carries stale, no chain op does. For a stale guess h, Attr(P,now,h) also requires (`Rule::StaleAnchor`) ¬Owed(P,now), no pending Insert g with g.seq > h.seq and g.ch = h.ch, and no typed owe (seq, ch, t) with seq > h.seq, ch = h.ch and t inside W(P) (a later guess a Backspace cancelled, whose echo still lands): the owed echo may move the cursor the run was placed from, and an erase we did not place moves it back, so a guess's cell then receives the NEXT key's echo. Hence a run placed before such an erase lands is never credited by a repeated glyph, pending or cancelled (§5.18, §5.21).	src/lib.rs::a_stale_anchor_is_not_credited_by_the_next_key_s_echo, src/lib.rs::a_cancelled_stale_guess_anchors_a_stale_run	VecDeque scan, Option<Instant>	0,1,2,12,13
shift-ops	contract	Predictor::{arm_glyph,arm_inside,arm_in_chain,predict_edit,start_chain,retire_chain,in_place_worlds,world_step,proven_for,class_proven,pair_cancels,credit_op}	A SHIFT is an op typed inside the known text: an Insert at the chain's cursor E.c < E.hi (class InsertShift) or an Erase at E.lo < E.c < E.hi (class EraseShift), whose state moves the text from the cursor one cell right or left (edit-fold). Armed: (a) not Engaged, preds=[] and the line of P.epoch with its cursor left of its end, a glyph press that read its row (arm_inside): ¬editing_off, the glyph one cell wide, proof_cell = the live cursor (cancelled_at, else cur), the standing proof held to that row (standing-tail (b)), Placeable, E.hi+2 ≤ cols (R-ROW: the text's end, one cell on, short of the last column) and the row clear of opaque cells in [E.lo, min(E.V+1, cols)) ⇒ start_chain (seen and the tail snapshot := the row from lo to the margin), push the Insert, Armed; otherwise the glyph lane's, after Void (edit-line-lifetime); (b) Engaged, by arm_in_chain (predict-accept-decline); a glyph inside the text the chain cannot place is owed and Flushed (`Rule::MidLineDeclineFlushes`; off: declined with its pixels kept, which FIFO makes safe too: its echo lands after every op in flight, so the theorem has no counterexample, §5.20); (c) a Backspace inside the text by predict_edit, under R-VACATE (edit-key-routing). Each counts shifts_armed. Shown only once a shift of its kind is proven in the op's epoch (edit-class-proof; `Rule::InsertShiftProof`, `Rule::EraseShiftProof`). THE IN-PLACE WORLDS (in_place_worlds, world_step), asked at a retirement at j* (edit-reconcile) while some pending g has a shift class not in proven_for(g.epoch) (proven, or carried_proven for grandfathered_epoch): U = the shift kinds {Insert, Erase} unproven for some pending op's epoch; for each nonempty F ⊆ U the world W_F folds preds onto the verified line (cells [lo, hi) its text, [hi, V_0) blank, the rest unknown; buffer length |text|, cursor c−lo): an Insert of a kind in F with cur < len replaces cells[cur] and moves the cursor on, an Erase of a kind in F with 0 < cur < len blanks cells[cur−1] and moves the cursor back (readline's overwrite mode), every other Insert or Erase acts as the shift (edit-fold, the vacated cell blank), ← moves back, → moves on inside the buffer and stays at its end (the beep); ← or an Erase at cur 0, or an Insert with len+2 > cols−lo, is UNKNOWN. W_F SHOWS the screen at m when cur = Some((row, lo+cur_m)) and every known cell i holds seen[i]; it is OWN at m when m ≥ j* and its buffer and cursor are S_m's (no op it edits in place has told it apart yet, so what it shows is the chain's own evidence, which the smallest match reads; before j* the chain's own state did not match). Fewer ⇔ some W_F shows at m < j*, or meets UNKNOWN at m < j*; Other ⇔ some W_F shows at some m not own, or meets UNKNOWN; Proves ∋ K ⇔ K ∈ U and every W_F with K ∈ F neither shows at any m nor meets UNKNOWN. With Fewer the retirement credits nothing and Flushes (`Rule::InPlaceReadsFewer`, ambiguous_shifts); with Other it credits its ops and then Flushes (`Rule::ShiftProofDistinguishes`, ambiguous_shifts): the buffer may be a cell shorter or hold another glyph under the cursor, so the state is no known line and the proof naming its cursor goes; otherwise a credited shift proves its class iff its kind ∈ Proves (rule off: always). Why: a tail of spaces moved one cell is the same screen as one left in place, and so is a lone glyph moved onto a cell where a suggestion's same glyph stood: matched there and taken as the line, a line one cell longer than the buffer showed a shift and a `→` overwrite mode did not make (§5.23); and a burst whose intermediate state no frame showed, a rubout then a glyph in overwrite mode (`a c`, the transit wait, then `aoc`, the screen of both shifts), differs op by op from each edit in place against the fold's own intermediate state, which proved both kinds; and overwrite mode's screen after k keys can be the chain's state after more (`c`, `←`, `b`, `→`, ⌫ read as `←b`: `b`), which credited keys never read (§5.25). A glyph and the Backspace after it cancel only with PairCancels (backspace-pops-own); without it the Backspace is a chain op, (c). Cost: at most three worlds of n ops over cols−lo cells, only at such a retirement.
	src/lib.rs::an_erase_inside_the_text_shifts_its_tail, src/lib.rs::a_first_shift_stays_dark_until_its_echo_proves_the_class, src/lib.rs::a_shift_of_spaces_proves_nothing, src/lib.rs::a_lone_glyph_moved_past_the_line_proves_nothing, src/lib.rs::a_glyph_the_chain_cannot_place_inside_the_text_flushes_it, src/lib.rs::a_shift_into_the_last_column_is_not_placed, src/lib.rs::a_burst_overwrite_mode_also_shows_proves_no_shift, src/lib.rs::a_screen_overwrite_mode_shows_with_fewer_keys_credits_none, src/lib.rs::an_erase_before_a_tail_of_spaces_proves_nothing	&mut self, slice windows over Vec<char>, VecDeque push_back/pop_front, u8 bitset, Option chains	0,1,2,3,5,7,13
right-to-left-rows	contract	Predictor::{arm_glyph,arm_in_chain,press_row_clear}, reorders, row_reorders, aterm_bidi::has_bidi	Reorders(ch) ⇔ aterm_bidi::has_bidi([ch]): ch ≥ U+0590 with Bidi_Class R, AL, AN, RLE, RLO, RLI or FSI, a character that makes the engine's UAX #9 reorder draw its row in another order. RowReorders(row) ⇔ some cell's legacy() is Some(ch) with Reorders(ch). With `Rule::RightToLeftRows` (off: neither check): (a) a glyph not Engaged with Reorders(ch), or with its press row Some(row) and RowReorders(row), is declined as predict-accept-decline declines a printable (owe now, EndEpoch, anchor_lost, declined_tail+1) before any shift is tried; (b) arm_in_chain places no glyph with Reorders(ch): inside the text it is owed and Flushed (shift-ops), at the end declined; (c) predict_edit refuses a press row with RowReorders: ←/→ are declined with their pixels kept, a Backspace is predict_backspace. Each counts bidi_refusals. Only a run's first press reads its row, so a row output reorders during a run is seen at the next run. Why: a right-to-left run is drawn reversed with the spaces and numbers between its letters, a key's echo there lands where UAX #9 draws it and may move its neighbours, and a lone right-to-left letter keeps the identity order (no map, no opaque cell), so a line proved on one: a space and two letters typed after `א` were shown `א אא` while the row settled to `אא א` (§5.22).	src/lib.rs::a_right_to_left_row_is_never_guessed_on, src/lib.rs::a_right_to_left_row_takes_no_edit_key	cross-crate class table (aterm_bidi::has_bidi), slice any over Seen, &mut self	0,1,3,5,7,13
line-edge-witness	contract	Predictor::{predict_edit,predict_line_edge,witnessed,arm_glyph,extend_line}	THE WITNESS (R-WITNESS). THE FIRST KEY's gate `prompt_gate` ∈ {Shut, Open{after}}: Shut at new() and after reset() (so after reset_session, take_link_estimate and restore_link_estimate) and a mode change (set_mode to another mode), but kept as it stood across a coordinate move (observe_coordinates runs reset() and puts the gate back, `Rule::ScrollKeepsFirstKey`; off: Shut as reset() leaves it: the terminal's own rows moved, every guess goes as at reset(), and no key went past the predictor, so the next key handed over is still the first since the terminator; shut there, no line was witnessed at a shell on the bottom row, whose Enter scrolls the screen before the next prompt's first key, measured by tests/editing_win.rs, 2026-10-20, with the rule on and off; the model's RowsMoved, visibility-model-refinement); note_line_submit_at(prompt, now) sets it Open{after: prompt} for prompt = Some(after), else Shut, then does what note_line_submit did (owe(now), EndEpoch, declined_tail := 0); note_line_submit(now) = note_line_submit_at(None, now); and every key entry point takes it (first_key: read and set Shut) before anything else, armed or not: arm_glyph (predict_glyph, predict_char, predict_char_in_grid), predict_edit, predict_backspace and note_unmodelled_input. A run's first press (arm_glyph with nothing of its epoch to extend) sets run_witness := witness_of(s.input_start, the gate it took, its guess's cell): s.input_start = Some(i) with i.cell the guess's cell, and (`Rule::WitnessFirstKey`; off: any gate) the gate Open{after} with after ≠ i.prompt; else None. extend_line's restart at a credited append p sets l.fresh := Some(run_witness.prompt) when run_witness.cell = (p.row, p.col), else None, counting witnessed_lines; an append and every state a chain retires to keep it. Witnessed(P,s,E) ⇔ s.input_start = Some(i), i.cell = (r,c) ∧ r = E.row ∧ (`Rule::WitnessFreshPrompt`; off: true) line.fresh = Some(i.prompt) ∧ c = line.lo. predict_edit hands a learned key k (Home, End, CtrlA, CtrlE, Delete) to predict_line_edge after its common prelude: editing_off ⇒ note_unmodelled_input, Declined; k ∈ {CtrlA, CtrlE} ∧ s.escaped_chords ⇒ kitty_refusals+1, note_unmodelled_input, Declined (R-KITTY: they go out as Kitty's CSI u or modifyOtherKeys level 2's CSI 27); observe_context(s.context) (learned-bindings); Banned(k) ⇒ note_unmodelled_input, Declined; ¬Engaged ⇒ standing_at_press(s.row, s.cursor) (standing-tail (b)); E = Placeable(P, s.cursor) kept only if Witnessed(P,s,E) and press_row_clear(s.row, E, k = Delete) (right-to-left-rows, R-OPAQUE, and for Delete R-VACATE). The op is LineStart(k) for Home and CtrlA, armed iff E.c > E.lo; LineEnd(k) for End and CtrlE, iff E.c < E.hi (`Rule::EndRefusal`; off: at E.hi too); Delete iff E.c < E.hi; each only while |preds| < EDIT_CHAIN_MAX: start_chain if ¬Engaged, push {op, class LineEdge (DeleteFwd for Delete), probe := `Rule::LearnedBindings` ∧ ¬Learned(k)}, edit_ops_armed+1, line_edge_armed+1 and probes_armed+1 for a probe, Armed. Otherwise, with E kept and a LineEnd at E.c ≥ E.hi, decline_motion (R-END: zsh-autosuggestions and fish accept the suggestion there, which no state names, and whatever the far end does it reads the key after every key in flight); else note_unmodelled_input, Declined (Home at lo and Delete at the end change nothing canonically and teach nothing). Why: the B (aterm-core's input-region-start: authenticated, and saying it stands where the buffer starts, which zsh's after a pre-filled line does not) vouches for the buffer it found empty, and only the first key typed after it meets that buffer: a run started at its cell by any later key may have text right of it the screen draws as blanks (a literal tab, spaces stepped back over by a key not modelled), or stand at a B the shell left behind when it redrew the prompt narrower (zsh's reset-prompt sends none). A line started there by the first key has lo at the buffer's start and, every key since placed and verified (any other ends the epoch and drops the line), its text the whole buffer, so the canonical effects are states the far end reaches; a line started after a Tab completion begins mid-buffer, where Home went to the buffer's start while the state put the cursor on the line's. The gate opens only at a terminator pressed at an authenticated prompt and only for ANOTHER prompt's B: an Enter the line editor kept (a completion menu's pick) leaves the same prompt in front, and one pressed while a command runs sends the keys typed ahead into the next prompt's buffer.	src/lib.rs::a_learned_key_is_placed_only_on_a_witnessed_line, src/lib.rs::ctrl_a_under_a_kitty_mode_is_not_modelled, src/lib.rs::end_at_the_end_of_the_text_is_declined_with_its_pixels_kept, src/lib.rs::home_at_the_start_and_delete_at_the_end_are_not_modelled, src/lib.rs::delete_erases_the_glyph_under_the_cursor_and_shifts_the_tail, src/lib.rs::only_the_first_key_of_a_prompt_starts_a_witnessed_line, src/lib.rs::a_scroll_after_a_line_terminator_keeps_the_first_key_s_gate, src/lib.rs::a_b_left_behind_by_a_narrower_redraw_witnesses_nothing, src/lib.rs::delete_vacates_only_beside_blanks	&mut self, Option<(u16,u16)> compare, enum match with guards, VecDeque push_back/back_mut	0,1,2,3,13
learned-bindings	contract	Predictor::{observe_context,credit_binding,contradict_head,retire_chain,reconcile_chain,expire,op_visible,visible_count,take_link_estimate,restore_link_estimate,reset_session}, EditorContext::new	bindings = (context: Option<EditorContext>, per learned key k: seen ≤ 2, contradictions ≤ 2); Learned(k) ⇔ seen ≥ 2; Banned(k) ⇔ contradictions ≥ 2. observe_context(c), at each learned key's press: bindings.context = Some(c) ⇒ nothing; else, with a pending op of a learned key, Flush (its echo is the old editor's answer), then bindings := (Some(c), every key zero). A chain's retirement (edit-reconcile) that credits its ops and is not left Other by the in-place worlds (shift-ops) credits each credited op of a learned key k once: ¬Banned(k) ∧ ¬Learned(k) ⇒ seen+1, bindings_learned+1 on reaching 2. An Engaged chain's divergence (edit-reconcile: no state matches and no transit) and its expiry (edit-expiry) CONTRADICT the head's learned key, if any: seen := 0, contradictions := min(contradictions+1, 2), bindings_unlearned+1. Display: op_visible of class LineEdge or DeleteFwd ⇔ ¬g.probe, with the epoch and glyph-proof terms (edit-class-proof), and Vis in Always stops at the first probe (visibility-gate), so nothing after a probe shows in any mode. EditorContext::new(session, nonce, DECCKM, escaped_chords) compares the four, the nonce as its FNV-1a digest (no capability held, none printed); escaped_chords is the host's KeySample::escaped_chords (aterm-core's control_chords_escaped: a Kitty mode that reports functional keys, or modifyOtherKeys level 2). Link carries bindings (link-estimate-swap): reset keeps them, reset_session clears them. Why: no binding is assumed (zsh without its integration leaves Home unbound and inserts `~` for Delete, zsh's vi insert mode inserts Ctrl-A); a probe is armed only where its canonical effect changes the state, so a key bound to nothing waits out its window and one bound to a glyph diverges, each a contradiction, and two agreeing credited echoes, each a whole state the screen matched at the smallest match, are the evidence the binding is canonical in this context. A keymap switched inside one context (a `bindkey -v` typed at the shell) shows a learned key wrong for at most one window, until its contradiction (residual 4).	src/lib.rs::an_unlearned_key_is_a_dark_probe_until_two_echoes_teach_it, src/lib.rs::nothing_after_a_probe_is_shown_even_in_always, src/lib.rs::a_contradicted_binding_is_unlearned_and_a_second_bans_it, src/lib.rs::bindings_hold_for_their_editor_context_and_travel_with_the_session, src/lib.rs::a_new_editor_context_flushes_the_old_one_s_ops, src/lib.rs::a_divergence_a_learned_key_heads_unlearns_it	&mut self, fixed array of small Copy structs, Option<struct> equality, u64 wrapping_mul fold, saturating u8	0,1,2,13
```

Merge notes:
- `glitch-window` gained gaps 1 and 2 here. It is a `&self` method (gap 2) that reads `srtt: Option<f32>`
  (gap 1). The chunk had listed only 3, 6, 12 and 13.
- `every_epoch_boundary_is_the_models_submit` appeared in the subsumes list of both
  `no-unechoed-flash` and `visibility-model-refinement`. It is a conformance test of the model, so its
  primary is `visibility-model-refinement`. `no-unechoed-flash` is its secondary (§2).
- No two specs stated the same property, so nothing was merged away. The building blocks `Flush`,
  `EndEpoch`, `W` and `Vis` are defined once, in §0, and every spec cites them from there.

## 2. Test → spec map (TSV; all 162)

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
src/lib.rs	a_glyph_typed_over_the_text_its_press_saw_waits_for_its_echo	reconcile-contract	tail-guard
src/lib.rs	a_right_to_left_row_is_never_guessed_on	right-to-left-rows	tail-guard, seen-opacity
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
src/lib.rs	a_glyph_typed_inside_the_text_is_not_an_append	edit-line-lifetime	predict-accept-decline, shift-ops
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
src/lib.rs	a_chain_composed_before_any_reconcile_paints_only_what_it_changes	edit-overlay	edit-key-routing
src/lib.rs	overlay_frame_is_the_overlay_and_its_cursor	edit-overlay	visibility-gate
src/lib.rs	a_flush_takes_the_chain_s_cursor_with_it	edit-overlay	link-estimate-swap
src/lib.rs	glitch_window_ewma_and_ghost_wire_codec_round_trip	glitch-window	rtt-only-from-echo
src/lib.rs	an_erase_inside_the_text_shifts_its_tail	shift-ops	edit-class-proof, edit-overlay
src/lib.rs	a_first_shift_stays_dark_until_its_echo_proves_the_class	shift-ops	edit-class-proof
src/lib.rs	a_shift_of_spaces_proves_nothing	shift-ops	edit-reconcile, edit-line-lifetime
src/lib.rs	a_lone_glyph_moved_past_the_line_proves_nothing	shift-ops	edit-reconcile, edit-fold
src/lib.rs	a_glyph_the_chain_cannot_place_inside_the_text_flushes_it	shift-ops	predict-accept-decline
src/lib.rs	a_shift_into_the_last_column_is_not_placed	shift-ops	edit-line-lifetime
src/lib.rs	a_chain_beside_a_right_prompt_holds_the_row_past_it	standing-tail	edit-reconcile
src/lib.rs	a_credited_shift_re_bases_the_standing_proof	standing-tail	edit-reconcile
src/lib.rs	an_edit_press_holds_the_standing_proof_to_its_row	standing-tail	edit-key-routing
src/lib.rs	a_burst_overwrite_mode_also_shows_proves_no_shift	shift-ops	edit-reconcile, edit-class-proof
src/lib.rs	a_screen_overwrite_mode_shows_with_fewer_keys_credits_none	shift-ops	edit-reconcile
src/lib.rs	a_backspace_cancels_no_glyph_of_an_unproven_shift	backspace-pops-own	shift-ops, edit-key-routing
src/lib.rs	an_arrow_with_a_run_in_flight_holds_the_standing_tail	standing-tail	edit-key-routing
src/lib.rs	a_blank_row_behind_a_declined_arrow_is_no_line_end	tail-guard	edit-key-routing, unmodelled-input-retires
src/lib.rs	a_run_started_inside_the_text_proves_nothing	tail-guard	shift-ops
src/lib.rs	an_erase_before_a_tail_of_spaces_proves_nothing	shift-ops	edit-reconcile, edit-line-lifetime
src/lib.rs	a_glyph_typed_inside_the_text_holds_the_standing_proof_to_its_row	standing-tail	shift-ops
src/lib.rs	a_right_to_left_row_takes_no_edit_key	right-to-left-rows	edit-key-routing, shift-ops
src/lib.rs	a_proof_whose_echo_was_redrawn_stands_no_more	standing-tail	reconcile-contract, no-unechoed-flash
src/lib.rs	a_cancelled_glyph_s_erase_is_no_space_s_echo	backspace-pops-own	typed-owes
src/lib.rs	an_erase_caught_part_way_is_no_later_state	edit-reconcile	edit-custody
src/lib.rs	an_unlearned_key_is_a_dark_probe_until_two_echoes_teach_it	learned-bindings	line-edge-witness, edit-class-proof
src/lib.rs	nothing_after_a_probe_is_shown_even_in_always	learned-bindings	visibility-gate
src/lib.rs	a_learned_key_is_placed_only_on_a_witnessed_line	line-edge-witness	unmodelled-input-retires
src/lib.rs	a_contradicted_binding_is_unlearned_and_a_second_bans_it	learned-bindings	edit-expiry
src/lib.rs	bindings_hold_for_their_editor_context_and_travel_with_the_session	learned-bindings	link-estimate-swap
src/lib.rs	ctrl_a_under_a_kitty_mode_is_not_modelled	line-edge-witness	
src/lib.rs	only_the_first_key_of_a_prompt_starts_a_witnessed_line	line-edge-witness	
src/lib.rs	a_scroll_after_a_line_terminator_keeps_the_first_key_s_gate	line-edge-witness	
src/lib.rs	a_b_left_behind_by_a_narrower_redraw_witnesses_nothing	line-edge-witness	learned-bindings
src/lib.rs	a_new_editor_context_flushes_the_old_one_s_ops	learned-bindings	
src/lib.rs	a_divergence_a_learned_key_heads_unlearns_it	learned-bindings	edit-reconcile
src/lib.rs	end_at_the_end_of_the_text_is_declined_with_its_pixels_kept	line-edge-witness	edit-key-routing
src/lib.rs	delete_erases_the_glyph_under_the_cursor_and_shifts_the_tail	line-edge-witness	edit-fold, edit-overlay
src/lib.rs	delete_vacates_only_beside_blanks	line-edge-witness	edit-reconcile
src/lib.rs	text_drawn_under_the_line_voids_its_proof	standing-tail	
src/lib.rs	text_over_a_wrapped_guess_s_cell_voids_its_proof	standing-tail	
src/lib.rs	nothing_shows_behind_a_guess_held_on_its_own_glyph	visibility-gate	reconcile-contract
src/lib.rs	a_cursor_back_on_a_credited_cell_takes_the_credit_back	reconcile-contract	typed-owes
src/lib.rs	a_cursor_on_text_in_the_last_column_is_not_parked	predict-placement	
src/lib.rs	home_at_the_start_and_delete_at_the_end_are_not_modelled	line-edge-witness	
src/lib.rs	a_cancelled_stale_guess_anchors_a_stale_run	stale-anchor	backspace-pops-own
src/lib.rs	a_delete_s_step_back_is_no_later_state	edit-reconcile	learned-bindings
src/lib.rs	a_narrower_redraw_is_no_echo_in_transit	edit-reconcile	
tests/conformance_visibility.rs	adaptive_fast_expiry_is_invisible_and_slow_echo_may_display	visibility-model-refinement	expiry-contract, echo-regime-latch
tests/conformance_visibility.rs	the_display_gate_closes_only_on_sustained_fast_evidence	visibility-model-refinement	echo-regime-latch
tests/conformance_visibility.rs	every_epoch_boundary_is_the_models_submit	visibility-model-refinement	no-unechoed-flash
tests/conformance_visibility.rs	an_owed_echo_proves_nothing_until_its_window_passes	visibility-model-refinement	reconcile-contract
tests/conformance_visibility.rs	expiry_owes_and_voids_the_proof	visibility-model-refinement	expiry-contract
tests/conformance_visibility.rs	an_edit_displays_only_after_its_class_is_proven	visibility-model-refinement	edit-class-proof
tests/conformance_visibility.rs	a_line_edge_key_displays_only_when_learned_at_a_witnessed_prompt	visibility-model-refinement	learned-bindings, line-edge-witness
tests/conformance_visibility.rs	a_scroll_keeps_the_first_key_s_gate	visibility-model-refinement	line-edge-witness, reconcile-contract
tests/editing_win.rs	predictive_editing_wins_and_never_shows_a_wrong_line	edit-class-proof	visibility-gate, line-edge-witness, learned-bindings, edit-reconcile, queue-cap
tests/editing_win.rs	a_cooked_tty_shows_no_motion_and_overwrite_mode_no_shift	edit-class-proof	shift-ops
tests/editing_win.rs	class_proofs_end_with_their_line	edit-class-proof	
```

## 3. Coverage

- 162 tests, each mapped to one of the 37 specs in §2 (the 2026-10-15 recovery: 59 read, 58 mapped to
  19 specs, 1 deleted as §4 says).
- In that recovery the biggest collapses were `predict-placement`, `echo-regime-latch`, `reconcile-contract`,
  `no-unechoed-flash` and `link-estimate-swap` (7 primary tests each), then `expiry-contract` and
  `unmodelled-input-retires` (4 each).
- Gap histogram over the 19 specs of the 2026-10-15 recovery (the number of specs each gap blocks; a
  spec counts once under every gap it lists). The sixteen added since list their gaps in their rows.

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

> 2026-10-20: `tests/editing_win.rs::the_corpus_is_the_region_probe_s` samples no spec. It pins the
> measurement's corpus generator to the region design's probe (6999 keys, the same lines per family), so
> the measurement's numbers and the probe's are of one corpus; a proof of the crate retires nothing of it.

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
    opaque cells from the edit lane waits for increment 3, whose span painter moves cells. Increment 3 adds two
    more without a counterexample: `MidLineDeclineFlushes` (a glyph inside the text a chain cannot place,
    declined with the pixels kept, echoes after every op in flight, so each op's frames settle before it moves
    the tail; the flush stays as the fail-closed choice), and `VacateRefusal` again, now with a far end that
    fetches its suggestion into the cell an erase vacates (`Editor::Autosuggest`): the vacated blank under it is
    late decoration, which the decoration rule counts. A host hiding opaque cells is still not a control: the span
    painter refuses a moved cell whose source is half a wide glyph or carries marks, and a reordered row whole,
    so what such a host hands the predictor never reaches glass.
21. **The sweep over more seeds than the committed one (fixed 2026-10-20, after review of increment 2).**
    Each break below is a named case of the theorem (`edit_review_cases`), shrunk, held by the faithful
    predictor and broken in its clause by its control. Review's sweep found seven: a STALE ANCHOR credited by a
    later key a Backspace had cancelled, which asked only the pending ones (FIDELITY; `StaleAnchor` now asks
    the typed owes too); a guess retired under an autosuggestion drawn with its cursor not yet stepped back,
    which anchored the next run a cell right (FIDELITY; `CursorReach`); a chain's motions credited ahead of a
    re-typed glyph found owed (ATTRIBUTION; `ChainAttributedWhole`); a cancelled glyph's typed owe dated by
    the glyph, lapsing while its erase was on time (ATTRIBUTION; the owe is dated by the Backspace,
    `backspace-pops-own`); a last-column guess credited when ICH echoes pushed the line's own glyph under the
    cursor there (ATTRIBUTION; `ParkedAtTheEnd`); a run typed before a zero-width mark the editor's cursor
    had stepped behind, proved as appends (FRAME FIDELITY; `MarkedEcho`); and a chain's vacated blank under
    an autosuggestion the erase brought up, which the theorem counted as a ghost glyph contradicted (no
    predictor change: the decoration rule now counts a blank ghost under decoration the far end drew, as its
    statement always said). Four of the seven also broke on main before the edit lane. This round then swept
    800 seeds (seed index i is `SEED ^ i·0xD1B5_4A32_D192_ED03`) and found five more: a chain waiting through
    a password prompt printed below it, its cursor brought back up past every state, with the secret typed
    there joining the chain on glass (SECRECY; `TransitReach`); a run typed on a space inside the known text,
    which the tail guard read as typed over nothing (FRAME FIDELITY; `run_inside`), and on the same seed once
    that held, a guess typed on a proof earned between a glyph and its tail redraw and carried past its epoch by
    an arrow declined with its pixels kept, standing on glass after the redraw, since the standing tail ended
    with the epoch (FRAME FIDELITY; the carried standing tail, `standing-tail` (c)); a carried erase credited
    when the Enter's carriage return alone put the cursor on its state (ATTRIBUTION; `EditOwedAfter`); and a
    space typed while `Password: ` was being drawn, credited with the prompt's own trailing space, proving the
    line where the secret was then typed (SECRECY, in the glyph lane; `SpaceNeverProves`). With all of them
    the faithful predictor holds on the 800 seeds. The theorem's transit floor drops (34 part-way echoes to 4
    on the committed seed): a cooked tty's `^[[D` now flushes the chain instead of waiting it out, and
    `WholeStateMatch` is caught at its named case alone.
22. **A run typed after a lone right-to-left letter was shown in the order typed (fixed 2026-10-20, increment
    3).** With a right-to-left letter in the theorem's world (`Config::bidi`), two hundred seeds (seed index i is
    `SEED ^ i·0xA24B_AED4_963E_E407`) found the glyph lane on main breaking FRAME FIDELITY (seed index 125, case 509,
    a readline far end; review found the provenance first written here, seed index 7, to be wrong): `א` typed and echoed
    proves its line, because a lone right-to-left letter keeps the row in logical order (the engine records no
    map, so no cell reads opaque), and a space and two more `א` typed on that proof were painted `א אא`; their
    echoes join one right-to-left run, which the engine draws reversed, and the row settled to `אא א`. Nothing
    positional can follow UAX #9 from a column, so a glyph that makes its row reorder, and any key on a row that
    holds one, is declined (`right-to-left-rows`, `Rule::RightToLeftRows`, the named case
    `a_right_to_left_run_is_never_shown`). The cost is every prediction on a line with right-to-left text in
    it, a prompt's included.
23. **Overwrite mode's echo matched an insert's state beside a suggestion (fixed 2026-10-20, increment 3).** The
    same sweep, with readline's overwrite mode among the far ends, found an echo that replaced the glyph under the
    cursor matching the state of an insert there: the text the insert would move was one glyph, and a
    suggestion's same glyph stood on the cell it would move onto. Retired at that state, the chain made a line one
    cell longer than the buffer, proved the insert class on it, and showed the next insert shifted while
    overwrite mode replaced, and a `→` it beeped at (FRAME FIDELITY; the named case
    `a_shift_a_suggestion_could_show_proves_nothing`). The class proof as first written asked only that the text
    moved hold a glyph, which caught a tail of spaces and not this: a shift is Evident only where it differs from
    an edit in place on a cell whose earlier content the chain knows, and one that is not, with its class
    unproven, leaves the line unknown (`shift-ops`). The cost is the first shift of a line typed before its last
    glyph, which proves nothing and darkens the line until the next proof.
24. **Tail adoption is not implemented (2026-10-20, increment 3, by decision).** The plan's tail adoption had the
    first insert of an epoch begun inside a buffer (after Home, a Tab, a recalled line) adopt the text right of the
    cursor up to its first blank as the known line, once its echo showed that text moved one cell. A suggestion
    moves the same way: zsh-autosuggestions fetches one for the new buffer after every widget that changes it and
    draws it after the buffer's new end, so buffer `l` with suggestion `sxyz`, Home, `x` typed and re-suggested,
    is the screen the adoption expects, cell for cell. Adopted, `lsxyz` showed a `→` one cell on where the editor's
    `→` accepts the whole suggestion, and a glyph typed next moved text the editor clears. Nothing the predictor
    reads (glyphs and opacity, no style) tells suggestion text from buffer text, so an epoch begun inside a buffer
    stays dark until an append proves it, as before. The theorem's `Editor::Autosuggest` now fetches a suggestion
    after edits inside the buffer too, which holds the shifts (whose line holds only echoed text) to that
    behaviour.

25. **Review of increment 3 (fixed 2026-10-20).** Four breaks of the increment and one on main before it, each
    pinned by a unit test that fails without its fix, three also by a named case of the theorem with its control.
    (a) A burst judged op by op: `retire_chain` asked each credited shift whether it differed from its edit in place
    against the fold's state before it, which no frame showed. Overwrite mode's rubout and a glyph typed inside the
    text (`a c`, the transit wait, then `aoc`) leave the very screen of the two shifts, and both kinds were proven;
    `n` typed next was shown shifted (`aonc`) while overwrite mode settled to `aon` (FRAME FIDELITY; the named case
    `overwrite_mode_s_burst_proves_no_shift`, control `ShiftProofDistinguishes`). The IN-PLACE WORLDS (`shift-ops`)
    replace the per-op test: each world is held to the screen at every count of keys, from the verified line.
    (b) Fewer keys read: overwrite mode's screen after `k` keys can be the chain's state after more (`c` echoed,
    `←`, `b`, `→` and a Backspace typed, `b` shown after the far end read `←b`, which is the chain's state after
    all four), and the smallest match credited `→` and the Backspace before either was read (ATTRIBUTION;
    `Rule::InPlaceReadsFewer`, the named case `overwrite_mode_s_screen_with_fewer_keys_credits_none`).
    (c) A Backspace after a glyph typed inside the text popped it as a pair that cancels, whatever its kind: overwrite
    mode's glyph and rubout erase a glyph, and the chain's `←` then showed a line the editor no longer had (FRAME
    FIDELITY; a regression of the increment, since on main such a glyph flushed the chain; `Rule::CancelsOnlyKnownShifts`,
    the named case `a_backspace_cancels_no_glyph_of_an_unproven_shift`, the review's shrink of seed index 152, case 65
    with `VacateRefusal` off). With the fix that seed holds with `VacateRefusal` off too.
    (d) The standing tail at an edit press ran only with nothing pending, while `start_chain` re-snapshots the row
    whatever is pending and the chain's reconciles hold only the row past its states: a run's glyph echoed before its
    tail redraw proved on the frame between, `←` pressed after the redraw and before the next frame re-snapshotted the
    moved row, and the run's next glyph was shown over it (`an_arrow_with_a_run_in_flight_holds_the_standing_tail`).
    The theorem's world draws a frame after every delivery, before the next press, so it cannot reach it; a press
    that starts a chain now never skips the tail, a cancelled pair's transit included (`standing-tail` (b)).
    (e) On main before the increment: a run typed while a declined `←` was still owed read a blank row, took itself
    to be at the end of the line and proved without comparing its tail, though the far end inserted it before the
    line's last glyph, and the glyph typed next was painted over that glyph (FRAME FIDELITY, found by review;
    `a_blank_row_behind_a_declined_arrow_is_no_line_end`). A declined arrow and a key not modelled now make the
    line's end unknown for a window (`tail-guard`).
    Evidence the review found missing, now in place: the `run_inside` guard is `Rule::InsideRunsNeverProve`, with its
    unit test and control (this twelve-column world cannot reach it inside the editor's model: `rule_clause`); unit
    tests pin the in-place rubout, `arm_inside`'s standing check and both `RightToLeftRows` checks, each failing
    when its guard is removed; the `RightToLeftRows` provenance is seed index 125, case 509 (seed index 7 does not
    break with the rule off, on this tree or before it); the `edit-overlay` row no longer says a chain starts with
    nothing read. aterm-core's span painter now paints a moved cell whose source is its own column, and its order
    test reads styles (aterm-core `paint-prediction-span`). The review's seed numbers for (a), (b) and (e), and for the
    breaks on main it listed (its indices 761, 829, 1022, 1456, 1963, 1496, 701 and 391), name no failing case of
    this harness's sweep at those indices; the classes they describe are either fixed here or are §5.26's.
26. **Two thousand seeds (2026-10-20, after the review of increment 3).** The theorem's sweep over seed indices
    0..2000 (`SEED ^ i·0xA24B_AED4_963E_E407`, the eight shipped configurations, six hundred cases each) failed
    nineteen cases on the tree the review read (ten of them ATTRIBUTION in overwrite mode, §5.25 (b)) and seven
    with §5.25's fixes. Five of the seven were the world claiming too much: an unprompted right prompt (`<rp>`)
    drawn plain, cleared by a line editor's `\b ESC[K` or a suggestion's blanking beside correct ghosts, which FRAME
    FIDELITY counted as a wrong frame though nothing the predictor painted moved; it and the unprompted suggestion
    are now drawn in italics, as every right prompt and suggestion of the world is, so the decoration rule counts
    them. Two were breaks on main before increment 3, in the glyph lane:
    - SECRECY (seed index 1121, case 571): `i` and a space typed, `i`'s echo proving; the space's echo, a password
      prompt printed on the same row, and a screen clear that redrew the shell prompt landed in one burst, the
      cursor back on the space's cell; the redraw's trailing space credited the space, which carried the proof `i`
      had earned, and `8`, typed at the password read, was painted. A proof now stands only while its echo's own
      cell holds its glyph (`standing-tail` (e), `Rule::StandingGlyph`, the named case `a_redrawn_line_keeps_no_proof`).
    - ATTRIBUTION (seed index 1200, case 44): `n` typed and cancelled, a space typed on the cancelled cell, and zle's
      erase (`\b \b`) caught after its blank and before its step back, a space's echo exactly: the space was credited
      before the far end read it. The erase's echo is owed as a space (`backspace-pops-own`,
      `Rule::CancelOwesItsBlank`, the named case `a_cancelled_glyph_s_erase_is_no_space_s_echo`).
    The same two cases, run whole after those fixes, still failed further on, and each shrank to a second form:
    - ATTRIBUTION (seed index 1200, case 44): the same erase caught part-way in the edit lane, `n` proven and a
      Backspace and a space typed after it as a chain, whose state after both keys is that very screen: retired
      there, the space was credited unread, and its own echo then credited the space typed next. A state an
      earlier op's echo shows part-way (zle's erase before its step back, readline's insert with its tail written)
      is no match (`edit-reconcile`, `Rule::PartWayEchoes`, the named case
      `an_erase_caught_part_way_is_no_later_state`). The theorem's ATTRIBUTION scope leaves out a credit made while
      an earlier key's output is part-way, which is why only the later credit it led to was seen.
    - SECRECY (seed index 1121, case 571): the world claiming too much. Its far end, reading a password with echo
      off, redrew the buffer with the line on a screen clear (`$ s`), echoing a secret key, which the predictor then
      took for that key's echo. A far end that displays the keys typed at its no-echo read is one SECRECY cannot
      hold; the world's redraw at a no-echo read now shows the prompt alone.
    With every one of them fixed, the same two thousand seeds find no failing case, and no frame in any of them
    displays a shift while the far end is in overwrite mode (the class ladder's claim, held there as on the
    committed seed). Two hundred of them, run with `VacateRefusal`, `MidLineDeclineFlushes` or
    `InsideRunsNeverProve` switched off, find none either: §5.20's claim for the first two holds again, which
    the review had refuted for `VacateRefusal` at seed index 152 before (c).
27. **Review of increment 4 (fixed 2026-10-20).** The witness took a `B` for the buffer's start wherever a
    run was typed at its cell, and three things the theorem's world never did made that wrong:
    (a) zsh draws a line pre-filled from its buffer stack (push-line, accept-and-hold, `print -z`) before
      zle-line-init runs, so the integration's `B` landed after the text (measured on zsh 5.9: `P> echo hi`
      with the mark at column 10), and `x` typed there started a "witnessed" line at the text's end, Home
      shown there while zle went to the buffer's start. aterm's integration now says `aterm_input=start` on
      a signed `B` only where it stands at the buffer's start (bash's and fish's, at the end of the prompt;
      zsh's only over an empty buffer), and aterm-core names a start only for such a `B`
      (`input-region-start` there). The world pre-fills a prompt from a pushed line (`Unmodelled::PushLine`,
      the zle `B` after the text, the readline one before it), and a host that ignores the word is caught at
      `a_line_pre_filled_from_the_buffer_stack_has_no_buffer_start`.
    (b) A run started at the `B`'s cell after other keys meets the buffer they left: two spaces typed, Home
      at the start not modelled (the line gone), `c` typed at the `B`, and End shown after `c` while the far
      end went past the spaces (a literal tab, which zle draws as blanks, the same). (c) zsh's `reset-prompt`
      redraws the prompt narrower and sends no new `B`, which then stands mid-buffer under the cursor. THE
      FIRST KEY (`prompt_gate`, `Rule::WitnessFirstKey`) fixes both: only the first key typed at a prompt,
      after a terminator pressed at another authenticated prompt, starts a witnessed line; any key, a reset or
      a mode switch shuts the gate. Named cases `only_the_first_key_meets_the_empty_buffer` and
      `a_b_left_behind_by_a_narrower_redraw_witnesses_nothing` (the world's `Unprompted::ResetPrompt`) catch
      the rule switched off. Two host mutants lost their bite to the gate: a remembered start is refused
      at the prompt its terminator was pressed at, so `StaleInputStart` forgets the prompt as well; a
      program's unsigned mark after a Tab is no first key, so `UnsignedInputStart`'s case is a forged mark on
      a pre-filled line now.
    (d) Under xterm's modifyOtherKeys at level 2 Ctrl-A goes out as `CSI 27;5;97~`, which R-KITTY did not
      know: `KeySample::escaped_chords` (aterm-core's `control_chords_escaped`, bound to the encoder by its
      test) now covers both, named case `ctrl_a_under_modify_other_keys`.
    Evidence the review found missing: `bindings_hold_for_their_editor_context_and_travel_with_the_session`
    restored the bindings it then found (now `reset_session` alone, DECCKM and the escaped chords as
    contexts); `a_new_editor_context_flushes_the_old_one_s_ops` and
    `a_divergence_a_learned_key_heads_unlearns_it` hold the flush and the divergence's contradiction, each
    failing with its clause removed. aterm-core's two re-key conjuncts gained a negative control each.
    The review's sweep of two thousand seeds also found six breaks, five of them on main before the increment;
    each is a named case of the theorem now (`review_4_cases`). (e) SECRECY, seed index 1206: on a cooked tty,
    ` sie` echoed and proving, then `\r\nPassword: ` on the next row and a reverse index back onto the proof's cell,
    in one burst; every cell of the line held its glyph and the cursor stood where the proof said, so the secret
    typed there showed. THE FLOOR (`Rule::FloorBelow`, standing-tail (f)): the run's first press hands the row
    under the line too (`KeySample::below`), and text drawn where it read a blank voids the proof. (f) SECRECY,
    seed index 1909: ` Password: ` printed after `sad` on the same row and a reverse index back onto `d`, which
    then waited as a guess typed onto a suggestion waits, and the secret typed next showed after it. The
    wrapped `:` under the line closes it by the floor; a prompt that wraps only a blank (the cursor moved back
    onto the guess by a jump) is closed by DARK BEHIND A WAIT (`Rule::DarkBehindAWait`, visibility-gate): a guess
    waiting on its own glyph that its press did not see shows nothing typed after it until it retires. Flushing
    such a guess instead was measured to darken every run typed ahead along an autosuggestion, the very case
    the wait exists for. (g) ATTRIBUTION, seed indices 784 and 2115: an autosuggestion drawn part-way, its first
    glyph the typed one with the cursor past it, credited the guess (out of the clause's scope, output nobody
    typed for), and once the cursor stepped back the guess typed next was credited with the first key's real
    echo. THE CREDIT TAKEN BACK (`Rule::CreditTakenBack`, reconcile-contract): the cursor found on or before the
    newest credited cell owes that key's echo again. It also closes `a_cancelled_glyph_s_erase_is_no_space_s_echo`
    by itself, so `CancelOwesItsBlank` has no consequence left in this world and is pinned by its unit test (its
    clause is `None`). (h) FRAME FIDELITY, seed index 1202, the world's: its far end drew a prompt and a right
    prompt on a row a wrapped line had left text on, and the erase that later hid the right prompt took that text
    with it, the frame before a correct ghost. zsh clears that row (PROMPT_SP), and so does the world now.
    (i) FIDELITY, seed index 1841, reachable once the keymaps came in: readline's overwrite mode left its cursor
    on its own `d` in the last column, which the last reconcile read as the deferred-wrap park, and `i` was put on
    the next row while overwrite mode wrote it over `d`. The press places by the engine's own flag
    (`KeySample::wrap_pending`, `Rule::EngineWrapFlag`, predict-placement).
    Evidence: a learned Delete is shown shifting text in the theorem (the line-edge motif teaches it, and
    `a_learned_delete_shifts_the_tail` is a named case), its unit test asserts the moved cell, and
    `delete_vacates_only_beside_blanks` fails with either half of Delete's R-VACATE removed.
    A sweep of seed indices 0 to 2400 with those fixes found three more breaks, each a named case now:
    (j) SECRECY, seed index 2146: the floor skipped a row a guess typed ahead had wrapped onto, so the same
    prompt-and-reverse-index burst beside it went unread; the floor now reads every cell no guess in flight
    names, while the cursor is above it (on it, the line wrapped there and the editor writes it).
    (k) FRAME FIDELITY, seed index 778, the engine's: a combining mark on the prompt's trailing space vanished
    from the frames once `\b ESC[K` erased a glyph after it (the row recounted from cell bits alone) and came
    back with the next glyph written there, the frame a guess was shown in lacking it. aterm-grid's EL 0 now
    keeps a marked blank in the row (`Grid::keep_len_over_marks`, its `el-span`). (l) FRAME FIDELITY, seed
    index 1287, the world's: its zle rubbed out a glyph whose tail began with a zero-width mark, redrawing the
    mark onto the cell before the cursor (or, at a row's start, onto the cell its blank then took) while its
    buffer kept it after the cursor; such an erase is beyond the model now, as an ICH in front of a mark is.
    A third sweep of the same indices found one more, a named case now: (m) SECRECY, seed index 2146 again, case
    131: with more typed ahead, every cell `Password:` was printed over was one a guess wrapped onto the floor
    named, so (j)'s floor, excusing any text on such a cell, read only the blank after the colon. A guess's cell
    excuses its own glyph now (its echo landing early) and nothing else (standing-tail (f),
    `text_over_a_wrapped_guess_s_cell_voids_its_proof`, which fails with the old excuse). A fourth sweep of
    the same indices, over all eight faithful configurations, found none.

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
