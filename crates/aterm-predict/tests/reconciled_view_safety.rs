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
//! `RenderInput::paint_prediction_ghost`), so `D` is what goes on glass, every
//! field of every cell, the grapheme clusters and combining marks included; the
//! GUI also draws its cursor one past the newest guess, which changes no cell.
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
//!   host can get wrong.) A STATE CHAIN (the edit lane: `←`, `→` and an erase
//!   at the end of the line, armed on the line its epoch watched echo) is held
//!   to an ORACLE instead (`Sim::oracle_check`): from the screen `A` showed
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
//!   far end draws as decoration (here a right prompt, drawn in italics) may be
//!   stale on glass, which is counted (`stale_decoration`); a ghost glyph that
//!   the settled screen contradicts is a violation, decoration or not.
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
//!   is a frame no program draws (`chain_paint_clauses`).
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
//! keys still in flight), a far-end program (a line editor in the byte forms
//! measured from it (`Editor`): readline, which inserts mid-line by writing the
//! glyph, then the tail it shifts, then stepping back, and beeps at an edge;
//! readline with ICH, which reprints the glyph `→` steps over; zle, silent at
//! an edge; and a cooked tty, which echoes `←` as `^[[D`; each rotated per case;
//! a command that starts `cat`, cooked, until ^C; the line editor now and then
//! draws an autosuggestion after an append's
//! echo, erases back across the right margin and up a wrapped row, either
//! leaves the terminal parked in the deferred-wrap slot or wraps itself, and in
//! half the cases draws a right prompt with each prompt and hides it before the
//! line reaches it, as zsh does; a password mode that does not echo; a
//! full-screen app on the alternate screen) whose output reaches the engine in
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
//! as aterm-gui paints it (`paint_chain`): all or none, its cursor drawn only
//! when every cell was painted. Every configuration runs under both
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
//! predictor Alt-B, or the keypad's Left under DECKPAM, as a bare `←`, and one
//! that draws a chain's cursor one past its newest glyph (FRAME FIDELITY); and a
//! predictor with one of its rules switched off (`Host::Rule`, each in the
//! clause it keeps, which is how ATTRIBUTION has its controls). ATOMICITY and
//! CURSOR LOCALITY are held on a frame the world cannot reach (a chain flushes on
//! any opaque cell of its line), with a host painting what it can and one drawing
//! a chain's cursor on a wide glyph's right half (`the_chain_painter_paints_all_or_none`).
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
//! Each such rule of the predictor is a [`Rule`] the debug-only
//! `Predictor::disable_rule_for_test` seam switches off, run as the mutant host
//! `Host::Rule`, and its negative control is caught in its clause by the sweep
//! (`MUTANT_HOSTS`) or at a NAMED CASE (`each_named_case_holds_and_catches_its_mutant`),
//! a directed interleaving the faithful predictor holds; usually both, and
//! `every_rule_has_a_negative_control` holds each rule to at least one.

#[path = "common/world.rs"]
mod world;

use aterm_predict::{PredictMode, Rule};
use world::*;

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
    // Non-vacuity: the world reached every clause it claims to check.
    let all_cases = (CASES * faithful_configs().len() / 2) as u64;
    assert_eq!(adaptive.settled_cases, all_cases, "{adaptive:?}");
    assert_eq!(always.settled_cases, all_cases, "{always:?}");
    // The floors sit well under what the committed seed reaches over both
    // ambiguous-width policies (2196 painting frames, 1134 fidelity comparisons,
    // 3312 secrets, 16588 row moves, 297 ghosts the painter refused on a wide
    // glyph in `Always`): a generator change that starves a clause fails here
    // instead of passing vacuously. They count comparisons, not whether a
    // comparison can fail; the mutant hosts below are that half.
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
    // four Adaptive configurations (896 whole frames compared with the screen
    // they settle to, 299 credited echoes refused proof over a moved tail, 21
    // standing proofs voided when the tail moved after them, 9726 expired
    // guesses owed, 9762 typed owes, 4640 turns holding a guess on a glyph
    // shown before its echo, 4841 credits checked against their key's output, 96 frames
    // painting beside a right prompt, 199 proofs over decoration the echo kept,
    // 106 stale decoration cells counted), and every ghost on glass in either
    // mode checked against the window (10990 Adaptive, 573302 Always).
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
    // the committed seed reaches over the four Adaptive configurations: 670 edit
    // keys armed, 404 frames displaying a chain, every one held to the
    // PROVENANCE oracle, 104 of them showing a motion, 96 motions that proved
    // their class, 142 arrows armed over a cooked tty and never shown, 8 net-null
    // tails, 34 part-way echoes waited on, 28112 arrows declined with their
    // pixels kept, and 4 frames of a chain carried across its terminator (the
    // named case `enter_with_keys_in_flight` holds that one to its control).
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
        adaptive.net_null_retired > 2 && adaptive.transit_waits > 10,
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
            // `ab cd` echoed on a slow link, three Lefts onto the space, a pause past
            // the window, then `e` and `i`. `e`'s echo inserts it and shifts ` cd`
            // right; it is `e`'s own (nothing owed in the window) and the cursor
            // advanced, so positional evidence credited it. Proving the epoch, it
            // put `i` on glass over the shifted space: the frame read `abeicd`, the
            // screen settled to `abei cd`. FIDELITY's one cell was right (`i` echoes
            // where it was painted); only the whole frame was wrong. The tail guard
            // sees the row right of `e` changed since the press, and proves nothing.
            // The pause outlasts the window an expiry may have widened: the Lefts
            // are a state chain now, which a host blind to the row right of the
            // cursor never sees match, so its chain expires and owes them.
            name: "mid_line_over_a_space",
            cfg: adaptive_remote(),
            ops: [
                &echoed(glyph('a'))[..],
                &echoed(glyph('b')),
                &echoed(glyph(' ')),
                &echoed(glyph('c')),
                &echoed(glyph('d')),
                &echoed(LEFT),
                &echoed(LEFT),
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
                &echoed(LEFT),
                &echoed(LEFT),
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
    .collect()
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
            // The keypad's Left under DECKPAM sends `ESC O t`, which readline
            // leaves unbound: it beeps and the cursor stays. Handed to the
            // predictor as `←`, the frame moved the cursor.
            name: "a_keypad_arrow_under_deckpam_is_not_a_motion",
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

/// A host paints a state chain ALL OR NONE, and draws its cursor only on a cell
/// that is no wide glyph's right half. The world cannot reach the frames that
/// tell (a chain flushes on any opaque cell of its line, and its cells and
/// cursor are inside it), so the painter is held here on one that does: a row
/// holding a wide glyph, a chain whose second cell, or whose cursor, falls on
/// its right half. aterm's host paints none and draws the engine's cursor; a
/// host painting what it can breaks ATOMICITY, and one drawing the cursor
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
    let motion: [(u16, u16, char); 0] = [];
    for (shown, cursor, mutant) in [
        (&cells[..], (0, 4), Host::PaintsPartialState),
        (&motion[..], (0, 3), Host::CursorOnAContinuation),
    ] {
        for host in [Host::Faithful, mutant] {
            let mut d = frame.clone();
            let (painted, drawn) = paint_chain(&mut d, shown, Some(cursor), (12, blank), host);
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
