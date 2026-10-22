// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Safety theorem for predictive local echo: predictions never corrupt the
//! reconciled view across any interleaving of typing, program output, and time.
//!
//! ## Views and coordinate space
//!
//! * **Authoritative view (`A`)**: `aterm-core`'s `Terminal` fed only program
//!   output and read through `cell_frame_into` / `visual_col`.
//! * **Overlay (`O`)**: `Predictor::overlay(now)` after `reconcile` against `A`.
//! * **Displayed view (`D`)**: `A` with `O` painted via
//!   `RenderInput::paint_prediction_ghost` or `paint_prediction_span`.
//! * **Coordinate space**: `(coordinate_generation, screen_epoch)` on the main
//!   screen at the armed geometry.
//!
//! ## Clauses
//!
//! * **Locality**: `D` differs from `A` only at `O`'s cells, never on half of a
//!   wide glyph; every guess in `O` is in bounds and on a distinct cell.
//! * **Provenance**: Every guess in `O` matches the keystroke and coordinate
//!   space it was armed in. State chains match an independent key fold
//!   (`Sim::oracle_check`) over the last matched authoritative screen.
//! * **Secrecy (Adaptive)**: Keys typed into a visible no-echo prompt are never
//!   in `O`.
//! * **Fidelity (Adaptive)**: Without intervening unprompted output, a space
//!   change, or a predecessor whose output arrived past the predictor's glitch
//!   window (`Sim::late_predecessor`), a displayed guess's echo lands on the
//!   cell it was painted on.
//! * **Frame fidelity (Adaptive)**: On a settled base within fidelity's scope,
//!   every displayed frame matches the screen its newest painted guess's output
//!   settles to (`Sim::settled`), except stale italic decoration cells and
//!   vacated blanks under newly drawn decoration.
//! * **Timeliness**: Every ghost on glass is still pending and younger than the
//!   predictor's glitch window (at most 2 s).
//! * **Attribution**: Every guess credited with its echo has had its own output
//!   begin (`Sim::attributed`), within the link model's scope and on a boundary
//!   between keys' output.
//! * **Atomicity**: A state chain paints all of its displayed cells or none.
//! * **Cursor locality**: A state chain's drawn cursor is in bounds and never
//!   on a wide glyph's continuation cell.
//! * **Eventual**: Once all keys are read, all output is delivered, and the
//!   clock passes the 2 s ceiling, `O` is empty and `D = A`.
//!
//! ## Simulation and negative controls
//!
//! `common/world.rs` drives `Sim` with deterministic `XorShift` op streams and
//! shrinks any failure by delta debugging. Every clause and predictor [`Rule`]
//! is guarded by mutant negative controls in [`MUTANT_HOSTS`] and [`named_cases`].

use aterm_predict::{GhostCell, PredictMode, Rule};

use crate::world::*;

/// Checks the safety theorem across all shipped configurations.
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
    eprintln!(
        "coverage, Adaptive, {} configurations: {adaptive:?}\ncoverage, Always, {} \
         configurations: {always:?}",
        configs.len() / 2,
        configs.len() / 2
    );
    // Non-vacuity floors across clauses and lanes.
    let all_cases = (CASES * faithful_configs().len() / 2) as u64;
    assert_eq!(adaptive.settled_cases, all_cases, "{adaptive:?}");
    assert_eq!(always.settled_cases, all_cases, "{always:?}");
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
    assert!(
        always.text_waits > 5000,
        "guesses waiting over the line's text, held off the glass: {always:?}"
    );
    assert!(
        adaptive.rprompt_frames > 20 && adaptive.decorated_proofs > 50,
        "lines with a right prompt that proved and painted: {adaptive:?}"
    );
    assert!(
        adaptive.stale_decoration > 10,
        "cells of decoration shown stale and counted, not failed: {adaptive:?}"
    );
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

/// Additional seed indices swept alongside [`SEED`] (index 0).
const SWEPT_SEED_INDICES: std::ops::RangeInclusive<u64> = 1..=2;

/// Checks the safety theorem over [`SWEPT_SEED_INDICES`].
#[test]
fn a_prediction_never_corrupts_the_reconciled_view_over_more_seeds() {
    let failures = sweep_seed_indices(SWEPT_SEED_INDICES);
    assert!(failures.is_empty(), "{}", failures.join("\n=====\n"));
}

/// On-demand 1000-seed sweep across all faithful configurations.
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

/// Verifies that every mutant in [`MUTANT_HOSTS`] fails its declared [`Host::breaks`] clause.
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

/// Directed fidelity regressions from early interleaving reviews.
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

/// Directed fidelity interleavings checked by [`the_reviewed_interleavings_hold`].
fn reviewed_interleavings() -> [(&'static str, Vec<Op>); 3] {
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
            // Guess retired without credit by an earlier key's echo still owes its own echo.
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
            // Pane swap away and back preserves the owed echo.
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
            // Key refused at the no-echo gate still owes its echo.
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

/// Directed interleaving that holds under [`Host::Faithful`] and breaks each mutant's clause.
struct NamedCase {
    name: &'static str,
    cfg: Config,
    ops: Vec<Op>,
    /// Mutants that must fail [`Host::breaks`] on this case.
    mutants: Vec<Host>,
}

/// Adaptive, remote, narrow configuration.
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

/// Press `key`, read it, and deliver its echo over a 60 ms link.
fn echoed(key: Op) -> [Op; 4] {
    [key, Op::Program(1, 0), Op::Advance(60), Op::Deliver(1024)]
}

fn named_cases() -> Vec<NamedCase> {
    vec![
        NamedCase {
            // Expired guess owes its late echo inside the predictor's glitch window.
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
            // Mid-line insert shifting a tail right of the cursor does not prove the epoch.
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
            // Uncredited retirement owes only its own glyph.
            name: "a_guess_retired_without_credit_owes_its_own_glyph",
            cfg: adaptive_remote(),
            ops: reviewed_interleavings()[0].1.clone(),
            mutants: vec![Host::Rule(Rule::TypedOwesSameGlyph)],
        },
        NamedCase {
            // Cancelled glyph owes its echo so it cannot credit a subsequent same-glyph guess.
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
            // Cancelled glyph at the right margin does not wrap the next guess.
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
            // Mid-line glyph echoed before its shifted tail redraws voids the standing proof.
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
            // Guesses typed over an autosuggestion require cursor advance before retirement.
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
                        Op::Unprompted(Unprompted::Suggest, 3),
                        Op::Deliver(1024),
                        Op::Key(0, 36),
                        Op::Key(0, 16),
                        Op::Key(0, 24),
                        Op::Key(0, 4),
                        Op::Frame,
                        Op::Program(4, 0),
                        Op::Advance(300),
                        Op::Deliver(1024),
                        Op::Advance(400),
                        Op::Key(0, 6),
                        Op::Unprompted(Unprompted::StatusLine, 0),
                        Op::Advance(10),
                        Op::Deliver(755),
                        Op::Deliver(1024),
                        Op::Advance(100),
                        Op::Key(0, 28),
                        Op::Key(0, 118),
                        Op::Key(0, 2),
                        Op::Advance(190),
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
            // Uncredited retirement voids the epoch proof on its cell.
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
            // Cancelled glyph's cell anchors the next guess while its erase is unread.
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

/// Directed cases for mid-line insert and erase shifts.
fn shift_cases() -> Vec<NamedCase> {
    vec![
        NamedCase {
            // Mid-line insert stays dark until the insert shift class is proven.
            name: "overwrite_mode_never_shows_a_shift",
            cfg: adaptive_on(Editor::Overwrite),
            ops: [&typed("ab")[..], &echoed(LEFT), &echoed(glyph('o'))].concat(),
            mutants: vec![Host::Rule(Rule::InsertShiftProof)],
        },
        NamedCase {
            // Mid-line rubout in overwrite mode never proves the erase shift class.
            name: "overwrite_rubout_never_shows_a_shift",
            cfg: adaptive_on(Editor::Overwrite),
            ops: [&typed("abc")[..], &echoed(LEFT), &echoed(BACKSPACE)].concat(),
            mutants: vec![Host::Rule(Rule::EraseShiftProof)],
        },
        NamedCase {
            // Shifting a tail of all spaces cannot distinguish insert from overwrite.
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
            // Mid-line insert paints only the chain's span, not suggestion cells past the line.
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
            // Unplaceable wide glyph mid-line flushes the active state chain.
            name: "a_wide_glyph_mid_line_retires_the_tail",
            cfg: adaptive_on(Editor::Readline),
            ops: [
                &typed("abc")[..],
                &echoed(LEFT),
                &echoed(LEFT),
                &echoed(glyph('o')),
                &[
                    LEFT,
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
            // Right-to-left glyphs that trigger BiDi reordering are declined.
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
            // Shift indistinguishable from an in-place overwrite beside a suggestion proves nothing.
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
            // Multi-op burst matching an in-place overwrite screen proves neither shift class.
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
            // Screen reachable by fewer keys in overwrite mode credits no chain ops.
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
            // Backspace does not cancel a prior glyph from an unproven mid-line shift.
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

/// Local narrow configuration with the given `editor`, `rprompt`, and `forced_wrap`.
fn swept(editor: Editor, rprompt: bool, forced_wrap: bool) -> Config {
    Config {
        remote: false,
        rprompt,
        forced_wrap,
        ..adaptive_on(editor)
    }
}

/// Directed cases for edit-lane and glyph-lane interactions.
fn edit_review_cases() -> Vec<NamedCase> {
    vec![
        NamedCase {
            // Stale-anchored run is not credited by the echo of a later cancelled key.
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
            // Cursor past every cell the run could reach does not retire the run.
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
            // State chain is credited as a whole or not at all.
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
            // Cancelled glyph's typed owe is dated from the cancelling Backspace.
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
            // Shifted tail reaching the last column is not mistaken for a deferred-wrap park.
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
            // Echo that acquires a combining mark does not prove a plain guess.
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
            // Transit wait only holds when the cursor is within the chain's reachable range.
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
            // Run started inside known text after a retired motion chain proves nothing.
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
            // Proof carried across an epoch boundary still checks its standing tail.
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
            // Edit chain is not credited while a later key's output is owed.
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
            // Space echo alone never proves a line.
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
            // Line proof is voided if the cell that earned it no longer holds its glyph.
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
            // Part-way `\b \b` erase of a cancelled glyph does not credit a typed space.
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
            // Part-way erase echo matching a later chain state does not credit unread keys.
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
            // Vacated blank overwritten by a fetched autosuggestion counts as stale decoration.
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

/// Adaptive remote configuration using `editor`.
fn adaptive_on(editor: Editor) -> Config {
    Config {
        editor,
        ..adaptive_remote()
    }
}

/// Press and echo each character of `text` over a 60 ms link.
fn typed(text: &str) -> Vec<Op> {
    text.chars().flat_map(|c| echoed(glyph(c))).collect()
}

/// Type `ab` and prove the motion class with `←` and `→`.
fn motions_proven_on_ab() -> Vec<Op> {
    [&typed("ab")[..], &echoed(LEFT), &echoed(RIGHT)].concat()
}

/// Press Enter with command `choice`, deliver its output, and wait past the glitch window.
fn enter_and_wait(choice: u32) -> [Op; 5] {
    [
        Op::Key(79, 1),
        Op::Program(1, choice),
        Op::Advance(60),
        Op::Deliver(1024),
        Op::Advance(300),
    ]
}

/// Reach an authenticated fresh prompt with the first-key gate open.
fn at_a_fresh_prompt(choice: u32) -> Vec<Op> {
    [&enter_and_wait(0)[..], &enter_and_wait(choice)].concat()
}

/// Directed cases for state chains on a known line.
fn edit_cases() -> Vec<NamedCase> {
    vec![
        NamedCase {
            // Arrows over a cooked tty never prove the motion class.
            name: "a_cooked_tty_never_shows_a_motion",
            cfg: adaptive_on(Editor::Cooked),
            ops: [&typed("ab")[..], &echoed(LEFT)].concat(),
            mutants: vec![Host::Rule(Rule::MotionClassProof)],
        },
        NamedCase {
            // Motion class proof resets when the epoch ends.
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
            // Left arrow at the start of the known text is declined.
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
            // Chain carried across Enter draws its own cursor rather than the glyph lane's.
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
            // State chain ending in `←` draws the chain's cursor.
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
            // Word motion (Alt-B) is not routed as a single-cell arrow.
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
            // Keypad Left under Kitty DISAMBIGUATE is not routed as an arrow.
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
            // Net-null `←` `→` burst matches the smallest prefix and retires uncredited.
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
            // State chain requires the whole line text to match, not just the cursor.
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
            // Unanswered state chain flushes when its head expires.
            name: "a_chain_no_echo_answers_expires",
            cfg: adaptive_on(Editor::ReadlineIch),
            ops: [&typed("ab")[..], &echoed(LEFT), &[LEFT, Op::Advance(1000)]].concat(),
            mutants: vec![Host::Rule(Rule::StateExpiryFlushes)],
        },
        NamedCase {
            // Run anchored before an unread Backspace lands is marked as a stale anchor.
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

/// Adaptive remote configuration with shell integration enabled.
fn integrated(editor: Editor, keymap: Keymap) -> Config {
    Config {
        integration: true,
        keymap,
        ..adaptive_on(editor)
    }
}

/// Teach `start` and `end` bindings by pressing and echoing each twice.
fn learn_edges(start: Op, end: Op) -> Vec<Op> {
    [
        &echoed(start)[..],
        &echoed(end),
        &echoed(start),
        &echoed(end),
    ]
    .concat()
}

/// Tab key op.
const TAB: Op = Op::Key(87, 13);

/// Directed cases for learned line-edge and Delete bindings.
fn line_edge_cases() -> Vec<NamedCase> {
    vec![
        NamedCase {
            // Unbound Home under `zsh -f` stays a dark probe.
            name: "an_unbound_home_is_not_predicted",
            cfg: integrated(Editor::Zle, Keymap::ZshVanilla),
            ops: [&at_a_fresh_prompt(0)[..], &typed("ab"), &echoed(HOME)].concat(),
            mutants: vec![Host::Rule(Rule::LearnedBindings)],
        },
        NamedCase {
            // Delete inserting `~` under `zsh -f` is not shown as delete-char.
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
            // Ctrl-A inserting `^A` in vi insert mode is not shown as beginning-of-line.
            name: "ctrl_a_in_vi_insert_mode_is_a_glyph",
            cfg: integrated(Editor::Zle, Keymap::ViIns),
            ops: [&at_a_fresh_prompt(0)[..], &typed("ab"), &echoed(CTRL_A)].concat(),
            mutants: vec![Host::Rule(Rule::LearnedBindings)],
        },
        NamedCase {
            // Re-executing the shell with a new nonce resets learned bindings.
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
            // Line started after Tab completion is not witnessed as the buffer start.
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
            // Unsigned OSC 133 marks are ignored when the nonce gate is active.
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
            // Signed `B` mark without `aterm_input=start` on a pre-filled line is not a buffer start.
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
            // Only the first key typed at a fresh prompt witnesses an empty buffer.
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
            // Stale `B` mark left behind by a narrower `reset-prompt` redraw does not witness a line.
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
            // Child program's prompt at the shell's old coordinates has no input region.
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
            // Control chords escaped under Kitty DISAMBIGUATE are not predicted as line edges.
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
            // Control chords escaped under modifyOtherKeys level 2 are not predicted as line edges.
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
            // Learned Delete shifts the tail left and keeps the chain's cursor.
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
            // Line-end key at the end of the text is declined so it can accept an autosuggestion.
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
            // Part-way line-end reprint matching an intermediate cursor does not credit later ops.
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
            // Combining mark carried onto the next guess's cell makes the head opaque and flushes.
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

/// Verifies that `paint_chain` enforces atomicity and cursor locality on wide glyphs.
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

/// Verifies that every [`Rule`] with a safety clause has a negative control in [`MUTANT_HOSTS`] or [`named_cases`].
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

/// Verifies that every [`NamedCase`] holds under [`Host::Faithful`] and catches each of its mutants.
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

/// Directed cases for floor validation, wrap flags, and credit revocation.
fn review_4_cases() -> Vec<NamedCase> {
    vec![
        NamedCase {
            // Unexplained text on the row below a wrapped guess voids the line proof.
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
            // Prompt text overwriting wrapped guess cells on the row below voids the line proof.
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
            // Combining mark on the prompt's trailing space survives an erase after it.
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
            // Prompt drawn after `^C` clears residual wrapped text on its row.
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
            // Cursor moved onto the last column in overwrite mode is not treated as pending wrap.
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
            // Credit is revoked if the cursor steps back onto the credited guess's cell.
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
            // Part-way autosuggestion fetch that steps back revokes the credit it triggered.
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
            // Guesses behind a head waiting on its own glyph with the cursor on it stay dark.
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
            // Password prompt wrapping onto the row below voids the proof and holds guesses dark.
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
            // Text appearing on the previously blank row below a proven line voids its proof.
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

/// Directed cases for transit blanks, Delete step-backs, and stale cancelled anchors.
fn review_5_cases() -> Vec<NamedCase> {
    vec![
        NamedCase {
            // Unexplained blanks left by a narrower prompt redraw flush the chain instead of waiting.
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
            // Cursor caught mid step-back during a Delete echo does not credit later chain ops.
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
            // Cancelling a stale-anchored guess leaves a stale anchor for the next run.
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

/// Directed cases for mid-line echoes hiding a shifted tail.
fn sweep_cases() -> Vec<NamedCase> {
    vec![NamedCase {
        // Echo drawn over the identical glyph it shifts marks the tail as in flight.
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
