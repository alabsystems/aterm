// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE SAFETY THEOREM of predictive local echo: a prediction can never corrupt
//! the session, for ANY interleaving of typing, program output and time.
//!
//! The donor is astream-term's verified speculation
//! (`vendor/astream/crates/astream-term/tests/suite/predict.rs`,
//! `predictions_never_corrupt_the_final_screen`): there the reconciled view
//! equals the pure authoritative fold for any interleaving of predictions and
//! output. This file states and checks the same theorem for `aterm-predict` as
//! aterm actually wires it, against aterm's real engine.
//!
//! ## What "the reconciled view" is in aterm
//!
//! The AUTHORITATIVE view `A` is the engine's active screen: `aterm-core`'s
//! `Terminal`, fed the program's output and nothing else, read through the same
//! extraction the GUI paints from (`cell_frame_into` -> `RenderInput::cells`,
//! columns through `visual_col`). The predictor never writes it: it sees `A` only
//! through the `observe` closure the host hands `reconcile`.
//!
//! The OVERLAY `O` is what `Predictor::overlay(now)` returns on a frame, after
//! that frame's `reconcile` against `A` (aterm-gui's order: reconcile against the
//! unpainted snapshot, then overlay). The DISPLAYED view `D` is `A` with the
//! guesses in `O` painted by the painter aterm-gui runs on its frames
//! (`paint_prediction_ghosts` is a loop over aterm-core's
//! `RenderInput::paint_prediction_ghost`; a state chain's cells are one span
//! through `RenderInput::paint_prediction_span`, which paints text a shift moved
//! as the cell it came from), so `D` is what goes on glass, every field of every
//! cell, the grapheme clusters and combining marks included; the GUI also draws
//! its cursor one past the newest guess, or a chain's own, which changes no cell.
//!
//! A guess's coordinates are absolute (row, col) in the COORDINATE SPACE it was
//! armed in: one content-coordinate generation (`ContentScrollState::
//! coordinate_generation`, which moves exactly when rows move under the screen),
//! one screen (main, not the alternate screen) and one host geometry (a resize or
//! a pane swap resets the predictor). Painting a guess in any other space paints
//! a cell it never predicted: the row now holds some other line.
//!
//! ## The theorem
//!
//! On every frame, for every interleaving this file generates:
//!
//! * **Locality.** `D` differs from `A` only at the cells of `O`, and never on
//!   half of a wide glyph (the renderer draws a wide glyph as one unit across
//!   its lead and continuation, so a ghost on either half changes the other,
//!   which no guess predicted); every guess in `O` is inside the grid; no two
//!   guesses in `O` share a cell (two guesses on one cell is a typed glyph
//!   painted over another typed glyph).
//! * **Provenance.** Every guess in `O` is a keystroke the host predicted, in
//!   the coordinate space it was armed in — hence nothing is painted while the
//!   alternate screen is up, and nothing armed before rows moved survives the
//!   move — at the cell, with the glyph, it was armed with. (The last part pins
//!   that a guess never moves or changes once armed; the space is the part a
//!   host can get wrong.) A STATE CHAIN (the edit lane: `←`, `→`, and glyphs and
//!   erases at the end of the text or inside it, which SHIFT the text right of
//!   the cursor, armed on the line its epoch watched echo) is held to an ORACLE
//!   instead (`Sim::oracle_check`): from the screen `A` showed
//!   when the chain last retired to a state it matched, fold the keys of its
//!   displayed ops, independently of the predictor's fold; the frame must show
//!   that text on the chain's row from its `lo` to the furthest end any of
//!   those states reaches (a blank on every cell it vacated), with the cursor
//!   the fold leaves. Only `lo` and the text's end come from the predictor.
//! * **Secrecy (Adaptive).** A key typed into a no-echo prompt the program has
//!   visibly put up (`Password: ` delivered, echo off) is never in `O`, even on a
//!   remote link where the host cannot read the far end's termios.
//! * **Fidelity (Adaptive).** A displayed guess is where its keystroke's echo
//!   lands: if the program echoes the key with no unprompted output in between
//!   and in the same coordinate space, the echo lands on the very cell the guess
//!   was painted on. A wrong glyph flashed and then corrected is a corruption of
//!   what the user read, if not of what the program holds. Claimed for a line
//!   editor that knows its own column (a standard one; the far end here does,
//!   until unprompted output or a resize leaves it guessing) and for a link
//!   that honours the predictor's echo deadline: an echo arriving later than
//!   the predictor's own glitch window at its arrival (never less than 250 ms)
//!   after its keystroke can stand in a later guess's cell, and with no
//!   `caused_by` on a PTY nothing positional can tell whose echo it is
//!   (`Sim::late_predecessor`, THE LINK MODEL). `Always` makes no fidelity
//!   claim: it paints guesses no echo has proven.
//! * **Frame fidelity (Adaptive).** Every displayed frame is the screen its
//!   newest painted guess's output settles to: once the last byte that key
//!   caused lands, and nothing a later key caused has, the screen equals the
//!   frame on every cell of every row, and on the cursor the GUI drew one past
//!   that guess (`Sim::settled`). Within FIDELITY's scope, for every key from
//!   the frame's oldest pending guess through its newest, on a SETTLED BASE:
//!   every key before that oldest guess read and its output landed (a frame
//!   caught mid-way through an earlier key's output shows the program's own
//!   transit, which FIDELITY still holds each ghost's cell against). Unprompted
//!   output is counted in bytes, so a frame inside an unprompted segment is
//!   out of scope for what lands after it. It subsumes FIDELITY,
//!   which sees one cell: a frame can put every guess on its echo's cell and
//!   still show a tail that is about to move. The DECORATION RULE: a cell the
//!   far end draws as decoration (here a right prompt or an autosuggestion,
//!   drawn in italics) may be stale on glass, which is counted
//!   (`stale_decoration`), and so may the blank a state chain paints on a cell
//!   its erase vacated, where the settled screen draws decoration (no glyph was
//!   shown there; the frame only lacked what the far end drew); a ghost glyph
//!   that the settled screen contradicts is a violation, decoration or not.
//! * **Timeliness (every mode).** Every ghost on glass is a guess still pending,
//!   typed no longer ago than the predictor's glitch window (at most 2 s), so
//!   nothing flushed or expired is shown after the flush and a wrong cell lives
//!   at most one window.
//! * **Attribution (every mode).** A guess the predictor credits with its echo
//!   (measuring the link with it, perhaps proving its epoch on it) is one whose
//!   own key has echoed, within the link model's scope and on a boundary between
//!   keys' output (`Sim::attributed`). A credit with no echo behind it is a
//!   sample of nothing, and the window every owe is judged by, which the link
//!   model reads, shrinks with it: so this clause is held directly, not through
//!   a consequence the link model could excuse.
//! * **Atomicity (every mode).** A state chain's frame paints every cell of its
//!   displayed state, or none and draws the engine's cursor: a part of a state
//!   is a frame no program draws (`chain_paint_clauses`), and a shift painted
//!   in part shows its tail both moved and not.
//! * **Cursor locality (every mode).** A state chain's drawn cursor is inside
//!   the grid and never on a wide glyph's right half. With LOCALITY for a chain
//!   (its cells on its row, from its `lo`), these two are what painting a state
//!   rather than a set of guesses adds.
//!
//! And once the program has read every key, every byte of output is applied and
//! reconciled, and the clock has passed the longest glitch window (the crate's
//! documented 2 s ceiling), **`O` is empty**, so `D = A`, the pure fold of the
//! output: the astream statement. (In aterm `A` is the pure fold by construction,
//! since the predictor has no path that writes the engine; the settle compares it
//! with a second engine fed only the output, which pins the harness, not the
//! predictor. The predictor's part is that nothing is left on glass.)
//!
//! ## The world
//!
//! A deterministic simulation with a seeded `aterm_spec::rng::XorShift` (the
//! generator behind every generated-input test here) over: keystrokes (printable,
//! wide and combining glyphs, Backspace, Enter, Tab, arrows, Home, End, Delete,
//! Alt-B, Up, Escape, the keypad's Left, kill chords, ^C, paste bursts, now and
//! then a typo fixed mid-line, and LINE EDITING: a word typed, the cursor walked
//! back and forth over it, glyphs erased at its end, Enter now and then with
//! keys still in flight, and SHIFTS: the cursor walked into a word and glyphs
//! or erases typed there, the first one a round trip, the rest in a burst; in
//! a third of the cases now and then a right-to-left letter, which the engine's
//! BiDi draws reversed with the run it joins), a far-end program (a line editor in the byte forms
//! measured from it (`Editor`): readline, which inserts mid-line by writing the
//! glyph, then the tail it shifts, then stepping back, and beeps at an edge;
//! readline with ICH, which reprints the glyph `→` steps over; zle, silent at
//! an edge; a cooked tty, which echoes `←` as `^[[D`; readline in OVERWRITE
//! mode, which replaces the glyph under the cursor where the others shift the
//! tail, and whose rubout blanks a glyph in place; and zle with an
//! AUTOSUGGESTION plugin, whose suggestion (decoration) a glyph that continues
//! it consumes, any other edit clears and now and then fetches anew after the
//! buffer's new end, an erase at the end fetches anew into the cell it vacated,
//! and `→` or End at the end accepts; each rotated per case;
//! a command that starts `cat`, cooked, until ^C; the line editor now and then
//! draws an autosuggestion after an append's
//! echo, erases back across the right margin and up a wrapped row, either
//! leaves the terminal parked in the deferred-wrap slot or wraps itself, and in
//! half the cases draws a right prompt with each prompt and hides it before the
//! line reaches it, as zsh does; a password mode that does not echo; a
//! full-screen app on the alternate screen; in two cases in three aterm's SHELL
//! INTEGRATION, the host authorizing a capability nonce and requiring it and the
//! shell signing its OSC 133 marks with it, so Home, End, Ctrl-A, Ctrl-E and
//! Delete are learned keys, each bound as the case's KEYMAP binds it (readline's
//! and the integrated zsh's canonical edges, `zsh -f` with Home and End unbound
//! and a `~` inserted for Delete, zsh's vi insert mode inserting `^A`), the
//! shell now and then re-executing with the next keymap under a new nonce, a
//! program printing an unsigned mark, Kitty `DISAMBIGUATE` pushed or popped, and
//! a command that clears the screen and reads a line after a `$ ` of its own)
//! whose output reaches the engine in
//! arbitrary byte splits, calm stretches included,
//! UNPROMPTED program output (job notices, prompt redraws, right prompts,
//! autosuggestions, status lines, clears, cursor jumps, line inserts and scrolls,
//! RIS, alt-screen enter and leave, password prompts on a new row or the same row,
//! echo switched off with no prompt at all), host resets (resize, pane swap), and
//! clock advances from a local link to a 2.6 s stall. The host side is wired the
//! way `aterm-gui` wires it (`app_input.rs`, `app_render.rs`): the key-time
//! cursor, generation, grid and alt-screen sample, and the cursor row when the
//! predictor wants it (`predict_glyph`), projected to `Seen` as the GUI's
//! `predict_seen` projects it for the press and for the frame's observe
//! (`reconcile_seen`); at the no-echo gate (that
//! sample saw the alternate screen, or the local termios has ECHO clear,
//! `tty_swallows`) a glyph or an edit key is refused as `note_unmodelled_input`;
//! past it, `set_mode` and `set_ambiguous_width_double` with the terminal's
//! policy before the glyph is predicted or the edit key (a bare arrow, a
//! Backspace) handed to `predict_edit` with the cursor row; `note_line_submit`
//! on Enter; `note_unmodelled_input` on every other non-glyph press and on
//! paste; each note with the press's clock. A state chain's frame is painted
//! as aterm-gui paints it (`paint_chain`): one span, all or none, its cursor
//! drawn only when the span was painted. Every configuration runs under both
//! ambiguous-width policies: `ambiguous_width = "wide"` writes `é` two cells
//! wide, in the engine and in the far end's column arithmetic alike.
//!
//! Every run is a pure function of its seed and op list. A failing case is
//! shrunk (delta debugging over the op list) and reported with its seed, its case
//! index, the minimal ops that still fail the same clause, and a step-by-step
//! trace of them (each press, delivery and frame, with the screen, the pending
//! guesses and what was painted).
//!
//! Mutant hosts are the negative controls, one or more per clause, each run
//! through the same checker on the same seed, which must report THAT clause: a
//! host that hands the predictor a frozen coordinate generation (it forgot that
//! rows scroll) and one that never reports the alternate screen (PROVENANCE); a
//! painter that paints each ghost one cell right, and the painter as it was
//! before it refused wide glyphs (LOCALITY); a host that displays Adaptive's
//! guesses as `Always` does (SECRECY); a host that never reports a line
//! terminator, and one that never tells the predictor the wide ambiguous width
//! (FIDELITY); a host that reads the row right of the cursor as blank (FRAME
//! FIDELITY); a host that skips the erase frame and one that reads its clock
//! only at a keypress (TIMELINESS, one per half); a host that hands the
//! predictor Alt-B, or the keypad's Left under Kitty DISAMBIGUATE, as a bare `←`, and one
//! that draws a chain's cursor one past its newest glyph (FRAME FIDELITY); a
//! host that moves the rest of the row with a shift, as ICH would (LOCALITY);
//! a host that hands on the last input start it read, one that takes it from a
//! mark with the nonce gate never on, one whose editor context never changes,
//! and one that never asks the Kitty mode (FRAME FIDELITY: a program's prompt,
//! a forged mark, a new shell's keymap, a chord sent as `CSI u`);
//! and a predictor with one of its rules switched off (`Host::Rule`, each in the
//! clause it keeps, which is how ATTRIBUTION has its controls). ATOMICITY and
//! CURSOR LOCALITY are held on a frame the world cannot reach (a chain flushes on
//! any opaque cell of its line), with a host painting what it can, one drawing a
//! chain's cursor over a span the painter refused, and one drawing it on a wide
//! glyph's right half (`the_chain_painter_paints_all_or_none`).
//! So a clause whose check went vacuous fails the test instead of passing it.
//!
//! What it found, on its committed seed, before the fixes it pins: a glyph typed
//! after a line terminator was armed on the cell of the submitted line's oldest
//! unechoed guess (LOCALITY, `Always`, three ops: `n`, Enter, `i`), and an echo
//! still owed to a flushed guess confirmed a later guess for the same glyph,
//! proving the epoch one keystroke behind so the next glyph was painted one cell
//! early (FIDELITY, Adaptive). `aterm-predict`'s unit tests pin both. Review then
//! found three more interleavings the sweep had not reached, each a FIDELITY
//! break inside the clause's scope, now pinned as directed cases
//! (`the_reviewed_interleavings_hold`): a guess retired by an earlier key's
//! echo whose own echo was not owed, a tab switched away and back that forgot
//! its owed echo, and a key refused at the no-echo gate that was never owed.
//!
//! The link model was then scaled from the 250 ms floor to the predictor's own
//! window, which a fixed deadline had made vacuous on every link slower than
//! 250 ms, and the sweep found that an expiry owed nothing and kept the epoch's
//! proof (FIDELITY, thirteen ops). FRAME FIDELITY then found that the echo of a
//! key typed mid-line proved its epoch like an append, painting the next glyph
//! over the tail it shifted (`abeicd` shown, `abei cd` settled; FIDELITY's one
//! cell was right). Run over forty seeds rather than one, the sweep then found
//! that a glyph typed after a Backspace cancelled our own guess anchored on the
//! live cursor (the cancelled glyph's echo lands before the erase's, the screen
//! a cell ahead or parked at the margin, and that glyph was credited with the
//! next key's echo, or wrapped, a cell off); and that the harness itself
//! claimed too much in four places, each now scoped: a key whose own echo
//! leaves the editor's model (an insert from the parked slot, or one pushing
//! the tail past the margin, which a real editor rewraps) is not reliable; a
//! frame inside an unprompted segment; a frame on an unsettled base; and a late
//! echo that landed with a later key of its epoch already typed taints that
//! epoch (`KeyLog::ends_epoch`).
//!
//! Review then found what the sweep could not reach. A far end that writes a
//! mid-line glyph before it redraws the tail it shifts (`Editor::Readline`, the
//! form a line editor without ICH, or one that rewrites the tail, sends) proved
//! on the frame between the two, and the next glyph was painted over the tail
//! about to move (THE STANDING TAIL, `a_glyph_echoed_before_its_tail_moves_proves_nothing_after`);
//! a run typed along an autosuggestion was credited before the far end read a
//! key of it, collapsing the window the link model judges lateness by, which
//! hid the misplaced glyph that followed from FIDELITY (ATTRIBUTION, added for
//! it, sees the credits; `cursor_advance_is_the_echo_s_evidence`); and an
//! autosuggestion drawn, changed or cleared beside the end of the line darkened
//! the keys after it (the predictor's unit tests pin that liveness). With
//! ATTRIBUTION and the rewriting far end in the world, sixty seeds found a guess
//! retired without credit that left its epoch's proof standing on a cell its
//! own echo was still to take (FIDELITY, `an_uncredited_retirement_leaves_no_proof`),
//! and two more places the harness claimed too much, now scoped: a credit made
//! while an earlier key's output is overdue but not yet arrived
//! (`Sim::overdue_predecessor`), and one made while the line editor has lost
//! its column. The "run typed back into a wrapped line" the forty-seed sweep
//! reported came from an insert pushing the tail past the margin, which the
//! first scope above then excluded: the rule it prompted (every guess of a run
//! typed over nothing or its own glyph) is pinned by the predictor's unit tests,
//! not here.
//!
//! With the edit lane in the world (state chains, the four editors, the cooked
//! `cat`), eighty seeds found a glyph-lane FIDELITY break the edit lane had made
//! reachable: a run typed while an erase we did not place was still unread was
//! placed a cell right of where it echoes, and the next key's echo, a repeated
//! glyph, credited its first guess once the erase's owe lapsed
//! (`a_run_placed_before_an_erase_lands_is_not_credited_by_the_next_key`, the
//! STALE ANCHOR); and one more place the harness claimed too much, now scoped:
//! a credit made inside output nobody typed for, part-way when the credit is
//! made (an unprompted suggestion drawn with the cursor not yet stepped back),
//! which ATTRIBUTION now excludes as FRAME FIDELITY already did.
//!
//! With the SHIFTS in the world (glyphs and erases typed inside the known text,
//! readline in overwrite mode and zle with an autosuggestion plugin among the
//! far ends, and now and then a right-to-left letter), two hundred seeds found
//! two breaks, each now a named case with its control. The glyph lane showed a
//! space and two letters typed after a lone right-to-left letter in the order
//! typed, which the engine's BiDi reorders once their echoes join one run
//! (`a_right_to_left_run_is_never_shown`: a break on main before this round,
//! reachable as soon as the world typed such a letter). And overwrite mode's
//! echo, beside a suggestion's glyph standing where the insert's state moved
//! the typed one, matched that state, whose line, a cell longer than the
//! buffer, then showed a shift and a `→` the editor did not make
//! (`a_shift_a_suggestion_could_show_proves_nothing`). The same two hundred
//! seeds, run after the fixes, found none.
//!
//! Review of that round found what the sweep had not. A rubout and a glyph
//! typed inside the text, which overwrite mode echoes as the very screen of the
//! two shifts, proved both kinds, each judged alone against a state no frame
//! showed (`overwrite_mode_s_burst_proves_no_shift`); the smallest match
//! credited keys overwrite mode had not read, its screen after fewer keys being
//! the chain's state after more (`overwrite_mode_s_screen_with_fewer_keys_credits_none`,
//! ATTRIBUTION); and a Backspace popped a glyph of an unproven shift as a pair
//! that cancels (`a_backspace_cancels_no_glyph_of_an_unproven_shift`). Each
//! retirement now asks the in-place worlds. Two thousand seeds after those fixes
//! found two breaks on main before the round, each a named case with its control
//! (`a_redrawn_line_keeps_no_proof`, SECRECY: a proof whose echo's cell a redraw
//! took; `a_cancelled_glyph_s_erase_is_no_space_s_echo`, ATTRIBUTION), and one
//! place the world claimed too much: its unprompted right prompt and suggestion
//! were drawn plain, so FRAME FIDELITY took an erase that cleared one beside
//! correct ghosts for a wrong frame. They are decoration, drawn in italics, as
//! every right prompt and suggestion of the world is. Run whole after those
//! fixes, the same two cases found a state an earlier op's echo shows part-way
//! taken for a later state of a chain (`an_erase_caught_part_way_is_no_later_state`,
//! ATTRIBUTION, seen only at the later credit it led to, since the clause leaves
//! out a credit made inside an earlier key's output), and a second over-claim:
//! the world's far end redrew its secret buffer at a no-echo read, displaying
//! the very keys SECRECY holds dark. Its redraw there shows the prompt alone.
//!
//! With the LEARNED KEYS in the world (the integration, three keymaps, a shell
//! re-executing under a new nonce, a forged mark, Kitty's mode pushed and
//! popped, a program's prompt where the shell's stood), the committed seed
//! found no break: FRAME FIDELITY compared 4813 whole frames where it had
//! compared 1568, 144 of them showing a learned key at once (180 displayed
//! frames showed one and held the chain's clauses; the rest repeated the frame
//! before them or fell out of FRAME FIDELITY's scope, unprompted output or an
//! unreliable editor since). Each of the
//! predictor's three new rules and each of the four new host mutants is caught
//! at a named case of its own, and the sweep catches three of them besides; the
//! world's change moved the sweep's case that caught `Host::TailBlind`, which
//! its named case (`mid_line_over_a_space`) still catches. Three hundred seeds
//! then found two breaks, each a named case with its control: End's echo,
//! which reprints the line a cell at a time, caught part-way where a later
//! `←`'s state puts the cursor, credited that `←` unread
//! (`a_line_end_caught_part_way_is_no_later_state`, ATTRIBUTION); and, a break
//! on main before this increment, a zero-width mark the far end's forced wrap
//! carried onto the next guess's cell left that guess painted plain where it
//! settled marked (`a_mark_carried_onto_the_next_guess_s_cell_flushes_it`,
//! FRAME FIDELITY). The rest were the world's own making, each now corrected:
//! its far end read Ctrl-A by the Kitty mode it had pushed since the key was
//! pressed, where the terminal had already sent it in the mode of the press;
//! its zle stepped `→` over a zero-width mark with `ESC [ 0 C`, which a
//! terminal reads as a cell, losing its column while it claimed to know it;
//! and its readline with ICH inserted in front of a mark, the gap leaving the
//! mark drawn on the glyph before while the buffer paired it with the one
//! inserted, which no longer knows its screen and is beyond the model now. Four
//! hundred seeds after these fixes found no break.
//!
//! Review of that increment found the world's far end always sent its `B`
//! over an empty buffer, where zsh sends it after a line pre-filled from its
//! buffer stack, and that a run typed at the `B`'s cell after other keys, or at
//! a `B` zsh's `reset-prompt` left behind, was taken for the buffer's start.
//! The world now pre-fills a prompt from a pushed line (zle's `B` after the
//! text, readline's before it), redraws its prompt in another width with no
//! new `B`, pushes modifyOtherKeys as well as Kitty's mode, and teaches and
//! shows Delete; the integration's `B` says where it stands at the buffer's
//! start, THE FIRST KEY's gate takes only the first key typed at a prompt, and
//! each is caught switched off at a named case. Its sweep of two thousand
//! seeds then found six breaks the committed seed misses (`review_4_cases`):
//! two SECRECY breaks, a password prompt and a reverse index bringing the
//! cursor back onto a proven line (THE FLOOR, DARK BEHIND A WAIT), two
//! ATTRIBUTION breaks, an autosuggestion drawn part-way crediting the typed
//! glyph it begins with (THE CREDIT TAKEN BACK), a FIDELITY break of a cursor
//! on text in the last column taken for the deferred-wrap park (the engine's
//! own flag now), and a far end drawing a prompt over a wrapped line's text
//! (the world's: it clears the row now). A sweep of seed indices 0 to 2400 after
//! those fixes found three more, each a named case: the floor beside a wrapped
//! guess, an engine bug (a mark on a blank lost from its row by an erase after
//! it, fixed in aterm-grid) and the world's own erase in front of a mark. A
//! second sweep of the same indices found one more, the floor excusing any
//! text on a cell a wrapped guess names, where it excuses that guess's own
//! glyph now (`a_prompt_drawn_over_the_guesses_on_the_floor_voids_the_proof`);
//! a third found none.
//!
//! The review of increment 5 (the measurement, `tests/editing_win.rs`) swept
//! past the 2400 seed indices swept before it and found three breaks on main,
//! at seed indices 2482, 2748 and 3942 (`review_5_cases`): the transit wait
//! took every blank for an echo part-way, so a prompt redrawn narrower under a
//! chain had the line painted
//! twice (FRAME FIDELITY, `Rule::TransitBlanks`); a Delete's echo split on its
//! step back stood the cursor on End's state and then Home's, each credited
//! unread (ATTRIBUTION, `Rule::PartWayStepBack`); and a cancelled stale guess's
//! cell anchored a fresh run a cell right of its echoes (FIDELITY,
//! `Rule::StaleCancel`). `Rule::TransitBlanks` also closes the named case of
//! `Rule::CancelsOnlyKnownShifts`, which keeps no clause of its own since.
//!
//! Each such rule of the predictor is a [`Rule`] the debug-only
//! `Predictor::disable_rule_for_test` seam switches off, run as the mutant host
//! `Host::Rule`, and its negative control is caught in its clause by the sweep
//! (`MUTANT_HOSTS`) or at a NAMED CASE (`each_named_case_holds_and_catches_its_mutant`),
//! a directed interleaving the faithful predictor holds; usually both, and
//! `every_rule_has_a_negative_control` holds each rule to at least one.

use aterm_predict::{GhostCell, PredictMode, Rule};

use crate::world::*;

/// THE THEOREM, over every shipped configuration.
#[test]
fn a_prediction_never_corrupts_the_reconciled_view() {
    let mut adaptive = Stats::default();
    let mut always = Stats::default();
    let configs = faithful_configs();
    for (cfg, swept) in configs
        .iter()
        .zip(aterm_spec::check::each(&configs, |&cfg| sweep(cfg, SEED)))
    {
        match swept {
            Ok(stats) => {
                if cfg.mode == PredictMode::Adaptive {
                    adaptive.add(stats);
                } else {
                    always.add(stats);
                }
            }
            Err(report) => panic!("{report}"),
        }
    }
    // THE COVERAGE COUNTERS, printed under `--nocapture`: what every clause
    // was checked on over the committed seed, per mode, which the floors
    // below hold and the docs quote.
    eprintln!(
        "coverage, Adaptive, {} configurations: {adaptive:?}\ncoverage, Always, {} \
         configurations: {always:?}",
        configs.len() / 2,
        configs.len() / 2
    );
    // Non-vacuity: the world reached every clause it claims to check.
    let all_cases = (CASES * faithful_configs().len() / 2) as u64;
    assert_eq!(adaptive.settled_cases, all_cases, "{adaptive:?}");
    assert_eq!(always.settled_cases, all_cases, "{always:?}");
    // The floors sit under what the committed seed reaches over both
    // ambiguous-width policies (5214 painting frames, 4180 fidelity comparisons,
    // 1336 secrets, 12866 row moves, 12 ghosts the painter refused on a wide
    // glyph in `Always`: 394 before DARK OVER TEXT kept a guess waiting over the
    // line's text off the glass, 28 before it held the run's first guess over
    // text behind a head that hides nothing): a generator change that starves a
    // clause fails here instead of passing vacuously. They count comparisons,
    // not whether a comparison can fail; the mutant hosts below are that half.
    assert!(
        adaptive.frames_displaying > 1000,
        "Adaptive must actually paint guesses for the theorem to mean anything: {adaptive:?}"
    );
    assert!(
        always.max_displayed >= 8,
        "type-ahead runs several guesses deep: {always:?}"
    );
    assert!(
        adaptive.secrets_typed > 500,
        "keys typed at visible no-echo prompts: {adaptive:?}"
    );
    assert!(
        adaptive.fidelity_checks > 200,
        "displayed guesses whose echo landed and was compared: {adaptive:?}"
    );
    assert!(
        adaptive.alt_frames > 1000 && always.alt_frames > 1000,
        "{adaptive:?} {always:?}"
    );
    assert!(
        adaptive.generation_moves > 2000,
        "rows moved under the guesses: {adaptive:?}"
    );
    assert!(
        always.ghosts_refused >= 10,
        "guesses displayed on half of a wide glyph, which the painter must refuse: {always:?}"
    );
    // The phase-0 floors, again well under the committed seed's counts over the
    // four Adaptive configurations (4806 whole frames compared with the screen
    // they settle to, 630 credited echoes refused proof over a moved tail, 94
    // standing proofs voided when the tail moved after them, 6710 expired
    // guesses owed, 12956 typed owes, 3896 turns holding a guess on a glyph
    // shown before its echo, 13228 credits checked against their key's output,
    // 1214 frames painting beside a right prompt, 2380 proofs over decoration
    // the echo kept, 580 stale decoration cells counted), and every ghost on
    // glass in either mode checked against the window (10744 Adaptive, 233488
    // Always; 287802 in `Always` with `Rule::DarkOverText` off).
    assert!(
        adaptive.frame_fidelity_checks > 500,
        "displayed frames compared whole with their settled screen: {adaptive:?}"
    );
    assert!(
        adaptive.tail_refusals > 100,
        "credited echoes the tail guard refused proof (mid-line typing reached): {adaptive:?}"
    );
    assert!(
        adaptive.expiry_owes > 1000,
        "expired guesses that owed their echo: {adaptive:?}"
    );
    assert!(
        adaptive.timeliness_checks > 5000 && always.timeliness_checks > 50_000,
        "ghosts on glass checked against the window: {adaptive:?} {always:?}"
    );
    assert!(
        adaptive.typed_owes > 1000,
        "guesses retired without credit that owed only their own glyph: {adaptive:?}"
    );
    assert!(
        adaptive.standing_voids > 5,
        "standing proofs voided by a tail that moved after them: {adaptive:?}"
    );
    assert!(
        adaptive.advance_waits > 1000 && adaptive.attribution_checks > 1000,
        "turns holding a guess on a glyph shown before its echo, and credits checked: {adaptive:?}"
    );
    // DARK OVER TEXT: turns a guess typed over text where the line's end is
    // not known was held off the glass, the head waiting on the screen as it
    // was or the first such guess behind it (26889 over the four `Always`
    // configurations, the same reconcile decisions as Adaptive's).
    assert!(
        always.text_waits > 5000,
        "guesses waiting over the line's text, held off the glass: {always:?}"
    );
    // A line with a right prompt proves through it and paints (decoration the
    // snapshot keeps), and a frame shows a right prompt the settled screen hid
    // (the decoration rule's counted case).
    assert!(
        adaptive.rprompt_frames > 20 && adaptive.decorated_proofs > 50,
        "lines with a right prompt that proved and painted: {adaptive:?}"
    );
    assert!(
        adaptive.stale_decoration > 10,
        "cells of decoration shown stale and counted, not failed: {adaptive:?}"
    );
    // The edit lane's floors (state chains on the known line), again under what
    // the committed seed reaches over the four Adaptive configurations: 860 edit
    // keys armed, 524 frames displaying a chain, every one held to the
    // PROVENANCE oracle, 200 of them showing a motion, 144 motions that proved
    // their class, 76 arrows armed over a cooked tty and never shown, 12 net-null
    // tails, 78 part-way echoes waited on (most of them a shift's glyph written
    // before its tail), 32904 arrows declined with their pixels kept, and 32
    // frames of a chain carried across its terminator (the
    // named case `enter_with_keys_in_flight` holds that one to its control).
    // The part-way echoes were 34 while the transit wait held a cursor anywhere
    // on the chain's row: most were a cooked tty's `^[[D`, whose cursor stands
    // right of every state and now flushes the chain at once
    // (`Rule::TransitReach`), as the divergence it is.
    assert!(
        adaptive.edit_keys_armed > 300 && adaptive.edit_frames_displaying > 200,
        "edit keys armed as chains, and chains displayed: {adaptive:?}"
    );
    assert!(
        adaptive.motions_proven > 50 && adaptive.motions_displayed > 50,
        "motions that proved their class, and frames showing one: {adaptive:?}"
    );
    assert!(
        adaptive.cooked_motions_armed > 50,
        "arrows armed over a cooked tty (tracked, never shown): {adaptive:?}"
    );
    assert!(
        adaptive.net_null_retired > 2 && adaptive.transit_waits >= 2,
        "net-null tails retired, and part-way echoes waited on: {adaptive:?}"
    );
    assert!(
        adaptive.decline_motions > 10_000 && adaptive.carried_chains > 0,
        "arrows declined with their pixels kept, and chains carried across Enter: {adaptive:?}"
    );
    assert!(
        adaptive.oracle_checks > 200,
        "displayed chains held to the PROVENANCE oracle: {adaptive:?}"
    );
    // The shifts' floors (glyphs and erases typed inside the known text), under
    // what the committed seed reaches over the four Adaptive configurations: 56
    // frames displaying a shift (298 in `Always`), 32 shifts that proved their
    // class, 44 shifts armed while the far end was in overwrite mode and none of
    // them ever shown (an exact count: the class ladder's whole claim), 20
    // retirements an in-place world also showed, which left their line
    // unknown, 32 glyphs inside the text a chain could not place, 12 vacated
    // blanks under a suggestion the erase fetched, 4 erases refused beside text
    // past the line, 15545 keys refused on a row a right-to-left letter
    // reorders, and 2 chains flushed for an opaque cell of their line.
    assert!(
        adaptive.shifts_displayed > 25 && adaptive.shifts_proven > 15,
        "shifts that proved their class, and frames showing one: {adaptive:?}"
    );
    assert!(
        always.shifts_displayed > 100,
        "shifts on glass in `Always`: {always:?}"
    );
    assert!(
        adaptive.overwrite_shifts_armed > 20 && adaptive.overwrite_shifts_displayed == 0,
        "shifts armed in overwrite mode, every one dark: {adaptive:?}"
    );
    assert!(
        adaptive.ambiguous_shifts > 2 && adaptive.mid_line_flushes > 10,
        "shifts no edit in place could be told from, and glyphs a chain could not place: \
         {adaptive:?}"
    );
    assert!(
        adaptive.late_decoration > 2 && adaptive.vacate_refusals >= 2,
        "vacated blanks under a fetched suggestion, and erases refused beside text: {adaptive:?}"
    );
    assert!(
        adaptive.bidi_refusals > 1000 && adaptive.opaque_refusals > 0,
        "keys refused on a right-to-left row, and chains on an opaque cell: {adaptive:?}"
    );
    // The learned keys' floors (Home, End, Ctrl-A, Ctrl-E and Delete under the
    // shell integration, two cases in three), under what the committed seed
    // reaches over the four Adaptive configurations: 292 lines started by the
    // first key typed at the shell's authenticated buffer start (798 before
    // THE FIRST KEY's gate took the rest), 950 learned keys armed, 742 of them
    // probes, 268 bindings learned and 94 contradicted (a key bound to nothing
    // or to a glyph, a late echo, a new shell's keymap), 140 frames showing a
    // learned key at once (208 in `Always`), 120 of them compared whole with
    // their settled screen, 32 showing a learned Delete (every one compared),
    // and 196 Ctrl-A or Ctrl-E refused under Kitty's mode or modifyOtherKeys,
    // which send them escaped. With them FRAME FIDELITY compared 4694 whole
    // frames (1568 before the learned keys) and found nothing wrong.
    assert!(
        adaptive.witnessed_lines > 150 && adaptive.bindings_learned > 130,
        "lines started at the authenticated buffer start, and bindings learned: {adaptive:?}"
    );
    assert!(
        adaptive.probes_armed > 350 && adaptive.bindings_unlearned > 40,
        "probes armed, and bindings contradicted: {adaptive:?}"
    );
    assert!(
        adaptive.line_edge_displayed > 80 && always.line_edge_displayed > 80,
        "frames showing a learned key: {adaptive:?} {always:?}"
    );
    assert!(
        adaptive.learned_fidelity_checks > 60 && adaptive.delete_fidelity_checks > 15,
        "frames showing a learned key, and a learned Delete, compared whole with their \
         settled screen: {adaptive:?}"
    );
    assert!(
        adaptive.kitty_refusals > 80,
        "chords refused under Kitty's mode or modifyOtherKeys: {adaptive:?}"
    );
}

/// The seed indices (`seed_index`) the committed sweep below adds to the
/// committed seed (index 0): as many as run in the time the theorem takes,
/// every faithful configuration of each (16 sweeps of 600 cases in 0.71 s on
/// 18 cores, beside the theorem's 8 in 0.64 s).
const SWEPT_SEED_INDICES: std::ops::RangeInclusive<u64> = 1..=2;

/// THE THEOREM OVER MORE SEEDS: the committed sweep of every faithful
/// configuration over the seed indices `SWEPT_SEED_INDICES`, where the theorem
/// holds index 0 alone. A sweep over many seeds found what the committed seed
/// never reached (`sweep_cases`); this keeps a few of them in every run,
/// deterministically, with every clause of the theorem in force. Its floors
/// are the theorem's: they are counted on index 0.
#[test]
fn a_prediction_never_corrupts_the_reconciled_view_over_more_seeds() {
    let failures = sweep_seed_indices(SWEPT_SEED_INDICES);
    assert!(failures.is_empty(), "{}", failures.join("\n=====\n"));
}

/// THE REVIEW'S SWEEP, run on demand (`--ignored`): the theorem over seed
/// indices 0 to 999, every faithful configuration of each, each failure
/// shrunk. About seven minutes on 18 cores, too long for every run; a review
/// of the predictor runs it, and each break it finds becomes a named case
/// (`sweep_cases`).
#[test]
#[ignore = "the review's sweep: 8000 sweeps of 600 cases, minutes on every core"]
fn a_prediction_never_corrupts_the_reconciled_view_over_a_thousand_seeds() {
    let failures = sweep_seed_indices(0..1000);
    for failure in &failures {
        let head: String = failure.chars().take(6000).collect();
        eprintln!("{head}\n=====");
    }
    assert!(
        failures.is_empty(),
        "{} sweeps broke a clause",
        failures.len()
    );
}

/// NEGATIVE CONTROLS, one or more per clause: each mutant host breaks one clause
/// (`Host::breaks`), and the same checker, on the same seed, must report THAT
/// clause, or the theorem above is vacuous in it. A frozen generation and a host
/// blind to the alternate screen paint a guess in a space it was not armed in; a
/// painter one cell off, and the painter that wrote over wide glyphs, change a
/// cell no guess predicted; a host showing Adaptive's guesses as `Always` paints a
/// password; a host that never reports Enter paints the next line's glyphs where
/// they do not echo, and so does one that never tells the predictor the wide
/// ambiguous width (an `é` guessed one cell where its echo takes two).
#[test]
fn the_checker_catches_every_mutant_host_in_its_clause() {
    let missed = aterm_spec::check::each(&MUTANT_HOSTS, |&host| {
        let clause = host.breaks().expect("a mutant breaks a clause");
        let caught = [PredictMode::Adaptive, PredictMode::Always]
            .iter()
            .any(|&mode| {
                let cfg = Config {
                    mode,
                    remote: true,
                    host,
                    forced_wrap: false,
                    wide: host.wide(),
                    rprompt: false,
                    editor: Editor::ReadlineIch,
                    bidi: false,
                    integration: false,
                    keymap: Keymap::Canonical,
                };
                (0..CASES).any(|case| {
                    let (cfg, _, _, ops) = case_world(cfg, SEED, case);
                    matches!(run(cfg, &ops), Err((_, v)) if v.clause == clause)
                })
            });
        (!caught).then(|| format!("{host:?}: the checker never reported `{clause}`"))
    });
    let missed: Vec<String> = missed.into_iter().flatten().collect();
    assert!(missed.is_empty(), "{}", missed.join("\n"));
}

/// Interleavings review found that the sweep had not reached, each a FIDELITY
/// break inside the clause's scope before its fix (every echo within 250 ms of
/// its key, a reliable editor, no unprompted output between a guess and its echo).
#[test]
fn the_reviewed_interleavings_hold() {
    let adaptive = adaptive_remote();
    for (label, ops) in reviewed_interleavings() {
        let mut trace = Some(String::new());
        if let Err((at, v)) = run_traced(adaptive, &ops, &mut trace) {
            panic!(
                "{label}: op {at} broke `{}`: {}\ntrace:\n{}",
                v.clause,
                v.detail,
                trace.unwrap_or_default()
            );
        }
    }
}

/// The interleavings review found (see `the_reviewed_interleavings_hold`), each
/// with its label. The first is also the named case of the typed owe's
/// same-glyph rule.
fn reviewed_interleavings() -> [(&'static str, Vec<Op>); 3] {
    // `b`, `c`, `n`, `o`, `n`: glyphs 1, 2, 6, 7 and 118 of `Sim::key_for`.
    let proven_line = [
        Op::Key(0, 1),
        Op::Program(1, 0),
        Op::Advance(60),
        Op::Deliver(1024),
        Op::Key(0, 2),
        Op::Program(1, 0),
        Op::Advance(60),
        Op::Deliver(1024),
    ];
    [
        (
            // `n` painted, then flushed by a frame caught mid status-line redraw;
            // `nnc` typed 190 ms later. The flushed key's echo retires the first
            // new guess without credit; that guess's own echo, landing after the
            // flushed key's window, proved the second one cell behind, and `c`
            // was painted where the second `n` echoed.
            "a guess retired without credit owes its own echo",
            [
                &[
                    Op::Key(79, 0),
                    Op::Program(1, 0),
                    Op::Deliver(1024),
                    Op::Advance(300),
                ][..],
                &proven_line,
                &[
                    Op::Key(0, 6),
                    Op::Unprompted(Unprompted::StatusLine, 0),
                    Op::Advance(10),
                    Op::Deliver(755),
                    Op::Deliver(1024),
                    Op::Advance(180),
                    Op::Key(0, 6),
                    Op::Key(0, 118),
                    Op::Key(0, 2),
                    Op::Advance(40),
                    Op::Program(1, 0),
                    Op::Deliver(1024),
                    Op::Advance(30),
                    Op::Program(1, 0),
                    Op::Deliver(1024),
                    Op::Advance(10),
                    Op::Program(2, 0),
                    Op::Deliver(1024),
                ],
            ]
            .concat(),
        ),
        (
            // `n` painted; the pane swapped away and straight back (take and
            // restore) 10 ms later; `n` typed again at the live cursor. The
            // first `n`'s echo, forgotten at the swap, proved the second.
            "a tab switched away and back keeps its owed echo",
            [
                &proven_line[..],
                &[
                    Op::Key(0, 6),
                    Op::Advance(10),
                    Op::HostReset(false),
                    Op::Key(0, 6),
                    Op::Advance(50),
                    Op::Program(1, 0),
                    Op::Deliver(1024),
                    Op::Key(0, 2),
                    Op::Advance(50),
                    Op::Program(2, 0),
                    Op::Deliver(1024),
                ],
            ]
            .concat(),
        ),
        (
            // Enter starts a full-screen app; Enter in it quits; `n` is typed
            // while the host still samples the alternate screen, so it is refused
            // at the no-echo gate — and the shell, back on the main screen, echoes
            // it. Never owed, that echo proved the `n` typed again at the prompt.
            "a key refused at the no-echo gate is owed",
            [
                &proven_line[..],
                &[
                    Op::Key(79, 1),
                    Op::Program(1, 5),
                    Op::Advance(60),
                    Op::Deliver(1024),
                    Op::Advance(300),
                    Op::Key(79, 1),
                    Op::Program(1, 0),
                    Op::Advance(200),
                    Op::Key(0, 6),
                    Op::Program(1, 0),
                    Op::Advance(40),
                    Op::Deliver(950),
                    Op::Advance(5),
                    Op::Key(0, 6),
                    Op::Program(1, 0),
                    Op::Advance(15),
                    Op::Deliver(1),
                    Op::Key(0, 7),
                    Op::Program(1, 0),
                    Op::Advance(10),
                    Op::Deliver(1024),
                ],
            ]
            .concat(),
        ),
    ]
}

/// A named directed case: an interleaving that breaks one clause when one rule
/// of the predictor (or of its host) is off, and that the faithful predictor
/// holds. Each rule's negative control is caught HERE, at its case, as well as
/// anywhere the seeded sweep happens to reach it, so a generator change that
/// stops reaching it cannot turn the control vacuous.
struct NamedCase {
    name: &'static str,
    cfg: Config,
    ops: Vec<Op>,
    /// The hosts, or predictor rules switched off, each of which must break its
    /// own clause ([`Host::breaks`]) here.
    mutants: Vec<Host>,
}

/// Adaptive, remote, narrow: where every fidelity clause is claimed.
fn adaptive_remote() -> Config {
    Config {
        mode: PredictMode::Adaptive,
        remote: true,
        host: Host::Faithful,
        forced_wrap: false,
        wide: false,
        rprompt: false,
        editor: Editor::ReadlineIch,
        bidi: false,
        integration: false,
        keymap: Keymap::Canonical,
    }
}

/// One key read and echoed on a 60 ms link: press, read, deliver.
fn echoed(key: Op) -> [Op; 4] {
    [key, Op::Program(1, 0), Op::Advance(60), Op::Deliver(1024)]
}

fn named_cases() -> Vec<NamedCase> {
    vec![
        NamedCase {
            // A guess expires on a slow link; the glyph typed after it anchors on
            // the live cursor, which still stands on the expired guess's cell, and
            // the epoch's proof named that cell. Kept, the proof painted the new
            // glyph there; the expired key's echo then took the cell, inside the
            // predictor's own window, and the new glyph echoed one cell on. The
            // sweep's shrunk counterexample once the link model was scaled to the
            // predictor's window.
            name: "an_expired_guess_s_late_echo_is_owed",
            cfg: Config {
                remote: false,
                ..adaptive_remote()
            },
            ops: vec![
                Op::Key(34, 2_238_325_228),
                Op::Program(1, 2_762_888_050),
                Op::Advance(24),
                Op::Key(43, 2_866_421_021),
                Op::Deliver(1024),
                Op::Program(1, 4_120_887_936),
                Op::Key(66, 1_953_497_343),
                Op::Advance(87),
                Op::Advance(77),
                Op::Deliver(1024),
                Op::Advance(42),
                Op::Advance(66),
                Op::Key(1, 1_095_511_450),
            ],
            mutants: vec![Host::Rule(Rule::ExpiryOwes)],
        },
        NamedCase {
            // `ab cd` echoed on a slow link, the cursor taken back onto the space
            // by keys the predictor does not place (Alt-B to `c`, then a `←` it
            // declines, the known line gone with Alt-B), a pause past the window,
            // then `e` and `i`. `e`'s echo inserts it and shifts ` cd` right; it
            // is `e`'s own (nothing owed in the window) and the cursor advanced, so
            // positional evidence credited it. Proving the epoch, it put `i` on
            // glass over the shifted space: the frame read `abeicd`, the screen
            // settled to `abei cd`. FIDELITY's one cell was right (`i` echoes
            // where it was painted); only the whole frame was wrong. The tail
            // guard sees the row right of `e` changed since the press, and proves
            // nothing. (With three predicted `←` the run starts inside the known
            // text, which proves nothing by knowledge, `run_inside`:
            // `a_run_typed_inside_the_known_text_proves_nothing`.)
            name: "mid_line_over_a_space",
            cfg: adaptive_remote(),
            ops: [
                &echoed(glyph('a'))[..],
                &echoed(glyph('b')),
                &echoed(glyph(' ')),
                &echoed(glyph('c')),
                &echoed(glyph('d')),
                &echoed(WORD_LEFT),
                &echoed(LEFT),
                &[Op::Advance(600)],
                &echoed(glyph('e')),
                &echoed(glyph('i')),
            ]
            .concat(),
            mutants: vec![Host::Rule(Rule::TailGuard), Host::TailBlind],
        },
        NamedCase {
            // The reviewed `n`, `n`, `c` interleaving: a guess retired by an
            // earlier key's echo owes its own, which lands in the second `n`'s
            // cell. Owed typed, as `n`, it refuses credit to that `n` and to no
            // other glyph; a typed owe that refused nothing credited it, proved
            // the epoch one cell behind and painted `c` where the second `n`
            // echoed.
            name: "a_guess_retired_without_credit_owes_its_own_glyph",
            cfg: adaptive_remote(),
            ops: reviewed_interleavings()[0].1.clone(),
            mutants: vec![Host::Rule(Rule::TypedOwesSameGlyph)],
        },
        NamedCase {
            // `s` typed, ` ` typed and cancelled by a Backspace before either
            // echoes, then three spaces. The cancelled space's echo lands (the
            // screen one cell ahead), so the first new space, anchored on the
            // live cursor, was armed a cell right of where it echoes; the erase
            // then lands, and the SECOND space's echo stands in the first one's
            // cell, the cursor advancing past it, which is how a space confirms.
            // Credited, that proved the epoch one cell behind and painted the
            // third space at (0, 4), where it does not echo. Anchored on the
            // cancelled cell, the first space echoes where it is armed. Shrunk
            // from the sweep's seed 13, case 27, once the sweep ran forty seeds.
            //
            // Anchored there, though, the cancelled space's echo lands in the
            // first new space's cell with the cursor past it, and credited it
            // with the cancelled key's echo (ATTRIBUTION). The cancelled glyph's
            // echo is now owed as its glyph (`CancelOwes`), which refuses that
            // credit; and it refuses the second space's credit in the cell a
            // cell-right anchor would give, so `CancelTransit` off no longer
            // breaks this case: its control is
            // `a_cancelled_glyph_s_cell_anchors_what_follows` below.
            name: "a_cancelled_glyph_leaves_its_cell_as_the_anchor",
            cfg: Config {
                remote: false,
                ..adaptive_remote()
            },
            ops: vec![
                Op::Key(31, 2_643_783_102),
                Op::Advance(56),
                Op::Program(1, 2_288_360_536),
                Op::Key(24, 1_893_182_434),
                Op::Key(76, 364_413_965),
                Op::Program(1, 1_261_122_255),
                Op::Deliver(1024),
                Op::Key(57, 2_826_364_955),
                Op::Key(45, 698_995_469),
                Op::Program(1, 1_108_020_928),
                Op::Program(1, 2_981_388_103),
                Op::Key(0, 9),
                Op::Program(1, 0),
                Op::Advance(57),
                Op::Deliver(1024),
            ],
            mutants: vec![Host::Rule(Rule::CancelOwes)],
        },
        NamedCase {
            // A proven line reaches the last column, where `c` is painted; a
            // Backspace cancels it before it echoes, and `c`'s echo then parks
            // the terminal in the deferred-wrap slot without moving the cursor.
            // The `c` typed next, anchored on that live cursor, matched the
            // proof's cell and was wrapped to the next row; the erase un-parked
            // the line and it echoed in the last column. Shrunk from the sweep's
            // seed 39, case 323.
            //
            // `CancelTransit` is no longer the only rule that keeps it: since the
            // standing tail, a guess wrapped off the cursor's row carries no
            // snapshot, so the parked `c` right of the proof's echo voids the
            // proof at the frame after the press, before anything is painted on
            // it. The case holds with `CancelTransit` off as well, and stays as the
            // interleaving both keep; the rule's negative control is
            // `a_cancelled_glyph_s_cell_anchors_what_follows` below.
            name: "a_cancelled_glyph_at_the_margin_does_not_wrap_the_next",
            cfg: Config {
                remote: false,
                ..adaptive_remote()
            },
            ops: vec![
                Op::Key(16, 1_093_200_419),
                Op::Key(54, 159_721_498),
                Op::Program(1, 3_905_491_751),
                Op::Program(1, 1_193_737_330),
                Op::Key(0, 2),
                Op::Key(0, 7),
                Op::Key(0, 15),
                Op::Program(3, 0),
                Op::Key(0, 1),
                Op::Key(0, 19),
                Op::Program(2, 0),
                Op::Advance(87),
                Op::Deliver(1024),
                Op::Key(0, 5),
                Op::Key(0, 5),
                Op::Program(2, 0),
                Op::Key(0, 5),
                Op::Advance(223),
                Op::Key(0, 6),
                Op::Program(2, 0),
                Op::Key(39, 2_763_853_278),
                Op::Deliver(1024),
                Op::Key(71, 1_029_438_371),
                Op::Program(1, 827_619_574),
                Op::Deliver(1024),
                Op::Key(36, 3_323_733_270),
            ],
            mutants: vec![],
        },
        NamedCase {
            // `mid_line_over_a_space` on a far end that writes the glyph before
            // it redraws the tail (`Editor::Readline`), its echo delivered in two
            // pieces: `e` alone, a frame, then ` cd` and the cursor stepped back.
            // The frame between them shows `abecd` with the cursor past `e`; the
            // row right of `e` is what the press saw, so `e` proves. Once the
            // redraw lands (`abe cd`), `i`, anchored on that proof, was painted
            // over the shifted space: `abeicd` on glass, settling to `abei cd`.
            // The standing tail re-reads the row at the frame after the redraw,
            // finds it moved, and voids the proof first. Found by review; the
            // sweep reaches it once calm stretches split their deliveries.
            name: "a_glyph_echoed_before_its_tail_moves_proves_nothing_after",
            cfg: Config {
                editor: Editor::Readline,
                ..adaptive_remote()
            },
            ops: [
                &echoed(glyph('a'))[..],
                &echoed(glyph('b')),
                &echoed(glyph(' ')),
                &echoed(glyph('c')),
                &echoed(glyph('d')),
                &echoed(WORD_LEFT),
                &echoed(LEFT),
                &[
                    Op::Advance(300),
                    glyph('e'),
                    Op::Program(1, 0),
                    Op::Advance(60),
                    Op::Deliver(1),
                    Op::Deliver(1024),
                ],
                &echoed(glyph('i')),
            ]
            .concat(),
            mutants: vec![Host::Rule(Rule::StandingTail)],
        },
        NamedCase {
            // Every echo 300 ms after its key: a proven line, then an
            // autosuggestion `dice` drawn after the cursor, and `d`, `i`, `c`, `e`
            // typed along it with no frame between. Their cells already show
            // their glyphs; retired on that, before the far end read one of them,
            // they measured the link at 0 ms each, and the smoothed RTT fell from
            // 300 to 72 ms, the window from 900 to 250. Then the reviewed `n`,
            // `n`, `c` interleaving: the flushed `n`'s echo, 300 ms after its key
            // and so on time for this link, outlived its owe, was credited to the
            // second `n` one cell behind, and `c` was painted where that `n`
            // echoes. Held for the cursor to pass them, the guesses retire on
            // their own echoes, with their real samples. FIDELITY cannot see it:
            // the link model calls the 300 ms echo late against the window the
            // collapse left. ATTRIBUTION sees its cause, the four credits with no
            // echo behind them, at the frame that makes them.
            name: "cursor_advance_is_the_echo_s_evidence",
            cfg: adaptive_remote(),
            ops: {
                let slow = |key: Op| [key, Op::Program(1, 0), Op::Advance(300), Op::Deliver(1024)];
                [
                    &[
                        Op::Key(79, 0),
                        Op::Program(1, 0),
                        Op::Deliver(1024),
                        Op::Advance(300),
                    ][..],
                    &slow(glyph('a')),
                    &slow(glyph('b')),
                    &slow(glyph('o')),
                    &[
                        Op::Advance(1000),
                        // `dice`, drawn dim, the cursor stepped back over it.
                        Op::Unprompted(Unprompted::Suggest, 3),
                        Op::Deliver(1024),
                        // `d`, `i`, `c`, `e`, each with no frame of its own.
                        Op::Key(0, 36),
                        Op::Key(0, 16),
                        Op::Key(0, 24),
                        Op::Key(0, 4),
                        Op::Frame,
                        Op::Program(4, 0),
                        Op::Advance(300),
                        Op::Deliver(1024),
                        Op::Advance(400),
                        // `n`, painted, then flushed by a frame caught mid
                        // status-line redraw.
                        Op::Key(0, 6),
                        Op::Unprompted(Unprompted::StatusLine, 0),
                        Op::Advance(10),
                        Op::Deliver(755),
                        Op::Deliver(1024),
                        Op::Advance(100),
                        // `n`, `n`, `c`, with no frame of their own.
                        Op::Key(0, 28),
                        Op::Key(0, 118),
                        Op::Key(0, 2),
                        Op::Advance(190),
                        // The flushed `n` read, its echo 300 ms after its key.
                        Op::Program(1, 0),
                        Op::Deliver(1024),
                        Op::Frame,
                        Op::Advance(110),
                        Op::Program(1, 0),
                        Op::Deliver(1024),
                        Op::Advance(30),
                        Op::Program(2, 0),
                        Op::Deliver(1024),
                    ],
                ]
                .concat()
            },
            mutants: vec![Host::Rule(Rule::CursorAdvance)],
        },
        NamedCase {
            // `n` echoed (a proof at (0, 1)); `s` typed and read; ` ` typed and
            // cancelled by a Backspace; `s` and the cancelled ` ` echo, the
            // screen a cell ahead with the erase unread, and `s`'s credit moves
            // the proof to (0, 2). A space typed then anchors on the cancelled
            // cell (0, 2) and is retired by the cancelled space's echo with the
            // cursor past it, without credit (that echo is owed). Its own echo is
            // still to come, and the erase lands next, putting the cursor back on
            // (0, 2): with the proof left standing there, `e` was painted at
            // (0, 2) and echoed at (0, 3), after the space. Shrunk from seed 43,
            // case 144 of the sixty-seed sweep.
            name: "an_uncredited_retirement_leaves_no_proof",
            cfg: Config {
                remote: false,
                editor: Editor::Readline,
                ..adaptive_remote()
            },
            ops: vec![
                Op::Key(0, 6),
                Op::Advance(142),
                Op::Program(1, 0),
                Op::Deliver(1024),
                Op::Key(0, 19),
                Op::Advance(228),
                Op::Program(2, 0),
                Op::Key(57, 3_339_781_981),
                Op::Key(76, 1_313_050_743),
                Op::Program(1, 2_545_784_962),
                Op::Deliver(1024),
                Op::Key(0, 9),
                Op::Program(1, 0),
                Op::Deliver(1024),
                Op::Key(0, 15),
            ],
            mutants: vec![Host::Rule(Rule::UncreditedVoids)],
        },
        NamedCase {
            // `s`, `b` typed and read; `b` cancelled by a Backspace before its
            // echo; `s` and `b` echo, the screen a cell ahead with the erase
            // unread. `n`, `n` typed, then `i`. Anchored on the live cursor
            // (0, 2) rather than the cancelled cell (0, 1), the first `n` is a
            // cell right of where it echoes: once the erase lands and both `n`s
            // echo, the second `n`'s echo stands in the first `n`'s cell with
            // the cursor past it and is credited to it, and the proof one cell
            // behind paints what follows a cell off. Shrunk from seed 35, case
            // 69 of the sixty-seed sweep, once the cancelled glyph's echo was
            // owed (`CancelOwes`) and the earlier controls of `CancelTransit`
            // held without it.
            name: "a_cancelled_glyph_s_cell_anchors_what_follows",
            cfg: Config {
                remote: false,
                forced_wrap: true,
                ..adaptive_remote()
            },
            ops: vec![
                Op::Key(37, 3_543_315_056),
                Op::Key(63, 65_976_681),
                Op::Program(1, 200_171_969),
                Op::Program(1, 2_627_421_720),
                Op::Advance(40),
                Op::Key(74, 3_854_888_406),
                Op::Deliver(1024),
                Op::Key(0, 4_013_821_073),
                Op::Key(21, 1_730_876_823),
                Op::Advance(56),
                Op::Program(1, 4_031_055_518),
                Op::Program(1, 2_680_122_373),
                Op::Key(66, 3_286_277_987),
                Op::Program(1, 81_452_752),
                Op::Deliver(1024),
            ],
            mutants: vec![Host::Rule(Rule::CancelTransit)],
        },
    ]
    .into_iter()
    .chain(edit_cases())
    .chain(edit_review_cases())
    .chain(shift_cases())
    .chain(line_edge_cases())
    .chain(review_4_cases())
    .chain(review_5_cases())
    .chain(sweep_cases())
    .collect()
}

/// The named cases of the SHIFTS (glyphs and erases typed inside the known
/// text, whose states move the text right of the cursor).
fn shift_cases() -> Vec<NamedCase> {
    vec![
        NamedCase {
            // `ab` proven, `←` echoed onto `b`, `o` typed there. Readline in
            // overwrite mode REPLACES `b`, where an insert shifts it right. The
            // first mid-line insert of the epoch is the proof of its class, and
            // overwrite mode never gives it, so `o` stays dark: its state matches
            // no screen and the chain waits out its window. Shown on the glyph
            // proof alone, `aob` was on glass while the screen settled to `ao`.
            name: "overwrite_mode_never_shows_a_shift",
            cfg: adaptive_on(Editor::Overwrite),
            ops: [&typed("ab")[..], &echoed(LEFT), &echoed(glyph('o'))].concat(),
            mutants: vec![Host::Rule(Rule::InsertShiftProof)],
        },
        NamedCase {
            // `abc` proven, `←` echoed onto `c`, Backspace: overwrite mode's
            // rubout BLANKS `b` and moves nothing (`a c`), where an erase shifts
            // `c` left (`ac`). Shown on the glyph proof alone, `ac` was on glass.
            name: "overwrite_rubout_never_shows_a_shift",
            cfg: adaptive_on(Editor::Overwrite),
            ops: [&typed("abc")[..], &echoed(LEFT), &echoed(BACKSPACE)].concat(),
            mutants: vec![Host::Rule(Rule::EraseShiftProof)],
        },
        NamedCase {
            // `a` and two spaces proven, `←` twice onto the first space, a space
            // typed there: overwrite mode replaces the space with a space, and
            // the insert's state, its tail all spaces, is the very same screen.
            // Credited, it proves no class: it moved no glyph. Taken as the
            // class proof, `←` twice back onto `a` and `o` typed there showed
            // `oa` (the insert shifting `a`) while overwrite mode settled to
            // `o` with a blank after it.
            name: "a_shift_of_spaces_proves_nothing",
            cfg: adaptive_on(Editor::Overwrite),
            ops: [
                &typed("a  ")[..],
                &echoed(LEFT),
                &echoed(LEFT),
                &echoed(glyph(' ')),
                &echoed(LEFT),
                &echoed(LEFT),
                &echoed(glyph('o')),
            ]
            .concat(),
            mutants: vec![Host::Rule(Rule::ShiftProofDistinguishes)],
        },
        NamedCase {
            // `abc` proven, `c`'s echo bringing up a dim suggestion `ion` after
            // it; `←` twice, and `o` typed inside the line, each echoed (readline
            // writes `o`, then `bc`, `c` over the suggestion's first cell): the
            // insert class is proven. `n` typed there shows at once, `bc` moved
            // one cell right, `c` over the suggestion's `o`, and the rest of the
            // row left as the far end leaves it. A host that moves the rest of
            // the row with the shift, as ICH would, moved the suggestion, which
            // no cell of the state names.
            name: "insert_mid_line_beside_a_suggestion",
            cfg: adaptive_on(Editor::Readline),
            ops: [
                &typed("ab")[..],
                &[
                    glyph('c'),
                    Op::Program(1, 3),
                    Op::Advance(60),
                    Op::Deliver(1024),
                ],
                &echoed(LEFT),
                &echoed(LEFT),
                &echoed(glyph('o')),
                &echoed(glyph('n')),
            ]
            .concat(),
            mutants: vec![Host::PaintsPastTheLine],
        },
        NamedCase {
            // `abc` proven with a shift: `←` twice and `o` echoed inside it. `←` is
            // armed and shown, and `日`, typed right after it, is a wide glyph
            // the chain cannot place: its echo shifts the text after the cursor
            // two cells. It is owed and the chain flushed (R-ROW), whose shown
            // `←` goes dark before the wide glyph's echo lands.
            name: "a_wide_glyph_mid_line_retires_the_tail",
            cfg: adaptive_on(Editor::Readline),
            ops: [
                &typed("abc")[..],
                &echoed(LEFT),
                &echoed(LEFT),
                &echoed(glyph('o')),
                &[
                    LEFT,
                    // `日`: `Sim::key_for`'s glyph 12, with no frame of its own.
                    Op::Key(0, 12),
                    Op::Frame,
                    Op::Program(2, 0),
                    Op::Advance(60),
                    Op::Deliver(1024),
                ],
            ]
            .concat(),
            mutants: vec![],
        },
        NamedCase {
            // `א`, a space and `א` `א` typed, the first read and echoed: a lone
            // right-to-left letter keeps the row in logical order, so its echo
            // proved the line, and the three after it were shown `א אא` in the
            // order typed. Their echoes join one right-to-left run, which the
            // engine draws reversed: the row settled to `אא א`. A glyph that
            // makes its row reorder is declined, and so is one typed on such a
            // row. Found by the two-hundred-seed sweep (seed index 125, case 509,
            // a readline far end: `o` and `אא א` shown while the row settled to
            // `א אא`), written out here on zle.
            name: "a_right_to_left_run_is_never_shown",
            cfg: Config {
                remote: false,
                forced_wrap: true,
                rprompt: true,
                bidi: true,
                ..adaptive_on(Editor::Zle)
            },
            ops: vec![
                RTL_GLYPH,
                glyph(' '),
                Op::Advance(28),
                RTL_GLYPH,
                Op::Program(1, 0),
                RTL_GLYPH,
                Op::Deliver(1024),
                Op::Program(1, 0),
                Op::Deliver(1024),
            ],
            mutants: vec![Host::Rule(Rule::RightToLeftRows)],
        },
        NamedCase {
            // `si` typed, `i`'s echo bringing up a suggestion `dice` after it;
            // `c` echoed over the suggestion's `d` (`sicice`); `c` again, `←`
            // and `e` typed before any is read. Readline in OVERWRITE mode
            // appends `c`, steps back and REPLACES it with `e`: `sicece`, the
            // suggestion's own `c` standing where the insert's state moves the
            // typed one. Matched there and taken as the line, `sicec` was a
            // cell longer than the buffer, the insert class proven on it, and
            // `o` typed next was shown shifted (`siceoc`) while overwrite mode
            // settled to `siceoe`. A shift no edit in place can be told from
            // leaves the line unknown. The two-hundred-seed sweep's seed index
            // 21, case 79, shrunk.
            name: "a_shift_a_suggestion_could_show_proves_nothing",
            cfg: Config {
                remote: false,
                forced_wrap: true,
                rprompt: true,
                ..adaptive_on(Editor::Overwrite)
            },
            ops: vec![
                Op::Key(44, 114_333_337),
                Op::Key(63, 3_449_488_800),
                Op::Program(1, 767_644_548),
                Op::Program(1, 2_253_465_987),
                Op::Key(0, 2),
                Op::Program(1, 0),
                Op::Advance(80),
                Op::Deliver(1024),
                Op::Key(0, 2),
                Op::Program(1, 0),
                Op::Advance(65),
                Op::Key(87, 1),
                Op::Program(1, 0),
                Op::Key(0, 15),
                Op::Program(1, 0),
                Op::Deliver(1024),
                Op::Key(8, 3_113_602_571),
            ],
            mutants: vec![Host::Rule(Rule::ShiftProofDistinguishes)],
        },
        NamedCase {
            // `abc` proven, `←` echoed onto `c`, then a Backspace and `o`
            // typed inside the text and read together. Overwrite mode blanks
            // `b` and writes `o` over the blank: `aoc`, the very screen of the
            // two shifts, though no frame showed the state between them. The
            // world that edits both in place shows it too, so neither kind is
            // proven, and `n` typed next stays dark. Judged op by op against the
            // fold's own intermediate state, both kinds were proven, and `n`
            // was shown shifted (`aonc`) while overwrite mode settled to `aon`.
            // Found by review.
            name: "overwrite_mode_s_burst_proves_no_shift",
            cfg: adaptive_on(Editor::Overwrite),
            ops: [
                &typed("abc")[..],
                &echoed(LEFT),
                &[
                    BACKSPACE,
                    glyph('o'),
                    Op::Program(2, 0),
                    Op::Advance(60),
                    Op::Deliver(1024),
                ],
                &echoed(glyph('n')),
            ]
            .concat(),
            mutants: vec![Host::Rule(Rule::ShiftProofDistinguishes)],
        },
        NamedCase {
            // `ab` proven, `←` echoed onto `b`, then `o`, `→` and a Backspace
            // typed before any is read. Overwrite mode reads `o` and replaces
            // `b`: `ao` with the cursor after it, which is the chain's state
            // after all three keys. Retired there, `→` and the Backspace were
            // credited before the far end read either. An in-place world shows
            // the screen with fewer keys read, so nothing is credited. Found by
            // review.
            name: "overwrite_mode_s_screen_with_fewer_keys_credits_none",
            cfg: adaptive_on(Editor::Overwrite),
            ops: [
                &typed("ab")[..],
                &echoed(LEFT),
                &[
                    glyph('o'),
                    RIGHT,
                    BACKSPACE,
                    Op::Program(1, 0),
                    Op::Advance(60),
                    Op::Deliver(1024),
                    Op::Program(2, 0),
                    Op::Advance(60),
                    Op::Deliver(1024),
                ],
            ]
            .concat(),
            mutants: vec![Host::Rule(Rule::InPlaceReadsFewer)],
        },
        NamedCase {
            // `n` typed inside the text, a shift whose kind is not proven, and
            // a Backspace after it, then `←`. Overwrite mode replaced the glyph
            // under the cursor with `n` and its rubout then erased `n`: the
            // line lost a glyph. Popped as a pair that cancels, the chain kept
            // its line, and `←` was shown with the lost `a` on glass while the
            // screen settled to `dse`. The Backspace is an op of the chain, an
            // erase, dark like the glyph before it. Found by review (its
            // sweep's seed index 152, case 65, shrunk: the failure that
            // `VacateRefusal` off exposed, `VacateRefusal` masking it there).
            // The transit wait now masks it too: `a`'s cell reads blank where
            // every state holds `a`, and the `←` in flight writes no blank
            // (`Rule::TransitBlanks`), so the chain is flushed before the frame;
            // the unit test of the same name holds the rule with both off.
            name: "a_backspace_cancels_no_glyph_of_an_unproven_shift",
            cfg: Config {
                remote: false,
                ..adaptive_on(Editor::Overwrite)
            },
            ops: vec![
                Op::Key(27, 1_475_804_828),
                Op::Key(55, 377_217_609),
                Op::Key(2, 1_868_172_738),
                Op::Key(3, 3_235_974_000),
                Op::Program(1, 4_023_946_444),
                Op::Deliver(259),
                Op::Key(87, 1),
                Op::Program(1, 1_378_646_736),
                Op::Key(57, 3_217_338_953),
                Op::Program(1, 1_086_249_451),
                Op::Key(78, 947_964_115),
                Op::Key(87, 1),
                Op::Deliver(1024),
                Op::Program(1, 600_413_991),
                Op::Advance(31),
                Op::Program(1, 1_936_873_531),
                Op::Deliver(125),
                Op::Deliver(22),
                Op::Program(1, 3_182_874_078),
                Op::Program(1, 0),
                Op::Deliver(1024),
            ],
            mutants: vec![],
        },
    ]
}

/// A case of the sweep over more seeds than the committed one, shrunk: on a
/// local far end, narrow, with the case's editor, right prompt and wrap policy.
fn swept(editor: Editor, rprompt: bool, forced_wrap: bool) -> Config {
    Config {
        remote: false,
        rprompt,
        forced_wrap,
        ..adaptive_on(editor)
    }
}

/// The interleavings found by running the sweep over more seeds than the
/// committed one, each shrunk and each a break of its clause before its fix:
/// seven by the review of increment 2 (four of them on main before the edit
/// lane too), and five by this round's own eight-hundred-seed sweep after
/// their fixes (two on one seed, the second once the first held), four in the
/// edit lane and one in the glyph lane. The last is the decoration rule's,
/// where nothing was wrong on glass.
fn edit_review_cases() -> Vec<NamedCase> {
    vec![
        NamedCase {
            // `dcee` on screen, a Backspace we did not place still unread. `n`,
            // `n` typed: a STALE ANCHOR at (0, 4), a cell right, the erase not
            // yet landed. A Backspace cancels the second `n`, and `i` is typed.
            // The first `n` echoes at (0, 3); the second `n`'s echo then lands
            // at (0, 4), the first guess's cell, with the cursor past it. The
            // stale anchor asked only about a later `n` still PENDING, and this
            // one was cancelled, owed typed: credited, the first `n` proved the
            // line at (0, 5) and `i` was painted there while it echoes at
            // (0, 4). Sweep seed index 151, case 409.
            name: "a_stale_run_is_not_credited_by_a_later_key_it_cancelled",
            cfg: swept(Editor::Readline, true, false),
            ops: vec![
                Op::Key(23, 3_975_185_203),
                Op::Key(0, 2),
                Op::Program(1, 0),
                Op::Key(0, 15),
                Op::Program(1, 0),
                Op::Advance(56),
                Op::Deliver(1024),
                Op::Key(0, 15),
                Op::Key(87, 1),
                Op::Advance(135),
                Op::Key(87, 2),
                Op::Advance(43),
                Op::Key(87, 2),
                Op::Program(4, 0),
                Op::Advance(27),
                Op::Key(70, 1),
                Op::Program(1, 0),
                Op::Advance(88),
                Op::Deliver(1024),
                Op::Key(18, 1_862_943_718),
                Op::Key(15, 811_336_316),
                Op::Advance(81),
                Op::Program(1, 4_069_894_191),
                Op::Advance(61),
                Op::Program(1, 3_504_997_513),
                Op::Deliver(1024),
                Op::Advance(61),
                Op::Key(77, 2_020_285_845),
                Op::Advance(55),
                Op::Key(18, 4_128_584_280),
                Op::Advance(85),
                Op::Advance(78),
                Op::Program(1, 2_845_744_319),
                Op::Deliver(34),
            ],
            mutants: vec![Host::Rule(Rule::StaleAnchor)],
        },
        NamedCase {
            // `d` and `a` typed; `d` echoes and the far end draws a dim
            // suggestion `ab` after it, the frame falling before its cursor
            // steps back: `dab` with the cursor at (0, 3). `a`'s cell shows `a`
            // with the cursor past it, so it retired; `n`, `n` typed next were
            // anchored at (0, 3), a cell right of where they echo, the first
            // credited with the second's echo, and `o` painted at (0, 5) while
            // it echoes at (0, 4). The cursor is two cells past the newest
            // guess's: no echo of the run puts it there. Sweep seed index 490,
            // case 407, which typed two spaces where this types `n`, `n`: a
            // space now proves no line by itself (`a_space_proves_no_line_by_itself`),
            // which holds that one too, and the rule's control needs a glyph.
            name: "a_cursor_past_every_echo_of_the_run_is_not_an_echo",
            cfg: swept(Editor::Readline, false, true),
            ops: vec![
                Op::Key(45, 1_413_385_414),
                Op::Advance(42),
                Op::Program(1, 364_165_579),
                Op::Key(48, 189_378_044),
                Op::Deliver(813),
                glyph('n'),
                Op::Program(1, 883_928_853),
                glyph('n'),
                Op::Program(1, 0),
                Op::Program(1, 0),
                Op::Advance(64),
                Op::Deliver(1024),
                Op::Key(0, 7),
            ],
            mutants: vec![Host::Rule(Rule::CursorReach)],
        },
        NamedCase {
            // `n` proven on a cooked tty; ` `, ` `, `s` typed, `s` cancelled by
            // a Backspace, `←`, `→`, `s` again, all within one round trip. The
            // tty echoes ` `, ` ` and the FIRST `s`: `n  s`, the cursor at (0,
            // 4), matches only the chain's last state. Retired front first, the
            // spaces and both arrows were credited before the re-typed `s` was
            // found owed (the cancelled `s`'s echo) and the chain flushed: two
            // arrows credited that the tty would echo as `^[[D`. Sweep seed
            // index 65, case 560.
            name: "a_chain_is_credited_whole_or_not_at_all",
            cfg: swept(Editor::Cooked, false, false),
            ops: vec![
                Op::Key(14, 3_700_411_327),
                Op::Key(48, 991_269_035),
                Op::Program(1, 2_140_597_800),
                Op::Deliver(1024),
                Op::Key(64, 2_943_271_238),
                Op::Key(44, 2_936_085_854),
                Op::Key(75, 1_151_846_133),
                Op::Key(87, 1),
                Op::Key(87, 2),
                Op::Key(37, 2_990_756_627),
                Op::Program(1, 3_955_518_544),
                Op::Program(1, 3_625_415_137),
                Op::Program(1, 1_881_084_364),
                Op::Deliver(1024),
            ],
            mutants: vec![Host::Rule(Rule::ChainAttributedWhole)],
        },
        NamedCase {
            // `o` typed; 187 ms later a Backspace cancels it; `o`'s echo lands,
            // the erase still unread; 82 ms later `o` is typed again, on the
            // cancelled cell. The cancelled glyph's typed owe was dated by its
            // own press and had lapsed: the old echo, still on screen until the
            // erase lands, was credited to the new `o`. Dated by the Backspace,
            // whose erase is what takes it off the screen, it holds. Sweep seed
            // index 455, case 569.
            name: "a_cancelled_glyph_is_owed_until_its_erase_is_due",
            cfg: swept(Editor::Cooked, true, false),
            ops: vec![
                Op::Key(17, 4_158_543_879),
                Op::Advance(57),
                Op::Advance(85),
                Op::Advance(45),
                Op::Key(77, 1_186_193_968),
                Op::Program(1, 268_707_047),
                Op::Deliver(191),
                Op::Advance(82),
                Op::Key(67, 1_752_612_451),
            ],
            mutants: vec![Host::Rule(Rule::CancelOwes)],
        },
        NamedCase {
            // A paste leaves `$  cisadd` with the cursor on the last `d`, at
            // (3, 8); `a`, `o`, `o`, `d` typed there, mid-line. Three ICH echoes
            // push that `d` to (3, 11), the last column, the cursor on it: what
            // the `d` guess's echo, parked in the deferred-wrap slot, would
            // show. Credited, it was the shifted tail's `d`, before the `d` key
            // was read. Sweep seed index 605, case 67.
            name: "a_shifted_tail_in_the_last_column_is_not_a_park",
            cfg: swept(Editor::ReadlineIch, false, false),
            ops: vec![
                Op::Key(2_517_048_684, 3_469_854_771),
                Op::Key(4_258_856_734, 2_892_766_870),
                Op::Program(3, 3_025_778_034),
                Op::Key(1_348_149_388, 2_260_744_365),
                Op::Key(2_944_813_698, 1_400_408_315),
                Op::Program(3, 1_308_541_924),
                Op::Advance(77),
                Op::Advance(54),
                Op::Advance(21),
                Op::Advance(31),
                Op::Deliver(1024),
                Op::Key(41, 92_207_024),
                Op::Key(68, 3_798_504_019),
                Op::Program(1, 1_897_849_624),
                Op::Key(50, 3_925_858_671),
                Op::Key(35, 2_321_211_751),
                Op::Program(1, 3_186_308_607),
                Op::Program(1, 522_419_628),
                Op::Advance(88),
                Op::Deliver(1024),
            ],
            mutants: vec![Host::Rule(Rule::ParkedAtTheEnd)],
        },
        NamedCase {
            // `e`, a combining acute and `←` typed (the mark declined, the arrow
            // declined with its pixels kept), then `i`, `e`. The far end moves
            // by code point: `←` put its cursor between `e` and the mark, so `i`
            // is inserted before the mark, which re-attaches to it (`i` and the
            // mark, then `e` and the mark). No cell right of `i` moved, and the
            // run, typed at the end of the row, proved: `e` was shown plain at
            // (0, 2) and settled to `é`. The echo's own cell came back opaque
            // where the press saw it plain. Sweep seed index 573, case 27.
            name: "an_echo_that_takes_a_mark_was_typed_before_it",
            cfg: swept(Editor::Readline, true, false),
            ops: vec![
                Op::Key(32, 541_568_668),
                Op::Key(12, 3_451_185_877),
                Op::Key(87, 1),
                Op::Program(1, 0),
                Op::Advance(51),
                Op::Deliver(1024),
                Op::Program(1, 0),
                Op::Advance(82),
                Op::Key(0, 5),
                Op::Program(1, 0),
                Op::Advance(52),
                Op::Deliver(1024),
                Op::Key(0, 15),
                Op::Program(1, 0),
                Op::Advance(76),
                Op::Deliver(1024),
            ],
            mutants: vec![Host::Rule(Rule::MarkedEcho)],
        },
        NamedCase {
            // `db` proven over ssh; a Backspace is armed as an erase, shown. The
            // far end prints `Password: ` on the next row, echo off, and moves
            // up a row (a reverse index): the chain's row is untouched and the
            // cursor stands on it at (0, 10), right of every state. Waited on as
            // a part-way echo, the chain kept displaying, and `8`, typed at the
            // password prompt, joined it and was painted at (0, 1). Found by the
            // eight-hundred-seed sweep of this round (seed index 230, case 315), not
            // by review.
            name: "a_chain_waits_only_for_a_cursor_its_echoes_can_reach",
            cfg: Config {
                rprompt: true,
                ..adaptive_on(Editor::Cooked)
            },
            ops: vec![
                Op::Key(3_024_918_327, 1_992_710_655),
                Op::Advance(61),
                Op::Program(2, 2_782_627_964),
                Op::Key(3_651_847_658, 1_377_204_047),
                Op::Deliver(122),
                Op::Program(3, 3_333_230_496),
                Op::Advance(45),
                Op::Deliver(1024),
                Op::Unprompted(Unprompted::PasswordNewRow, 1_607_206_130),
                Op::Key(1_238_439_470, 3_631_585_236),
                Op::Unprompted(Unprompted::ReverseIndex, 628_559_325),
                Op::Deliver(1024),
                Op::Key(1_944_211_913, 1_911_349_088),
            ],
            mutants: vec![Host::Rule(Rule::TransitReach)],
        },
        NamedCase {
            // `s i` proven, `←`, `←` echoed, the chain retiring with the cursor
            // on the line's space. `o` typed there is a mid-line insert, its
            // proof dropped, but the space reads blank, so the run looked typed
            // over nothing; readline writes `o` before it redraws the tail it
            // shifts, and the frame between showed `soi` with the row right of
            // `o` as the press saw it: `o` proved as an append, and ` `, typed
            // on that proof before the redraw landed, was painted over `i`,
            // which settled two cells right. A run started inside the known
            // text proves nothing. Found by the eight-hundred-seed sweep of this
            // round (seed index 361, case 559: a line that began with the
            // space), written out here on a line a glyph proves, since a space
            // now proves no line by itself.
            //
            // Since predictive editing's increment 3 a glyph typed inside the
            // known text is a SHIFT, an op of a state chain, not a glyph-lane
            // run: the frame between `o` and its tail is a part-way echo the
            // chain waits through, and ` ` joins the chain. So `TailGuard`, which
            // guards the glyph lane, no longer reaches this interleaving (its
            // controls are the sweep and `mid_line_over_a_space`); the run typed
            // inside the text that the lane cannot place (R-ROW, a line up to
            // the margin) proves nothing by `Rule::InsideRunsNeverProve`, pinned
            // with its control by the unit test
            // `a_run_started_inside_the_text_proves_nothing` (this world cannot
            // reach it inside the editor's model: `rule_clause`). The case stays:
            // the faithful predictor must hold it.
            name: "a_run_typed_inside_the_known_text_proves_nothing",
            cfg: adaptive_on(Editor::Readline),
            ops: [
                &typed("s i")[..],
                &echoed(LEFT),
                &echoed(LEFT),
                &[
                    Op::Advance(300),
                    glyph('o'),
                    Op::Program(1, 0),
                    Op::Advance(60),
                    Op::Deliver(1),
                    glyph(' '),
                    Op::Deliver(1024),
                    Op::Program(1, 0),
                    Op::Advance(60),
                    Op::Deliver(1024),
                ],
            ]
            .concat(),
            mutants: vec![],
        },
        NamedCase {
            // `i` proven after a space; `←`, `←` read together, the second
            // declined at the text's edge, and the far end steps the cursor onto
            // the space: the chain flushes, the known line with it. `o` typed on
            // the space; zle writes `o` before it redraws the tail it shifts, and
            // the frame between shows `oi` with the row right of `o` as the
            // press saw it, so `o` proves. ` ` is typed on that proof and `→`
            // declined at the edge with the pixels kept, which ends the epoch and
            // CARRIES ` `; the redraw lands (`o i`), and the standing tail, which
            // ended with the epoch, no longer looked: ` ` stood on glass over a
            // tail that had moved. The carried proof is held to its tail while
            // a carried guess is pending. Found by the eight-hundred-seed sweep
            // of this round (seed index 361, case 559), once a run typed inside
            // the known text proved nothing.
            name: "a_carried_line_is_held_to_its_standing_tail",
            cfg: swept(Editor::Zle, true, true),
            ops: vec![
                Op::Key(0, 9),
                Op::Program(1, 0),
                Op::Advance(20),
                Op::Deliver(1024),
                Op::Key(0, 5),
                Op::Program(1, 0),
                Op::Advance(43),
                Op::Deliver(1024),
                Op::Key(87, 1),
                Op::Key(87, 1),
                Op::Program(5, 0),
                Op::Deliver(1024),
                Op::Advance(490),
                Op::Key(60, 3_524_445_877),
                Op::Program(1, 1_839_723_715),
                Op::Deliver(18),
                Op::Key(16, 557_874_204),
                Op::Key(87, 2),
                Op::Deliver(1024),
            ],
            mutants: vec![Host::Rule(Rule::StandingTail)],
        },
        NamedCase {
            // `ab` proven at the start of an empty row with its motions; `←`,
            // `←` are armed, the chain's last state with the cursor on (0, 0).
            // The far end stops echoing (no prompt), Enter is pressed, and it
            // reads all three: its arrows draw nothing, and the Enter's carriage
            // return alone puts the cursor on (0, 0), the chain's last state.
            // Credited, both arrows were measured on the Enter's output. An edit
            // op is credited only with no later key still echoing. Found by the
            // eight-hundred-seed sweep of this round (seed index 508, case 102:
            // the erase of a space a line began with), written out here on a
            // line a glyph proves, since a space now proves no line by itself.
            name: "a_later_key_s_echo_is_not_an_edit_s",
            cfg: adaptive_on(Editor::Zle),
            ops: [
                &motions_proven_on_ab()[..],
                &[
                    LEFT,
                    LEFT,
                    Op::Unprompted(Unprompted::SilentNoEcho, 0),
                    Op::Key(79, 1),
                    Op::Program(3, 0),
                    Op::Advance(60),
                    Op::Deliver(1),
                ],
            ]
            .concat(),
            mutants: vec![Host::Rule(Rule::EditOwedAfter)],
        },
        NamedCase {
            // `ea o  ` typed and Enter, read and echoed; the far end prints
            // `Password:` (echo off) in pieces, and a space is typed with the
            // prompt's own trailing space not yet drawn, on its cell (1, 9).
            // That space then lands, the cursor past it: the credit of the
            // typed space, which proved the new line at (1, 10), where the
            // prompt ends, and `7`, typed at the password prompt, was painted.
            // A space proves no line by itself. Found by the eight-hundred-seed sweep
            // of this round (seed index 796, case 462), in the glyph lane.
            name: "a_space_proves_no_line_by_itself",
            cfg: Config {
                forced_wrap: true,
                rprompt: true,
                ..adaptive_on(Editor::ReadlineIch)
            },
            ops: vec![
                Op::Key(24, 1_337_264_648),
                Op::Key(0, 3_741_632_216),
                Op::Key(69, 3_365_632_231),
                Op::Key(63, 3_352_414_280),
                Op::Key(6, 3_355_817_594),
                Op::Advance(66),
                Op::Key(36, 2_937_571_973),
                Op::Key(86, 2_450_286_488),
                Op::Program(1, 1_015_739_813),
                Op::Deliver(1024),
                Op::Program(1, 4_082_722_814),
                Op::Program(1, 1_858_506_176),
                Op::Deliver(1024),
                Op::Program(1, 4_226_925_274),
                Op::Program(1, 510_017_910),
                Op::Program(1, 211_870_438),
                Op::Program(1, 2_811_016_764),
                Op::Deliver(531),
                Op::Advance(1176),
                Op::Deliver(931),
                Op::Key(15, 3_206_749_715),
                Op::Deliver(1024),
                Op::Key(57, 3_088_124_477),
            ],
            mutants: vec![Host::Rule(Rule::SpaceNeverProves)],
        },
        NamedCase {
            // `i` and a space typed and read; the far end prints ` Password: `
            // on the same row (echo off), and a screen clear then redraws `$ `.
            // `i`'s echo lands and proves the line; then the space's echo, the
            // prompt, the clear and `$` land in one burst, the cursor back on
            // the space's cell (`i`'s cell now `$`). `8` is typed at the
            // password read, and the redraw's trailing space lands: credited
            // with it, the space carried the proof `i` had earned, and `8` was
            // painted. The proof stands only while its echo's cell holds its
            // glyph. Found by the two-thousand-seed sweep of the review of
            // increment 3 (seed index 1121, case 571), a break on main before
            // it (the glyph lane alone).
            name: "a_redrawn_line_keeps_no_proof",
            cfg: Config {
                rprompt: true,
                bidi: true,
                ..adaptive_on(Editor::Autosuggest)
            },
            ops: vec![
                Op::Key(3_004_498_363, 400_119_926),
                Op::Key(907_211_601, 1_823_695_528),
                Op::Program(3, 1_608_380_017),
                Op::Unprompted(Unprompted::PasswordSameRow, 1_263_151_486),
                Op::Advance(89),
                Op::Deliver(54),
                Op::Unprompted(Unprompted::Clear, 1_094_166_945),
                Op::Deliver(1004),
                Op::Key(972_518_737, 1_141_601_958),
            ],
            mutants: vec![Host::Rule(Rule::StandingGlyph)],
        },
        NamedCase {
            // `n` typed and read, then cancelled by a Backspace, also read, and
            // a space typed on the cancelled cell. zle echoes `n` and erases it
            // with `\b \b`, and a frame falls after the blank and before the
            // step back: a blank on the cancelled cell with the cursor past
            // it, a space's echo exactly. Credited, the space was measured on
            // output that was not its own. The erase's echo is owed as a
            // space. Found by the two-thousand-seed sweep of the review of
            // increment 3 (seed index 1200, case 44), a break on main before it.
            // The step back onto the credited space's cell now takes that credit
            // back too (`Rule::CreditTakenBack`): each rule closes it alone, and
            // neither switched off breaks it (the unit test of the same name
            // holds `CancelOwesItsBlank` to the part-way frame, the cases of the
            // review of increment 4 hold `CreditTakenBack`).
            name: "a_cancelled_glyph_s_erase_is_no_space_s_echo",
            cfg: swept(Editor::Zle, true, true),
            ops: vec![
                Op::Key(34, 1_484_281_394),
                Op::Program(1, 1_163_281_073),
                Op::Key(73, 2_196_087_038),
                Op::Program(1, 778_873_047),
                Op::Key(40, 4_264_062_094),
                Op::Deliver(1021),
                Op::Deliver(376),
                Op::Program(1, 365_929_224),
                Op::Key(63, 602_220_650),
                Op::Deliver(1024),
            ],
            mutants: vec![],
        },
        NamedCase {
            // `n` typed and echoed, proving its line; a Backspace, read, and a
            // space typed after it: a chain, the erase at the end of the text
            // and the space appended where it leaves the cursor. zle's erase
            // (`\b \b`) is caught after its blank and before its step back: a
            // blank with the cursor past it, the chain's state after BOTH keys.
            // Retired there, the space was credited before the far end read it,
            // and once that space's own echo landed it credited the space typed
            // next. A state an earlier op's echo shows part-way is no evidence
            // of the keys after it. Found by the two-thousand-seed sweep of the
            // review of increment 3 (seed index 1200, case 44, once the glyph
            // lane's cancelled pair owed its blank).
            name: "an_erase_caught_part_way_is_no_later_state",
            cfg: swept(Editor::Zle, true, true),
            ops: vec![
                Op::Key(34, 1_484_281_394),
                Op::Program(1, 1_163_281_073),
                Op::Deliver(1024),
                Op::Key(73, 2_196_087_038),
                Op::Program(1, 778_873_047),
                Op::Key(40, 4_264_062_094),
                Op::Deliver(1021),
                Op::Deliver(376),
                Op::Program(1, 365_929_224),
                Op::Key(63, 602_220_650),
                Op::Deliver(1024),
            ],
            mutants: vec![Host::Rule(Rule::PartWayEchoes)],
        },
        NamedCase {
            // `sc` proven; Backspace, Backspace, `b`: the chain shows `b` with a
            // blank on the vacated (0, 1) and the cursor there. The far end
            // answers both erases and `b`, then draws a dim suggestion `e a`
            // from (0, 1) and steps back: the settled screen holds decoration
            // where the frame showed the vacated blank. A blank ghost is no
            // typed glyph: the frame lacked a decoration the far end drew,
            // which the decoration rule counts (`stale_decoration`). Sweep seed
            // index 189, case 64.
            name: "a_vacated_blank_under_a_suggestion_is_stale_decoration",
            cfg: Config {
                remote: false,
                ..adaptive_on(Editor::ReadlineIch)
            },
            ops: vec![
                Op::Key(0, 19),
                Op::Advance(64),
                Op::Program(1, 0),
                Op::Deliver(1024),
                Op::Key(0, 2),
                Op::Advance(65),
                Op::Program(1, 0),
                Op::Deliver(1024),
                Op::Key(73, 1_130_455_159),
                Op::Key(70, 823_801_729),
                Op::Key(29, 3_390_200_745),
                Op::Program(1, 1_809_224_116),
                Op::Program(1, 3_727_884_062),
                Op::Program(1, 1_775_561_435),
            ],
            mutants: vec![],
        },
    ]
}

/// Adaptive, remote, narrow, with the far end's line editor `editor`.
fn adaptive_on(editor: Editor) -> Config {
    Config {
        editor,
        ..adaptive_remote()
    }
}

/// `text` typed one glyph a round trip on a 60 ms link, each read and echoed.
fn typed(text: &str) -> Vec<Op> {
    text.chars().flat_map(|c| echoed(glyph(c))).collect()
}

/// A proven line `ab` whose motion class is proven too: `←` and `→` echoed in
/// turn, the cursor back at the end of the line.
fn motions_proven_on_ab() -> Vec<Op> {
    [&typed("ab")[..], &echoed(LEFT), &echoed(RIGHT)].concat()
}

/// Enter read with command `choice` and its output delivered, then a pause past
/// the window its owed echo holds: what follows is typed at a fresh prompt (or
/// in whatever program the command started).
fn enter_and_wait(choice: u32) -> [Op; 5] {
    [
        Op::Key(79, 1),
        Op::Program(1, choice),
        Op::Advance(60),
        Op::Deliver(1024),
        Op::Advance(300),
    ]
}

/// A FRESH PROMPT under the integration, its first key still to come: Enter
/// read from the empty screen, which draws the shell's first prompt, then Enter
/// pressed at that prompt, read with command `choice`, and its output
/// delivered. The second is a line terminator pressed at an authenticated
/// prompt, which opens THE FIRST KEY's gate (`Predictor::note_line_submit_at`).
fn at_a_fresh_prompt(choice: u32) -> Vec<Op> {
    [&enter_and_wait(0)[..], &enter_and_wait(choice)].concat()
}

/// The named cases of the edit lane (state chains on the known line).
fn edit_cases() -> Vec<NamedCase> {
    vec![
        NamedCase {
            // A cooked tty echoes `←` as `^[[D`, four cells after the line with the
            // cursor past them: no motion is ever exact there. So the class ladder
            // never shows one: the arrow is tracked dark, its echo matches no state
            // and the chain waits out its window. Shown on the glyph proof alone,
            // `ab` with the cursor on `b` was on glass while the screen settled to
            // `ab^[[D`.
            name: "a_cooked_tty_never_shows_a_motion",
            cfg: adaptive_on(Editor::Cooked),
            ops: [&typed("ab")[..], &echoed(LEFT)].concat(),
            mutants: vec![Host::Rule(Rule::MotionClassProof)],
        },
        NamedCase {
            // `←` proven at the shell; Enter starts `cat`, a cooked program, on
            // the next row; `cd` typed there proves that line, and `←` is pressed.
            // Its class proof went with the shell's line, so it stays dark; kept,
            // it showed the cursor on `d` while `cat`'s tty echoed `^[[D`.
            name: "a_proof_does_not_outlive_its_line",
            cfg: adaptive_on(Editor::ReadlineIch),
            ops: [
                &motions_proven_on_ab()[..],
                &enter_and_wait(11),
                &typed("cd"),
                &echoed(LEFT),
            ]
            .concat(),
            mutants: vec![Host::Rule(Rule::ProofsEndWithTheEpoch)],
        },
        NamedCase {
            // `ab` typed at a prompt and proven, `←` twice onto the line's first
            // glyph, then `←` again: at the start of the text the lane knows,
            // where readline beeps and the cursor stays. Armed, it showed the
            // cursor on the prompt's space while it settled on `a`.
            name: "a_motion_at_the_text_edge_beeps",
            cfg: adaptive_on(Editor::Readline),
            ops: [
                &enter_and_wait(0)[..],
                &typed("ab"),
                &echoed(LEFT),
                &echoed(LEFT),
                &echoed(LEFT),
            ]
            .concat(),
            mutants: vec![Host::Rule(Rule::TextEdges)],
        },
        NamedCase {
            // `c` typed at the end of a line whose motions are proven, `←`, then
            // Enter before any of them is read: the chain is carried across its
            // terminator with `c` painted and the cursor on it. Drawn one past the
            // newest glyph instead (the glyph lane's cursor), it was a key ahead
            // of the line the frame shows.
            name: "enter_with_keys_in_flight",
            cfg: adaptive_on(Editor::ReadlineIch),
            ops: [
                &motions_proven_on_ab()[..],
                &[
                    glyph('c'),
                    LEFT,
                    Op::Key(79, 1),
                    Op::Program(3, 0),
                    Op::Advance(60),
                    Op::Deliver(1024),
                ],
            ]
            .concat(),
            mutants: vec![Host::Rule(Rule::CarryCursor)],
        },
        NamedCase {
            // `c` typed at the end of a line whose motions are proven, then `←`:
            // the chain shows `c` with the cursor on it. A host drawing the glyph
            // lane's cursor, one past the newest glyph, drew it a cell right.
            name: "left_moves_the_cursor",
            cfg: adaptive_on(Editor::ReadlineIch),
            ops: [
                &motions_proven_on_ab()[..],
                &[
                    glyph('c'),
                    LEFT,
                    Op::Program(2, 0),
                    Op::Advance(60),
                    Op::Deliver(1024),
                ],
            ]
            .concat(),
            mutants: vec![Host::CursorAfterNewestGlyph],
        },
        NamedCase {
            // `abc de` proven with its motions, then Alt-B: readline goes back a
            // word, to `d`. Handed to the predictor as `←`, the frame put the
            // cursor on `e`.
            name: "a_word_motion_is_not_an_arrow",
            cfg: adaptive_on(Editor::Readline),
            ops: [
                &typed("abc de")[..],
                &echoed(LEFT),
                &echoed(RIGHT),
                &[
                    Op::Key(87, 9),
                    Op::Program(1, 0),
                    Op::Advance(60),
                    Op::Deliver(1024),
                ],
            ]
            .concat(),
            mutants: vec![Host::WordMotionAsArrow],
        },
        NamedCase {
            // The keypad's Left, while a Kitty mode that reports the keypad
            // (DISAMBIGUATE) is on, sends its own code (`CSI 57417 u`), which
            // readline leaves unbound: it beeps and the cursor stays. Handed to
            // the predictor as `←`, the frame moved the cursor. (Under the
            // legacy encoder the keypad's Left is the main block's `ESC [ D`,
            // an edit key like `←`.)
            name: "a_keypad_arrow_kitty_reports_is_not_a_motion",
            cfg: adaptive_on(Editor::Readline),
            ops: [
                &motions_proven_on_ab()[..],
                &[
                    Op::Key(87, 25),
                    Op::Program(1, 0),
                    Op::Advance(60),
                    Op::Deliver(1024),
                ],
            ]
            .concat(),
            mutants: vec![Host::KeypadArrowAsArrow],
        },
        NamedCase {
            // `←` and `→` pressed in one burst on a line whose motions are proven,
            // a frame before either is read: the screen shows the state before
            // them, which is also the state after them. The smallest match retires
            // nothing, and the net-null tail retires uncredited, owed. The largest
            // match, or a credited net-null tail, credited two keys whose output
            // never began.
            name: "an_ambiguous_match_proves_nothing",
            cfg: adaptive_on(Editor::Readline),
            ops: [
                &motions_proven_on_ab()[..],
                &[LEFT, RIGHT, Op::Advance(5), Op::Frame],
            ]
            .concat(),
            mutants: vec![
                Host::Rule(Rule::SmallestMatch),
                Host::Rule(Rule::NetNullUncredited),
            ],
        },
        NamedCase {
            // `abcdei` proven, `←` three times echoed, a fourth pressed (shown),
            // then a right prompt drawn over `ei` by the far end, the cursor
            // restored where the unread `←` leaves the chain's first state. That
            // state's cursor matches, its text does not: a whole-state match
            // flushes. Matched by the cursor alone, the chain kept its own `ei`
            // and painted it over the right prompt the screen shows, which the
            // keys folded over that screen do not give.
            name: "a_redrawn_line_retires_the_edits",
            cfg: adaptive_on(Editor::ReadlineIch),
            ops: [
                &typed("abcdei")[..],
                &echoed(LEFT),
                &echoed(LEFT),
                &echoed(LEFT),
                &[
                    LEFT,
                    Op::Unprompted(Unprompted::RightPrompt, 0),
                    Op::Deliver(1024),
                ],
            ]
            .concat(),
            mutants: vec![Host::Rule(Rule::WholeStateMatch)],
        },
        NamedCase {
            // `←` shown on a line whose motions are proven, and the far end never
            // reads it: the chain expires whole at its head's window. A chain that
            // never expires was on glass a second after its key.
            name: "a_chain_no_echo_answers_expires",
            cfg: adaptive_on(Editor::ReadlineIch),
            ops: [&typed("ab")[..], &echoed(LEFT), &[LEFT, Op::Advance(1000)]].concat(),
            mutants: vec![Host::Rule(Rule::StateExpiryFlushes)],
        },
        NamedCase {
            // Two spaces and `d` echoed, `→` at the end of the line (declined: an
            // edge), then a Backspace that cancels nothing of ours, and `c`, `c`,
            // `c` typed while the erase is still unread: the run is placed on the
            // live cursor, a cell right of where the erase leaves it, a STALE
            // ANCHOR. The first `c` echoes a cell left of its guess, and the
            // second `c`'s echo then stands in the first guess's cell with the
            // cursor past it. The Backspace's owe has lapsed by then; credited, the
            // first guess proved the line a cell behind and the third `c` was
            // painted where the second echoed. Shrunk from seed 76, case 91 of the
            // eighty-seed sweep of the edit lane's world.
            name: "a_run_placed_before_an_erase_lands_is_not_credited_by_the_next_key",
            cfg: Config {
                remote: false,
                rprompt: true,
                ..adaptive_on(Editor::Readline)
            },
            ops: vec![
                Op::Key(63, 4_278_273_584),
                Op::Key(2, 2_370_323_365),
                Op::Program(1, 0),
                Op::Key(0, 3),
                Op::Program(1, 0),
                Op::Deliver(1024),
                Op::Advance(134),
                Op::Key(87, 2),
                Op::Program(1, 0),
                Op::Deliver(1024),
                Op::Key(70, 1),
                Op::Program(1, 0),
                Op::Advance(63),
                Op::Advance(77),
                Op::Key(64, 269_991_312),
                Op::Program(1, 491_738_566),
                Op::Key(49, 3_243_435_352),
                Op::Program(1, 1_473_769_924),
                Op::Advance(84),
                Op::Deliver(1024),
                Op::Advance(73),
                Op::Program(1, 3_454_492_486),
                Op::Key(45, 4_053_137_784),
                Op::Deliver(1024),
            ],
            mutants: vec![Host::Rule(Rule::StaleAnchor)],
        },
    ]
}

/// Adaptive, remote, narrow, the far end's line editor `editor` binding the
/// learned keys as `keymap`, under aterm's shell integration (signed marks,
/// the nonce gate on).
fn integrated(editor: Editor, keymap: Keymap) -> Config {
    Config {
        integration: true,
        keymap,
        ..adaptive_on(editor)
    }
}

/// The line's start and end keys `(start, end)` each pressed twice in turn,
/// read and echoed on its own, from the end of the line: on a far end that
/// binds them canonically, the two echoes each that teach both.
fn learn_edges(start: Op, end: Op) -> Vec<Op> {
    [
        &echoed(start)[..],
        &echoed(end),
        &echoed(start),
        &echoed(end),
    ]
    .concat()
}

/// The op that presses Tab (a completion, input the predictor does not model)
/// and draws a frame.
const TAB: Op = Op::Key(87, 13);

/// The named cases of the LEARNED KEYS (Home, End, Ctrl-A, Ctrl-E, Delete):
/// placed only on a line started at the shell's authenticated buffer start,
/// shown only once learned in the editor context.
fn line_edge_cases() -> Vec<NamedCase> {
    vec![
        NamedCase {
            // `zsh -f` leaves Home unbound: it beeps and the cursor stays. A
            // probe is never shown, and its expiry unlearns nothing it never
            // learned. Shown as beginning-of-line on the glyph proof alone, the
            // cursor stood on `a` while the screen settled with it after `b`.
            name: "an_unbound_home_is_not_predicted",
            cfg: integrated(Editor::Zle, Keymap::ZshVanilla),
            ops: [&at_a_fresh_prompt(0)[..], &typed("ab"), &echoed(HOME)].concat(),
            mutants: vec![Host::Rule(Rule::LearnedBindings)],
        },
        NamedCase {
            // `zsh -f` reads Delete as an undefined prefix and a `~`, which it
            // inserts. Shown as delete-char, `b` was gone from a frame whose
            // line settled to `a~bc`.
            name: "delete_without_a_delete_binding",
            cfg: integrated(Editor::Zle, Keymap::ZshVanilla),
            ops: [
                &at_a_fresh_prompt(0)[..],
                &typed("abc"),
                &echoed(LEFT),
                &echoed(LEFT),
                &echoed(DELETE),
            ]
            .concat(),
            mutants: vec![Host::Rule(Rule::LearnedBindings)],
        },
        NamedCase {
            // zsh's vi insert mode inserts Ctrl-A, drawn `^A`. Shown as
            // beginning-of-line, the cursor stood on `a`.
            name: "ctrl_a_in_vi_insert_mode_is_a_glyph",
            cfg: integrated(Editor::Zle, Keymap::ViIns),
            ops: [&at_a_fresh_prompt(0)[..], &typed("ab"), &echoed(CTRL_A)].concat(),
            mutants: vec![Host::Rule(Rule::LearnedBindings)],
        },
        NamedCase {
            // Home and End learned at an integrated prompt; the shell then
            // re-executes as `zsh -f` (Home unbound), re-keyed, and prompts
            // anew. Enter there opens the first-key gate at the new shell's
            // prompt. A new nonce is a new editor context: Home is a probe there,
            // dark. A host handing one context forever showed the binding the
            // first shell taught, the cursor on `c` while the second beeped.
            name: "a_new_shell_relearns_its_bindings",
            cfg: integrated(Editor::Zle, Keymap::Canonical),
            ops: [
                &at_a_fresh_prompt(0)[..],
                &typed("ab"),
                &learn_edges(HOME, END),
                &[
                    Op::Unprompted(Unprompted::Rebind, 0),
                    Op::Deliver(1024),
                    Op::Advance(300),
                ],
                &enter_and_wait(0),
                &typed("cd"),
                &echoed(HOME),
            ]
            .concat(),
            mutants: vec![Host::StaleEditorContext],
        },
        NamedCase {
            // Home learned on `ab`; Tab completes `ion `, which ends the epoch,
            // and `cd` typed after it starts a line mid-buffer. Home there is
            // not modelled: the line is not the buffer. Placed on any line with
            // an authenticated B on its row, it showed the cursor on `c` while
            // the far end went to `a`.
            name: "home_after_a_tab_completion_is_not_predicted",
            cfg: integrated(Editor::Zle, Keymap::Canonical),
            ops: [
                &at_a_fresh_prompt(0)[..],
                &typed("ab"),
                &learn_edges(HOME, END),
                &echoed(TAB),
                &[Op::Advance(300)],
                &typed("cd"),
                &echoed(HOME),
            ]
            .concat(),
            mutants: vec![Host::Rule(Rule::WitnessFreshPrompt)],
        },
        NamedCase {
            // Home learned on `ab`; the line pushed onto zsh's buffer stack
            // (push-line), Enter pressed on the empty line, and the next prompt
            // pops `ab` pre-filled: zle draws it before zle-line-init runs, so
            // the shell's `B` lands after it and does not say it stands where
            // the buffer starts. A program then prints an unsigned OSC 133 `A`
            // and a `B` that says so, at the cursor. The nonce gate drops them,
            // and no start is handed; a host that never turned the gate on took
            // that B for the buffer's start, so `c`, the prompt's first key,
            // typed there started a witnessed line, and Home showed the cursor
            // on `c` while the far end went to `a`.
            name: "a_forged_prompt_mark_moves_no_home",
            cfg: integrated(Editor::Zle, Keymap::Canonical),
            ops: [
                &at_a_fresh_prompt(0)[..],
                &typed("ab"),
                &learn_edges(HOME, END),
                &echoed(PUSH_LINE),
                &enter_and_wait(0),
                &[
                    Op::Unprompted(Unprompted::ForgedMark, 0),
                    Op::Deliver(1024),
                    Op::Advance(300),
                ],
                &typed("c"),
                &echoed(HOME),
            ]
            .concat(),
            mutants: vec![Host::UnsignedInputStart],
        },
        NamedCase {
            // The same pre-filled prompt with no program printing anything:
            // the shell's own signed `B`, after the line zle popped from its
            // buffer stack, does not say it stands where the buffer starts
            // (aterm's integration says so only over an empty buffer), and
            // aterm-core names no start. A host that took the signed `B` of the
            // prompt in front wherever it stood made `c` start a witnessed line
            // at the end of `ab`, and Home showed the cursor on `c` while zle
            // went to `a` (measured on zsh 5.9: accept-and-hold's prompt drew
            // `P> echo hi` with the mark at column 10).
            name: "a_line_pre_filled_from_the_buffer_stack_has_no_buffer_start",
            cfg: integrated(Editor::Zle, Keymap::Canonical),
            ops: [
                &at_a_fresh_prompt(0)[..],
                &typed("ab"),
                &learn_edges(HOME, END),
                &echoed(PUSH_LINE),
                &enter_and_wait(0),
                &typed("c"),
                &echoed(HOME),
            ]
            .concat(),
            mutants: vec![Host::UnattestedInputStart],
        },
        NamedCase {
            // THE FIRST KEY. `ab` typed at the B (witnessed) and its edges
            // learned, both rubbed out, two spaces typed, Home echoed (the
            // cursor back on the B) and Home pressed again there, which moves
            // nothing and is not modelled: the line goes. `c` typed at the B's
            // cell then starts a run there, but not as the prompt's first key:
            // the buffer holds the two spaces after it, which the screen draws
            // as blanks. Its line is no witnessed one, and Home and End are not
            // placed. Taken for the buffer's start, End showed the cursor after
            // `c` while the far end went past the spaces (the review of
            // increment 4).
            name: "only_the_first_key_meets_the_empty_buffer",
            cfg: integrated(Editor::Zle, Keymap::Canonical),
            ops: [
                &at_a_fresh_prompt(0)[..],
                &typed("ab"),
                &learn_edges(HOME, END),
                &echoed(BACKSPACE),
                &echoed(BACKSPACE),
                &[Op::Advance(300)],
                &typed("  "),
                &echoed(HOME),
                &echoed(HOME),
                &[Op::Advance(300)],
                &typed("c"),
                &echoed(HOME),
                &echoed(END),
            ]
            .concat(),
            mutants: vec![Host::Rule(Rule::WitnessFirstKey)],
        },
        NamedCase {
            // A prompt drawn wide (`$$$ `, its B at column 4), `ab` typed and
            // its edges learned; then zsh's `reset-prompt` redraws it narrower
            // (`$ `), the buffer after it, and sends no new B: the engine keeps
            // the old one at column 4, where the cursor now stands. `s`, typed
            // there mid-buffer, is no prompt's first key: its line is not
            // witnessed and Home is not placed. Taken for the buffer's start,
            // Home showed the cursor on `s` while zle went to `a`.
            name: "a_b_left_behind_by_a_narrower_redraw_witnesses_nothing",
            cfg: integrated(Editor::Zle, Keymap::Canonical),
            ops: [
                &enter_and_wait(0)[..],
                &[
                    Op::Unprompted(Unprompted::ResetPrompt, 0),
                    Op::Deliver(1024),
                ],
                &enter_and_wait(0),
                &typed("ab"),
                &learn_edges(HOME, END),
                &[
                    Op::Unprompted(Unprompted::ResetPrompt, 0),
                    Op::Deliver(1024),
                    Op::Advance(300),
                ],
                &typed("s"),
                &echoed(HOME),
            ]
            .concat(),
            mutants: vec![Host::Rule(Rule::WitnessFirstKey)],
        },
        NamedCase {
            // Home learned at a prompt the shell drew after clearing the
            // screen, its B at (0, 2); then a command clears it again and
            // reads a line, cooked, after a `$ ` of its own, where the shell's
            // stood. A command runs: no input region. A host handing on the
            // last start it read, a cell no line terminator's prompt names, took
            // the program's prompt for the shell's, and Home showed the cursor on
            // `c` while the tty echoed `^[[H`. (Handed with the prompt it was read
            // at, the first-key gate refuses it: Enter was pressed there.)
            name: "a_program_s_prompt_is_no_input_region",
            cfg: integrated(Editor::Zle, Keymap::Canonical),
            ops: [
                &at_a_fresh_prompt(6)[..],
                &typed("ab"),
                &learn_edges(HOME, END),
                &enter_and_wait(19),
                &typed("cd"),
                &echoed(HOME),
            ]
            .concat(),
            mutants: vec![Host::StaleInputStart],
        },
        NamedCase {
            // Ctrl-A and Ctrl-E learned; the shell then pushes Kitty
            // `DISAMBIGUATE`, under which Ctrl-A goes out as `CSI 97;5u`, which
            // it does not bind (a beep). Not modelled (R-KITTY), and the
            // context it is pressed in is another. A host that never asks the
            // Kitty mode showed the cursor on `a`.
            name: "ctrl_a_under_kitty_disambiguate",
            cfg: integrated(Editor::Zle, Keymap::Canonical),
            ops: [
                &at_a_fresh_prompt(0)[..],
                &typed("ab"),
                &learn_edges(CTRL_A, CTRL_E),
                &[
                    Op::Unprompted(Unprompted::KittyChords, 0),
                    Op::Deliver(1024),
                ],
                &echoed(CTRL_A),
            ]
            .concat(),
            mutants: vec![Host::IgnoresKittyChords],
        },
        NamedCase {
            // As above under xterm's modifyOtherKeys at level 2 (`CSI > 4;2 m`,
            // a precmd's or a program's left behind): Ctrl-A goes out as
            // `CSI 27;5;97~`, which zle does not decode. Not modelled
            // (R-KITTY). A host that asked only Kitty's mode showed the cursor
            // on `a` (the review of increment 4).
            name: "ctrl_a_under_modify_other_keys",
            cfg: integrated(Editor::Zle, Keymap::Canonical),
            ops: [
                &at_a_fresh_prompt(0)[..],
                &typed("ab"),
                &learn_edges(CTRL_A, CTRL_E),
                &[
                    Op::Unprompted(Unprompted::KittyChords, 1),
                    Op::Deliver(1024),
                ],
                &echoed(CTRL_A),
            ]
            .concat(),
            mutants: vec![Host::IgnoresModifyOtherKeys],
        },
        NamedCase {
            // Delete LEARNED and shown shifting the tail: `abcd` typed, two `←`,
            // and Delete twice at the cursor, each a probe of delete-char its
            // echo credits (`c`, then `d`, gone, the cursor kept); `on` typed at
            // the end, three `←`, and Delete shown at once: `o` gone, `n` drawn
            // where it stood (moved from the cell right of it), the last cell
            // vacated, the cursor where it was. A host drawing a chain's cursor
            // one past the newest ghost it painted put it on the vacated cell.
            name: "a_learned_delete_shifts_the_tail",
            cfg: integrated(Editor::Zle, Keymap::Canonical),
            ops: [
                &at_a_fresh_prompt(0)[..],
                &typed("abcd"),
                &echoed(LEFT),
                &echoed(LEFT),
                &echoed(DELETE),
                &echoed(DELETE),
                &typed("on"),
                &echoed(LEFT),
                &echoed(LEFT),
                &echoed(LEFT),
                &echoed(DELETE),
            ]
            .concat(),
            mutants: vec![Host::CursorAfterNewestGlyph],
        },
        NamedCase {
            // Ctrl-E learned (each press with the cursor inside the text); `c`
            // typed, and its echo brings up a suggestion, `ion`, after it.
            // Ctrl-E (end-of-line) at the end of the line ACCEPTS the
            // suggestion, and `d` typed before either is read, with no frame
            // between them, lands after it. There it is declined with its pixels
            // kept. Armed as a line end that moves nothing, the frame showed
            // `abcd`, `d` over the suggestion's `i`, while the line settled to
            // `abciond`.
            name: "end_accepts_the_autosuggestion",
            cfg: integrated(Editor::Autosuggest, Keymap::Canonical),
            ops: [
                &at_a_fresh_prompt(0)[..],
                &typed("ab"),
                &echoed(LEFT),
                &echoed(CTRL_E),
                &echoed(LEFT),
                &echoed(CTRL_E),
                &[
                    glyph('c'),
                    Op::Program(1, 3),
                    Op::Advance(60),
                    Op::Deliver(1024),
                    // Ctrl-E with no frame after it (`Sim::key_for`).
                    Op::Key(87, 20),
                    glyph('d'),
                    Op::Program(2, 0),
                    Op::Advance(60),
                    Op::Deliver(1024),
                ],
            ]
            .concat(),
            mutants: vec![Host::Rule(Rule::EndRefusal)],
        },
        NamedCase {
            // `ca` typed at a prompt drawn after a clear; Ctrl-A echoed, the
            // cursor on `c`; Ctrl-E read, its echo, the line reprinted from the
            // cursor, delivered only as far as `c`, the cursor on `a`. Then `←`
            // and Ctrl-E pressed, unread: the chain's state after `←` has the
            // cursor on `a` too, and the first Ctrl-E's echo caught part-way
            // matched it, so `←` was credited unread, and once the reprint went
            // on the second Ctrl-E was credited with the first one's output. A
            // line edge's echo moves the cursor over the text a cell at a time,
            // and caught part-way it is no later state. Found by the
            // three-hundred-seed sweep of increment 4 (seed index 266, case 427).
            name: "a_line_end_caught_part_way_is_no_later_state",
            cfg: Config {
                rprompt: true,
                ..integrated(Editor::Zle, Keymap::ZshVanilla)
            },
            ops: [
                &at_a_fresh_prompt(6)[..],
                &typed("ca"),
                &echoed(CTRL_A),
                &[
                    CTRL_E,
                    Op::Program(1, 0),
                    Op::Deliver(1014),
                    LEFT,
                    CTRL_E,
                    Op::Deliver(168),
                ],
            ]
            .concat(),
            mutants: vec![Host::Rule(Rule::PartWayEchoes)],
        },
        NamedCase {
            // A combining mark typed after `a`, then `←` (declined with its
            // pixels kept): zle moves by code point, so its cursor stands
            // between `a` and the mark, and every glyph typed after it is
            // inserted before the mark, written with the mark after it. `e`,
            // typed into the last column, wraps: zle writes it, moves to the next
            // row, and writes the mark there, on a blank first cell, the cell of
            // the guess `c` typed after `e`. That cell reads opaque and blank;
            // waited on as a blank, `c` was painted plain while it settled
            // marked. Found by the three-hundred-seed sweep of increment 4 (seed
            // index 139, case 190), a break on main before it: no integration.
            name: "a_mark_carried_onto_the_next_guess_s_cell_flushes_it",
            cfg: Config {
                remote: false,
                forced_wrap: true,
                rprompt: true,
                ..adaptive_on(Editor::Autosuggest)
            },
            ops: vec![
                Op::Key(79, 1),
                Op::Program(1, 8),
                Op::Deliver(1024),
                Op::Advance(604),
                Op::Key(0, 19),
                Op::Advance(93),
                Op::Key(0, 15),
                Op::Key(27, 139_739_933),
                Op::Program(1, 3_445_152_002),
                Op::Program(1, 3_156_675_808),
                Op::Program(1, 1_476_264_012),
                Op::Key(23, 1_424_407_698),
                Op::Key(40, 2_986_212_544),
                Op::Program(1, 2_765_006_362),
                Op::Key(46, 3_582_249_901),
                Op::Program(1, 2_151_984_872),
                Op::Program(1, 4_096_582_094),
                Op::Key(87, 1),
                Op::Program(1, 1_641_130_639),
                Op::Key(23, 731_405_227),
                Op::Deliver(1024),
                Op::Program(1, 2_534_162_415),
                Op::Key(4, 1_487_521_832),
                Op::Program(1, 1_625_387_612),
                Op::Key(0, 22),
                Op::Program(1, 0),
                Op::Key(0, 5),
                Op::Program(1, 0),
                Op::Advance(102),
                Op::Deliver(1024),
                Op::Key(0, 15),
                Op::Advance(172),
                Op::Key(0, 2),
                Op::Program(1, 0),
                Op::Deliver(1024),
            ],
            mutants: vec![Host::Rule(Rule::OpaqueHead)],
        },
    ]
}

/// A host paints a state chain ALL OR NONE, and draws its cursor only on a cell
/// that is no wide glyph's right half. The world cannot reach the frames that
/// tell (a chain flushes on any opaque cell of its line, and its cells and
/// cursor are inside it), so the painter is held here on one that does: a row
/// holding a wide glyph, a chain whose second cell, or whose cursor, falls on
/// its right half, or a shift whose moved glyph would come from it, which the
/// span painter refuses whole. aterm's host paints none and draws the engine's
/// cursor; a host painting what it can breaks ATOMICITY, and so does one drawing
/// the chain's cursor over a span the painter refused; one drawing the cursor
/// without asking about its cell breaks CURSOR LOCALITY.
#[test]
fn the_chain_painter_paints_all_or_none() {
    let mut term = aterm_core::terminal::Terminal::new(2, 12);
    term.process("ab日c".as_bytes());
    let mut frame = aterm_core::render::RenderInput::empty();
    term.cell_frame_into(&mut frame, 2, 12);
    let blank = term.implicit_blank_render_cell();
    let geometry = (2, 12);
    let cells = [(0, 1, 'x'), (0, 3, 'y')];
    let typed = [GhostCell::Typed; 2];
    let motion: [(u16, u16, char); 0] = [];
    // A shift whose moved glyph would come from the wide glyph's lead: the
    // span painter refuses the whole span.
    let shift = [(0, 1, '日')];
    let moved = [GhostCell::Moved { from: 2 }];
    for (shown, kinds, cursor, mutant) in [
        (&cells[..], &typed[..], (0, 4), Host::PaintsPartialState),
        (&motion[..], &[][..], (0, 3), Host::CursorOnAContinuation),
        (&shift[..], &moved[..], (0, 1), Host::CursorWithoutSpan),
    ] {
        for host in [Host::Faithful, mutant] {
            let mut d = frame.clone();
            let (painted, drawn) =
                paint_chain(&mut d, (shown, kinds), Some(cursor), (12, blank), host);
            let clauses = chain_paint_clauses(shown, &painted, drawn, &d, geometry);
            match (host, clauses) {
                (Host::Faithful, Ok(())) => {
                    assert!(painted.iter().all(|&p| !p) && drawn.is_none(), "{shown:?}");
                }
                (Host::Faithful, Err(v)) => {
                    panic!("aterm's painter broke `{}`: {}", v.clause, v.detail)
                }
                (_, Err(v)) if Some(v.clause) == host.breaks() => {}
                (_, other) => panic!("{host:?} must break {:?}, got {other:?}", host.breaks()),
            }
        }
    }
}

/// Every predictor rule that keeps a clause has a negative control: the sweep
/// catches it (`MUTANT_HOSTS`, run by the checker test) or a named case does
/// (`each_named_case_holds_and_catches_its_mutant`). The rules come from
/// `Rule::ALL`, which the predictor derives from the enum, and `rule_clause`'s
/// exhaustive match makes a new rule name its clause or `None`, so a rule added
/// with a clause and no control fails here instead of asserting nothing.
#[test]
fn every_rule_has_a_negative_control() {
    let named: Vec<Host> = named_cases().into_iter().flat_map(|c| c.mutants).collect();
    for &rule in Rule::ALL {
        if rule_clause(rule).is_some() {
            let host = Host::Rule(rule);
            assert!(
                MUTANT_HOSTS.contains(&host) || named.contains(&host),
                "{rule:?} keeps a clause but nothing catches it switched off"
            );
        }
    }
}

/// Every named case holds for aterm's predictor and host, and breaks its
/// clause, at the case itself, with its rule switched off.
#[test]
fn each_named_case_holds_and_catches_its_mutant() {
    for case in named_cases() {
        let mut trace = Some(String::new());
        if let Err((at, v)) = run_traced(case.cfg, &case.ops, &mut trace) {
            panic!(
                "{}: op {at} broke `{}`: {}\ntrace:\n{}",
                case.name,
                v.clause,
                v.detail,
                trace.unwrap_or_default()
            );
        }
        for &host in &case.mutants {
            let clause = host.breaks().expect("a named case names mutants");
            let mutant = Config { host, ..case.cfg };
            let mut trace = Some(String::new());
            match run_traced(mutant, &case.ops, &mut trace) {
                Err((_, v)) if v.clause == clause => {}
                other => panic!(
                    "{}: {host:?} must break `{clause}`, got {:?}\ntrace:\n{}",
                    case.name,
                    other.map_err(|(at, v)| (at, v.clause, v.detail)),
                    trace.unwrap_or_default()
                ),
            }
        }
    }
}

/// The breaks the review of increment 4 found with a sweep of two thousand
/// seeds (seed index `i` is the committed seed xor `i` times the golden-ratio
/// constant), each on main before the increment, each a named case with the
/// rule that closes it switched off as its control.
fn review_4_cases() -> Vec<NamedCase> {
    vec![
        NamedCase {
            // `oi` echoed and proving while `a`, a space and `e` were typed
            // ahead, `e` wrapping onto the next row; then `\r\nPassword: ` on
            // that row and a reverse index back onto `a`, in one burst. The
            // floor under the line holds a guess in flight, which is no reason
            // to read none of it: the cells no guess names show text the press
            // saw no trace of, and the proof is void; the digit typed at the
            // password read was shown after `e` (this round's sweep, seed index
            // 2146, case 131, once the floor skipped a row a guess wrapped onto).
            name: "a_prompt_beside_a_wrapped_guess_voids_the_proof",
            cfg: Config {
                editor: Editor::Zle,
                integration: true,
                keymap: Keymap::ZshVanilla,
                ..adaptive_remote()
            },
            ops: vec![
                Op::Key(79, 1),
                Op::Program(1, 16),
                Op::Key(0, 19),
                Op::Advance(248),
                Op::Key(0, 5),
                Op::Program(2, 0),
                Op::Advance(37),
                Op::Deliver(1024),
                Op::Key(0, 22),
                Op::Advance(239),
                Op::Key(0, 19),
                Op::Program(2, 0),
                Op::Advance(44),
                Op::Deliver(1024),
                Op::Key(0, 6),
                Op::Key(0, 2),
                Op::Advance(121),
                Op::Program(2, 0),
                Op::Advance(63),
                Op::Deliver(1024),
                Op::Key(0, 7),
                Op::Advance(242),
                Op::Key(0, 5),
                Op::Program(2, 0),
                Op::Key(53, 204_191_344),
                Op::Unprompted(Unprompted::PasswordNewRow, 2_939_349_910),
                Op::Unprompted(Unprompted::ReverseIndex, 663_125_524),
                Op::Key(3_506_091_848, 2_653_026_509),
                Op::Key(1_236_596_240, 2_291_053_356),
                Op::Deliver(1024),
                Op::Key(3_448_654_510, 1_296_950_694),
            ],
            mutants: vec![Host::Rule(Rule::FloorBelow)],
        },
        NamedCase {
            // The same burst with more typed ahead: `d` and `n` fill the line
            // to the margin, and `aisbda e` wraps onto the floor, so every
            // cell `Password:` is printed over is one a guess names. A guess
            // naming a cell excuses its own glyph there (the echo of a glyph
            // typed ahead lands on the floor), never a different one: `P` over
            // the guessed `a` is the prompt, and the proof is void; the digit
            // typed at the password read was shown after `e` (the second sweep
            // of this round, seed index 2146, case 131, once the floor excused
            // every cell a guess named).
            name: "a_prompt_drawn_over_the_guesses_on_the_floor_voids_the_proof",
            cfg: Config {
                editor: Editor::Zle,
                integration: true,
                keymap: Keymap::ZshVanilla,
                ..adaptive_remote()
            },
            ops: vec![
                Op::Key(79, 1),
                Op::Program(1, 16),
                Op::Key(0, 19),
                Op::Advance(248),
                Op::Key(0, 5),
                Op::Program(2, 0),
                Op::Advance(37),
                Op::Deliver(1024),
                Op::Key(0, 22),
                Op::Advance(239),
                Op::Key(0, 19),
                Op::Program(2, 0),
                Op::Advance(44),
                Op::Deliver(1024),
                Op::Key(0, 6),
                Op::Key(0, 2),
                Op::Advance(121),
                Op::Program(2, 0),
                Op::Advance(63),
                Op::Deliver(1024),
                Op::Key(0, 7),
                Op::Key(0, 5),
                Op::Program(2, 0),
                Op::Advance(62),
                Op::Key(18, 1_693_025_569),
                Op::Deliver(1024),
                Op::Key(0, 2_867_950_350),
                Op::Key(19, 1_734_703_712),
                Op::Key(8, 1_330_556_869),
                Op::Key(52, 3_472_201_932),
                Op::Key(30, 4_106_978_364),
                Op::Key(43, 2_595_488_359),
                Op::Key(19, 3_563_359_556),
                Op::Key(53, 204_191_344),
                Op::Unprompted(Unprompted::PasswordNewRow, 2_939_349_910),
                Op::Unprompted(Unprompted::ReverseIndex, 663_125_524),
                Op::Key(3_506_091_848, 2_653_026_509),
                Op::Key(1_236_596_240, 2_291_053_356),
                Op::Deliver(1024),
                Op::Key(3_448_654_510, 1_296_950_694),
            ],
            mutants: vec![Host::Rule(Rule::FloorBelow)],
        },
        NamedCase {
            // A combining mark typed first at a fresh prompt joins the
            // prompt's trailing space; `n` typed and rubbed out with
            // `\b ESC[K`. The engine recounted the row's length from its cells'
            // bits and dropped the space the mark rides on, so the frames
            // showed it plain until `c`, written after it, grew the row back
            // over it: the frame `c` was shown in lacked the mark the screen
            // settled with. aterm-grid keeps a marked blank in the row
            // (`Grid::keep_len_over_marks`); no rule of the predictor is
            // involved (this round's sweep, seed index 778, case 211).
            name: "a_mark_on_the_prompt_s_space_survives_an_erase_after_it",
            cfg: Config {
                remote: false,
                ..adaptive_on(Editor::Readline)
            },
            ops: vec![
                Op::Key(79, 1),
                Op::Program(1, 0),
                Op::Advance(60),
                Op::Deliver(1024),
                Op::Advance(300),
                Op::Key(0, 13),
                Op::Program(1, 0),
                Op::Advance(60),
                Op::Deliver(1024),
                Op::Advance(300),
                Op::Key(42, 1_692_525_985),
                Op::Program(1, 1_873_842_150),
                Op::Advance(83),
                Op::Deliver(1024),
                Op::Key(74, 1_136_843_215),
                Op::Advance(85),
                Op::Program(1, 3_115_955_677),
                Op::Deliver(1024),
                Op::Key(4, 3_672_021_138),
            ],
            mutants: vec![],
        },
        NamedCase {
            // The world's own: a `^C` at a wrapped line drew the next prompt and
            // its right prompt on the row the line had wrapped onto, over its
            // text, and the erase that hid the right prompt when the typed line
            // reached it took that text with it, the very frame before a
            // correct `o` ghost settling otherwise. zsh clears the row it draws
            // a prompt on (PROMPT_SP), as the world does now; no rule of the
            // predictor is involved (seed index 1202, case 363).
            name: "a_prompt_drawn_over_a_wrapped_line_clears_its_row",
            cfg: Config {
                remote: false,
                wide: true,
                rprompt: true,
                keymap: Keymap::ZshVanilla,
                ..adaptive_on(Editor::Overwrite)
            },
            ops: vec![
                Op::Key(3_967_001_565, 1_090_444_910),
                Op::Key(4_173_506_031, 174_878_242),
                Op::Unprompted(Unprompted::Notice, 338_254_974),
                Op::HostReset(true),
                Op::Key(1_376_885_262, 2_714_040_639),
                Op::Key(3_764_850_899, 3_043_727_581),
                Op::HostReset(true),
                Op::Key(2_546_372_397, 733_172_417),
                Op::Key(3_415_502_628, 1_496_678_391),
                Op::Program(2, 2_035_971_394),
                Op::Key(2_054_964_825, 2_984_444_042),
                Op::Program(2, 3_901_573_507),
                Op::Key(3_585_466_768, 1_198_443_616),
                Op::Key(4_293_498_559, 3_257_385_715),
                Op::Program(3, 2_566_340_245),
                Op::Key(3_921_073_541, 2_388_012_768),
                Op::Program(1, 3_534_683_016),
                Op::Key(2_251_324_907, 3_515_928_375),
                Op::Program(1, 2_164_549_709),
                Op::Key(2_401_257_690, 2_957_572_945),
                Op::Key(2_728_527_065, 1_620_484_634),
                Op::Key(3_621_244_750, 529_522_794),
                Op::Key(1_008_440_008, 3_725_811_222),
                Op::Key(419_868_030, 2_832_329_680),
                Op::Key(1_303_058_106, 2_073_444_543),
                Op::Key(2_821_364_298, 1_232_447_801),
                Op::Key(394_330_654, 3_335_503_539),
                Op::Program(1, 2_304_745_812),
                Op::Key(2_688_292_427, 63_189_474),
                Op::Key(2_294_825_334, 1_893_751_162),
                Op::Key(2_915_177_766, 3_635_348_550),
                Op::Program(1, 342_962_009),
                Op::Key(3_486_013_192, 3_869_693_233),
                Op::Program(2, 245_198_765),
                Op::Program(3, 1_882_146_446),
                Op::Program(255, 0),
                Op::Key(87, 5),
                Op::Program(1, 0),
                Op::Key(79, 1),
                Op::Program(1, 0),
                Op::Deliver(1024),
                Op::Advance(581),
                Op::Key(0, 1),
                Op::Program(1, 0),
                Op::Key(0, 6),
                Op::Program(1, 0),
                Op::Key(0, 6),
                Op::Program(1, 0),
                Op::Key(0, 1),
                Op::Program(1, 0),
                Op::Advance(28),
                Op::Deliver(1024),
                Op::Key(0, 3),
                Op::Program(1, 0),
                Op::Advance(59),
                Op::Deliver(1024),
                Op::Key(0, 7),
            ],
            mutants: vec![],
        },
        NamedCase {
            // readline in overwrite mode under `zsh -f`'s keymap (Delete reads
            // as a `~`, inserted): the buffer edited until the cursor stands on
            // its own `d` in the last column, moved there by `→`. `i` typed
            // there: the last reconcile read a filled last column under the
            // cursor, the terminal's deferred-wrap park by position, and put
            // `i` on the next row, while the terminal had wrapped nothing and
            // overwrite mode wrote `i` over that `d`; each guess after it was a
            // cell right of its echo. The engine's own flag says the cursor is
            // not parked (the review of increment 4's sweep, seed index 1841,
            // case 453, reachable once the world's keymaps came in).
            name: "a_cursor_on_text_in_the_last_column_is_not_parked",
            cfg: Config {
                remote: false,
                forced_wrap: true,
                keymap: Keymap::ZshVanilla,
                ..adaptive_on(Editor::Overwrite)
            },
            ops: vec![
                Op::Key(79, 1),
                Op::Program(1, 6),
                Op::Deliver(1024),
                Op::Advance(608),
                Op::Key(0, 3),
                Op::Program(1, 0),
                Op::Deliver(1024),
                Op::Key(0, 5),
                Op::Key(0, 19),
                Op::Key(0, 22),
                Op::Program(1, 0),
                Op::Key(0, 2),
                Op::Program(1, 0),
                Op::Key(0, 7),
                Op::Program(2, 0),
                Op::Key(87, 21),
                Op::Program(2, 0),
                Op::Key(0, 2),
                Op::Program(1, 0),
                Op::Key(0, 7),
                Op::Program(1, 0),
                Op::Key(0, 3),
                Op::Program(1, 0),
                Op::Key(87, 1),
                Op::Program(1, 0),
                Op::Key(87, 1),
                Op::Program(1, 0),
                Op::Key(70, 1),
                Op::Program(1, 0),
                Op::Key(70, 1),
                Op::Program(1, 0),
                Op::Key(70, 1),
                Op::Program(1, 0),
                Op::Key(87, 2),
                Op::Key(87, 2),
                Op::Advance(65),
                Op::Program(2, 0),
                Op::Deliver(1024),
                Op::Advance(53),
                Op::Key(28, 2_962_266_151),
                Op::Advance(57),
                Op::Program(1, 3_971_393_991),
                Op::Key(0, 5),
                Op::Program(1, 0),
                Op::Advance(81),
                Op::Deliver(1024),
                Op::Key(0, 5),
                Op::Program(1, 0),
                Op::Key(0, 5),
                Op::Program(1, 0),
                Op::Advance(76),
                Op::Deliver(1024),
                Op::Key(0, 6),
            ],
            mutants: vec![Host::Rule(Rule::EngineWrapFlag)],
        },
        NamedCase {
            // A suggestion, `ab`, drawn (dim, italic) at an empty line, in
            // pieces: its `a` with the cursor past it, then the rest and the
            // step back. `a` typed meanwhile is credited with the suggestion's
            // `a` (a credit inside output nobody typed for, out of
            // ATTRIBUTION's scope), and once the cursor stepped back onto it the
            // `a` typed next, anchored there, was credited with the first `a`'s
            // real echo. An echo never steps back over itself: the cursor found
            // on the credited cell takes the credit back, owing that key's echo
            // (the review of increment 4's sweep, seed index 784, case 367).
            name: "a_suggestion_drawn_part_way_is_no_echo",
            cfg: Config {
                remote: false,
                forced_wrap: true,
                rprompt: true,
                integration: true,
                keymap: Keymap::ViIns,
                ..adaptive_on(Editor::Overwrite)
            },
            ops: vec![
                Op::Unprompted(Unprompted::Suggest, 1_245_248_047),
                Op::Key(4_292_485_864, 1_581_098_321),
                Op::Deliver(456),
                Op::Deliver(1024),
                Op::Key(3_486_505_620, 2_765_945_526),
                Op::Program(1, 3_172_092_415),
                Op::Deliver(202),
            ],
            mutants: vec![Host::Rule(Rule::CreditTakenBack)],
        },
        NamedCase {
            // zle with an autosuggestion plugin: `n`, `o`, `a` typed ahead;
            // `n` and `o` echoed, and the suggestion `ab` the `o` brought up
            // drawn after them in pieces, its `a` (the typed one) with the
            // cursor past it, crediting the pending `a`; then the step back
            // onto it. The `a` typed next, anchored there, was credited with
            // the first `a`'s real echo (the review of increment 4's sweep,
            // seed index 2115, case 383).
            name: "a_suggestion_fetched_part_way_is_no_echo",
            cfg: Config {
                remote: false,
                forced_wrap: true,
                rprompt: true,
                integration: true,
                keymap: Keymap::ZshVanilla,
                ..adaptive_on(Editor::Autosuggest)
            },
            ops: vec![
                Op::Key(21, 2_606_560_628),
                Op::Key(40, 2_330_836_285),
                Op::Program(1, 3_550_762_453),
                Op::Key(50, 3_580_536_366),
                Op::Program(1, 1_883_934_739),
                Op::Deliver(546),
                Op::Deliver(1024),
                Op::Key(34, 8_228_236),
                Op::Program(1, 16),
                Op::Deliver(1024),
            ],
            mutants: vec![Host::Rule(Rule::CreditTakenBack)],
        },
        NamedCase {
            // `so` typed, echoed and proving, `o` rubbed out (its erase echoed);
            // `a` typed and echoed, then ` Password: ` printed after it on the
            // same row (only its last space wraps, a blank under the line) and
            // the cursor moved back onto `a`, in one burst. `a` shows its glyph
            // with the cursor on it, as a guess typed onto a suggestion does, and
            // waits; the digit typed at the password read was shown after it.
            // Nothing typed after a guess held so shows until it retires. (The
            // blank wrapped under the line is no text: the floor holds.)
            name: "nothing_shows_behind_a_guess_the_cursor_was_brought_back_onto",
            cfg: Config {
                integration: true,
                ..adaptive_remote()
            },
            ops: [&typed("so")[..], &echoed(BACKSPACE), &[glyph('a')]]
                .concat()
                .into_iter()
                .chain([
                    Op::Program(1, 0),
                    Op::Unprompted(Unprompted::PasswordSameRow, 0),
                    // The cursor to the first row's second column: `a`'s cell.
                    Op::Unprompted(Unprompted::Jump, 7),
                    Op::Advance(60),
                    Op::Deliver(1024),
                    Op::Key(0, 1),
                    Op::Frame,
                ])
                .collect(),
            mutants: vec![Host::Rule(Rule::DarkBehindAWait)],
        },
        NamedCase {
            // `s`, `a`, `d` typed; `s` echoed and proving; then `a` and `d`
            // echoed, ` Password: ` printed after them on the same row, which
            // wraps its `: ` onto the next, and a reverse index back up, in one
            // burst: the cursor stands on `d`, which shows its glyph, so `d` waits
            // there as a guess typed onto a suggestion does, and `7`, typed at
            // the password read, was shown after it. The row under the line,
            // blank at the run's press, shows the wrapped `:`, which voids the
            // proof, and `d` held with the cursor brought back onto it holds `7`
            // dark: each rule closes it alone, so neither switched off breaks it,
            // and each has its own case as its control (seed index 1909, case
            // 109).
            name: "a_prompt_wrapped_under_the_line_voids_its_proof",
            cfg: Config {
                forced_wrap: true,
                rprompt: true,
                integration: true,
                ..adaptive_remote()
            },
            ops: vec![
                Op::Key(35, 1_105_504_744),
                Op::Program(1, 812_636_238),
                Op::Key(9, 3_010_783_216),
                Op::Key(42, 3_218_459_163),
                Op::Advance(45),
                Op::Deliver(1024),
                Op::Program(3, 3_813_857_926),
                Op::Unprompted(Unprompted::PasswordSameRow, 1_176_107_598),
                Op::Unprompted(Unprompted::ReverseIndex, 3_658_062_845),
                Op::Deliver(1024),
                Op::Key(882_776_852, 2_321_013_047),
            ],
            mutants: vec![],
        },
        NamedCase {
            // A cooked tty: ` sie` echoed and proving, then `\r\nPassword: `
            // printed on the next row and a reverse index back up, in the same
            // burst, onto the cell after `e`, where the proof stands. Every
            // cell of the line holds its glyph and the cursor is where the
            // proof says; `6`, typed at the password read, was shown there.
            // The row under the line, blank at the run's press, now shows
            // text: the proof is void (seed index 1206, case 434).
            name: "text_drawn_under_the_line_voids_its_proof",
            cfg: Config {
                editor: Editor::Cooked,
                integration: true,
                ..adaptive_remote()
            },
            ops: vec![
                Op::Key(87, 1),
                Op::Program(1, 1_954_412_082),
                Op::Key(0, 9),
                Op::Key(0, 2),
                Op::Key(0, 19),
                Op::Key(0, 6),
                Op::Program(1, 0),
                Op::Key(0, 22),
                Op::Program(1, 0),
                Op::Key(87, 1),
                Op::Key(87, 1),
                Op::Program(5, 0),
                Op::Advance(190),
                Op::Deliver(1024),
                Op::Advance(324),
                Op::Key(24, 2_830_178_288),
                Op::Key(54, 4_022_293_372),
                Op::Key(38, 1_824_765_798),
                Op::Advance(171),
                Op::Key(40, 1_600_981_608),
                Op::Program(1, 3_198_174_032),
                Op::Deliver(585),
                Op::Key(3_239_558_317, 3_554_669_388),
                Op::Program(1, 3_193_751_317),
                Op::Program(3, 755_110_609),
                Op::Unprompted(Unprompted::PasswordNewRow, 28_625_636),
                Op::Unprompted(Unprompted::ReverseIndex, 3_718_338_279),
                Op::Deliver(1024),
                Op::Key(1_130_100_006, 1_848_623_586),
            ],
            mutants: vec![Host::Rule(Rule::FloorBelow)],
        },
    ]
}

/// The breaks the review of increment 5 found sweeping past seed index 2400
/// (seed index `i` is the committed seed xor `i` times the golden-ratio
/// constant), each on main before the increment and none of its making, each
/// a named case with the rule that closes it switched off as its control.
fn review_5_cases() -> Vec<NamedCase> {
    vec![
        NamedCase {
            // A wide prompt (`$$$ `), `oos` typed and echoed on it, and Home,
            // End, `←` and Delete leaving `oo` with the cursor at its end, the
            // motion class proven; then `←` pressed and not yet read. An async
            // prompt segment resolves and zsh's `reset-prompt` redraws the
            // prompt narrower, `\r$ oo\e[K`, with no new `B`. The line's text
            // now stands left of `lo`, its old cells read blank, and the redraw
            // leaves the cursor on `lo`: no state matches, and the transit wait
            // took every blank for an echo part-way, kept the chain and painted
            // `oo` over the blanks, `$ oooo` on glass while the screen settled
            // to `$ oo`. No echo of `←` writes a blank, and
            // no echo of the chain's keys writes left of where its first insert
            // or erase writes: the blanks are unexplained, and the chain is
            // flushed (the review of increment 5's sweep, seed index 2482,
            // case 215).
            name: "a_narrower_redraw_is_no_echo_in_transit",
            cfg: Config {
                forced_wrap: true,
                editor: Editor::Readline,
                bidi: true,
                integration: true,
                keymap: Keymap::ViIns,
                ..adaptive_remote()
            },
            ops: vec![
                Op::Unprompted(Unprompted::ResetPrompt, 2_607_512_461),
                Op::Key(87, 5),
                Op::Program(1, 0),
                Op::Deliver(1024),
                Op::Key(79, 1),
                Op::Program(1, 8),
                Op::Deliver(1024),
                Op::Advance(549),
                Op::Key(0, 7),
                Op::Key(0, 7),
                Op::Program(1, 0),
                Op::Deliver(1024),
                Op::Key(0, 19),
                Op::Program(1, 0),
                Op::Key(87, 6),
                Op::Program(1, 0),
                Op::Program(1, 0),
                Op::Advance(70),
                Op::Deliver(1024),
                Op::Key(87, 7),
                Op::Program(1, 0),
                Op::Key(87, 1),
                Op::Program(1, 0),
                Op::Key(87, 21),
                Op::Program(1, 0),
                Op::Advance(66),
                Op::Deliver(1024),
                Op::Key(87, 1),
                Op::Unprompted(Unprompted::ResetPrompt, 1),
                Op::Deliver(1024),
            ],
            mutants: vec![Host::Rule(Rule::TransitBlanks)],
        },
        NamedCase {
            // On a witnessed line `ed` with the cursor on `lo`, Delete and End
            // pressed and read together; Delete's echo, `d`, ` `, `\b\b`,
            // split after its first `\b`: the cursor one cell right of `lo`,
            // which is End's state (`d`, the cursor at its end). Delete and End
            // were retired there, End credited with no output of its own; then
            // Home pressed, and the rest of Delete's step back put the cursor
            // on `lo`, Home's state, and Home was credited too, each credit a
            // sample of nothing and an echo of a binding that never came. A
            // cursor anywhere on a Delete's step back is that echo part-way
            // (the review of increment 5's sweep, seed index 2748, case 172).
            name: "a_delete_s_step_back_is_no_later_state",
            cfg: Config {
                forced_wrap: true,
                rprompt: true,
                editor: Editor::Overwrite,
                integration: true,
                ..adaptive_remote()
            },
            ops: vec![
                Op::Key(87, 5),
                Op::Program(1, 0),
                Op::Deliver(1024),
                Op::Key(79, 1),
                Op::Program(1, 6),
                Op::Deliver(1024),
                Op::Advance(681),
                Op::Key(0, 7),
                Op::Program(1, 0),
                Op::Key(0, 19),
                Op::Program(1, 0),
                Op::Key(0, 15),
                Op::Program(1, 0),
                Op::Key(0, 3),
                Op::Program(1, 0),
                Op::Deliver(1024),
                Op::Key(0, 7),
                Op::Program(1, 0),
                Op::Key(87, 6),
                Op::Program(1, 0),
                Op::Key(87, 21),
                Op::Program(1, 0),
                Op::Key(87, 21),
                Op::Program(1, 0),
                Op::Key(87, 7),
                Op::Program(1, 0),
                Op::Key(87, 1),
                Op::Program(1, 0),
                Op::Key(87, 21),
                Op::Program(1, 0),
                Op::Key(87, 6),
                Op::Program(1, 0),
                Op::Deliver(1024),
                Op::Key(87, 21),
                Op::Key(87, 7),
                Op::Program(2, 0),
                Op::Deliver(797),
                Op::Key(87, 6),
                Op::Deliver(518),
            ],
            mutants: vec![Host::Rule(Rule::PartWayStepBack)],
        },
        NamedCase {
            // Over ssh (no integration): `d` typed and echoed; `←` and Delete
            // pressed (Delete not modelled there: owed, the epoch ended) and
            // `o` guessed on the live cursor, a stale anchor. The far end's
            // `\b`, ` `, `\b`, `o` put `o` a cell left of its guess, which
            // stayed pending; a Backspace cancelled it, and `a`, `a`, `c` were
            // typed on its cell as a fresh run. The Backspace's echo returned
            // the cursor a cell left of that cell, and the second `a`'s echo
            // credited the first guess, so `a` and `c` were shown a cell right
            // of where they echoed. A cancelled stale guess's cell is a stale
            // anchor (the review of increment 5's sweep, seed index 3942, case
            // 141).
            name: "a_cancelled_stale_guess_anchors_a_stale_run",
            cfg: Config {
                forced_wrap: true,
                rprompt: true,
                editor: Editor::Zle,
                keymap: Keymap::ViIns,
                ..adaptive_remote()
            },
            ops: vec![
                Op::Key(0, 3),
                Op::Program(1, 0),
                Op::Advance(26),
                Op::Deliver(1024),
                Op::Key(87, 1),
                Op::Key(87, 21),
                Op::Advance(62),
                Op::Key(0, 7),
                Op::Program(3, 0),
                Op::Deliver(1024),
                Op::Advance(131),
                Op::Advance(65),
                Op::Key(76, 487_532_433),
                Op::Key(59, 265_158_879),
                Op::Key(12, 3_548_356_616),
                Op::Key(62, 2_173_896_862),
                Op::Program(1, 975_754_589),
                Op::Program(1, 1_787_353_441),
                Op::Program(1, 2_094_684_551),
                Op::Advance(79),
                Op::Deliver(1024),
            ],
            mutants: vec![Host::Rule(Rule::StaleCancel)],
        },
    ]
}

/// The breaks the sweep over many seeds found (`seed_index`, the family the
/// committed sweep and the review's draw from), each a named case with the
/// rule that closes it switched off as its control.
fn sweep_cases() -> Vec<NamedCase> {
    vec![NamedCase {
        // `i` typed and echoed; Home (owed: no integration) and its echo, the
        // far end's cursor back on `i`; `i` typed on it. Readline inserts it
        // before that `i`, writing the glyph, then the tail it shifts, then the
        // step back (`i`, `i\b`), and the read split after the glyph: the
        // frame shows `i` with the cursor past it and the row right of it
        // blank, as the press saw it, so the echo retired as an append,
        // uncredited while Home's echo was owed, with no proof to watch the
        // row. `a`, typed on the cursor it left, read a blank row and took
        // itself to be at the end of the line; the redraw and `a`'s echo
        // (`ai\b`) landed in one read, `iai`, and `a` proved beside the `i`
        // the redraw put right of it. `d` typed next was shown over that `i`:
        // `iad` on glass, `iadi` settled. The `i` was drawn over the glyph it
        // shifted (a HIDDEN TAIL), so for its window a blank row is no line
        // end (seed index 97, case 27, shrunk; a break on main since the
        // review of phase 0, 0376b47967).
        name: "an_echo_drawn_over_the_glyph_it_shifts_hides_its_tail",
        cfg: Config {
            editor: Editor::Readline,
            ..adaptive_remote()
        },
        ops: vec![
            Op::Key(0, 5),
            Op::Program(1, 0),
            Op::Advance(30),
            Op::Deliver(1024),
            Op::Key(87, 6),
            Op::Program(1, 0),
            Op::Advance(71),
            Op::Deliver(1024),
            Op::Advance(57),
            Op::Key(0, 5),
            Op::Program(1, 0),
            Op::Deliver(675),
            Op::Advance(138),
            Op::Key(0, 22),
            Op::Program(1, 0),
            Op::Advance(51),
            Op::Deliver(1024),
            Op::Key(0, 3),
        ],
        mutants: vec![Host::Rule(Rule::CleanTailInFlight)],
    }]
}
