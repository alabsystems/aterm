// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE TWINS (plan 5B-3; rulings T5, T29): every hand sequence of
//! `turn_replays.rs` and `transcript_replays.rs` that the `.seq` corpus
//! holds is run both ways, the Rust test (every assert of it) and its
//! `.seq` file, and the two take the same PATH: step for step the same
//! host call, the same model projection after it (the turn model's on the
//! screen rig, the join's on the transcript rig) and the same records
//! appended; and the same final window. Both copies stay: the twin is what
//! lets a later step drop a Rust copy (5B-4, under T5).
//!
//! The path is recorded by the rigs themselves while [`record`] runs: each
//! rig (`conformance_turn::Rig`, `transcript_replays`' and `seq_replay`'s
//! `TxRig`) notes every host call it makes, and hands its path over when
//! it is dropped. Off (every other test), nothing is noted.
//!
//! A host call is named by what it hands the tracker: the screen rig's
//! [`Step`](crate::conformance_turn::Step) (so the model drive's own step,
//! T29), and a transcript rig's verdict, wait, or `TxEvent` with its
//! `TxPos`, the source's state and the time ([`see_step`], [`wait_step`],
//! [`tx_step`]). Time passing with no wake, a state or file moving, and an
//! input noted are no call: their effect is in the next call's name, its
//! projection or its records.
//!
//! NEGATIVE CONTROLS: a ported file with one step altered (a screen
//! twin's half for an elapse, a transcript twin's row read at another
//! time), with its last step dropped, or replayed against another test's
//! twin, each fail; and two paths with nothing recorded on them are no
//! twins.

use std::cell::{Cell, RefCell};
use std::collections::BTreeSet;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};

use aterm_slog::{Cursor, Log, Tier, TxEvent, TxPos, TxState, Verdict, encode};
use aterm_spec::interp::State;

use crate::seq_replay::{Broken, RigKind, Seq, corpus, replay};

/// One host call of a rig: its name, the model state after it, and the
/// records it appended (encoded at the content tier).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Took {
    pub step: String,
    pub state: Option<State>,
    pub lines: Vec<String>,
}

/// A dropped rig's path and its final window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Path {
    pub steps: Vec<Took>,
    pub lines: Vec<String>,
}

thread_local! {
    static ON: Cell<bool> = const { Cell::new(false) };
    static BORN: Cell<usize> = const { Cell::new(0) };
    static DIED: RefCell<Vec<(usize, Path)>> = const { RefCell::new(Vec::new()) };
}

/// Whether paths are being recorded (inside [`record`], on this thread).
pub fn on() -> bool {
    ON.with(Cell::get)
}

/// A new rig's birth order, by which [`record`] lists the paths.
pub fn born() -> usize {
    BORN.with(|b| {
        let n = b.get();
        b.set(n + 1);
        n
    })
}

/// A rig's end: its path and `log`'s window, kept while recording.
pub fn died(born: usize, steps: Vec<Took>, log: &Log) {
    if on() {
        let lines = log.window().map(enc).collect();
        DIED.with(|d| d.borrow_mut().push((born, Path { steps, lines })));
    }
}

fn enc(r: &aterm_slog::Rec) -> String {
    encode(r, Tier::Content).expect("a record encodes")
}

/// The records `log` appended after seq `seq0`, encoded.
pub fn lines_since(log: &Log, seq0: u64) -> Vec<String> {
    let read = log.read(Cursor::new(log.ep(), seq0), usize::MAX);
    read.recs.into_iter().map(enc).collect()
}

/// Run `f` with recording on: the path of every rig it made, in the order
/// the rigs were made. A panic in `f` (a failed assert) is `f`'s.
pub fn record(f: impl FnOnce()) -> Vec<Path> {
    DIED.with(|d| d.borrow_mut().clear());
    ON.with(|o| o.set(true));
    let r = catch_unwind(AssertUnwindSafe(f));
    ON.with(|o| o.set(false));
    let mut died = DIED.with(|d| std::mem::take(&mut *d.borrow_mut()));
    if let Err(p) = r {
        resume_unwind(p);
    }
    died.sort_by_key(|(b, _)| *b);
    died.into_iter().map(|(_, p)| p).collect()
}

/// A transcript rig's verdict call, at test ms `now`.
pub fn see_step(v: Verdict, detail: Option<&str>, now: u64) -> String {
    format!("{now} see {v:?} {detail:?}")
}

/// A transcript rig's wait of `ms` from test ms `now` (the wake fires when
/// due).
pub fn wait_step(ms: u64, now: u64) -> String {
    format!("{now} wait {ms}")
}

/// A transcript rig's row (or bind) at `pos`, the source at `state`.
pub fn tx_step(ev: &TxEvent<'_>, pos: &TxPos, state: TxState, now: u64) -> String {
    format!("{now} tx {ev:?} {pos:?} {state:?}")
}

/// A successor's adoption of the carry line `line`, at test ms `now`.
pub fn adopt_step(line: &str, now: u64) -> String {
    format!("{now} adopt {line}")
}

/// How two paths differ, the first difference named; `whole` compares the
/// final windows too.
pub fn same(seq: &Path, rust: &Path, whole: bool) -> Result<(), String> {
    if seq.steps.is_empty() || rust.steps.is_empty() {
        return Err("a path with no host call on it is no twin (was it recorded?)".to_string());
    }
    for (i, (a, b)) in seq.steps.iter().zip(&rust.steps).enumerate() {
        if a != b {
            return Err(format!("step {i} differs:\n  .seq: {a:?}\n  rust: {b:?}"));
        }
    }
    if seq.steps.len() != rust.steps.len() {
        let (n, m) = (seq.steps.len(), rust.steps.len());
        let more = if n > m {
            &seq.steps[m..]
        } else {
            &rust.steps[n..]
        };
        return Err(format!(
            "the .seq makes {n} host calls, the Rust test {m}; past the shorter: {more:?}"
        ));
    }
    if whole && seq.lines != rust.lines {
        return Err(format!(
            "the final windows differ:\n  .seq: {:?}\n  rust: {:?}",
            seq.lines, rust.lines
        ));
    }
    Ok(())
}

/// A Rust hand test and the corpus files its rigs are, in the order it
/// makes them. `tail` names what the Rust test does past its last host
/// call that a `.seq` does not say (its final window is then not compared).
pub struct Twin {
    pub name: &'static str,
    pub rust: fn(),
    pub files: &'static [&'static str],
    pub tail: Option<&'static str>,
}

/// The `.seq` file's path (its one rig's).
pub fn seq_path(seq: &Seq) -> Result<Path, String> {
    let mut out = Err(String::new());
    let paths = record(|| out = replay(seq, Broken::No).map(|_| ()));
    out.map_err(|e| format!("{}: {e}", seq.name))?;
    match <[Path; 1]>::try_from(paths) {
        Ok([p]) => Ok(p),
        Err(ps) => Err(format!("{}: {} rigs", seq.name, ps.len())),
    }
}

/// Each twin's rigs against its files.
pub fn check_twins(twins: &[Twin], all: &[Seq]) -> Result<BTreeSet<&'static str>, String> {
    let mut held = BTreeSet::new();
    for t in twins {
        let rust = record(t.rust);
        if rust.len() != t.files.len() {
            return Err(format!(
                "{}: makes {} rigs, the twin names {} files",
                t.name,
                rust.len(),
                t.files.len()
            ));
        }
        for (path, file) in rust.iter().zip(t.files) {
            let seq = all
                .iter()
                .find(|s| s.name == *file)
                .ok_or_else(|| format!("{}: no corpus file {file}", t.name))?;
            let mine = seq_path(seq)?;
            same(&mine, path, t.tail.is_none())
                .map_err(|e| format!("{file} against {}: {e}", t.name))?;
            held.insert(*file);
        }
    }
    Ok(held)
}

// ---- the twins --------------------------------------------------------

use crate::turn_replays as tr;

/// Every hand sequence of `turn_replays.rs`, against its corpus files.
/// (`clause_words_fit_their_bytes` is no sequence: it stays, T5.)
pub const TURN_TWINS: &[Twin] = &[
    Twin {
        name: "claude_turn_with_an_approval_box",
        rust: tr::claude_turn_with_an_approval_box,
        files: &["prompt-approval-box.seq"],
        tail: None,
    },
    Twin {
        name: "question_dialog_mid_turn_stays_in_the_turn",
        rust: tr::question_dialog_mid_turn_stays_in_the_turn,
        files: &["prompt-question-dialog.seq"],
        tail: None,
    },
    Twin {
        name: "usage_wall_at_rest_closes_the_turn_and_holds_its_need",
        rust: tr::usage_wall_at_rest_closes_the_turn_and_holds_its_need,
        files: &["wall-at-rest-closes-the-turn.seq"],
        tail: Some("the run's close, which no host step makes"),
    },
    Twin {
        name: "agent_leaving_closes_its_box_gone",
        rust: tr::agent_leaving_closes_its_box_gone,
        files: &["gone-closes-the-box-and-the-turn.seq"],
        tail: None,
    },
    Twin {
        name: "a_settled_turn_is_dated_at_its_rest",
        rust: tr::a_settled_turn_is_dated_at_its_rest,
        files: &["settled-turn-dated-at-rest.seq"],
        tail: None,
    },
    Twin {
        name: "a_late_wake_never_merges_two_turns",
        rust: tr::a_late_wake_never_merges_two_turns,
        files: &["late-wake-never-merges.seq"],
        tail: None,
    },
    Twin {
        name: "input_stall_never_closes_the_turn",
        rust: tr::input_stall_never_closes_the_turn,
        files: &["stall-never-closes-the-turn.seq"],
        tail: None,
    },
    Twin {
        name: "codex_mid_answer_idle_does_not_close_the_turn",
        rust: tr::codex_mid_answer_idle_does_not_close_the_turn,
        files: &["idle-settles-after-a-second.seq"],
        tail: None,
    },
    Twin {
        name: "interrupted_turn_closes_interrupted",
        rust: tr::interrupted_turn_closes_interrupted,
        files: &["interrupted-turn.seq"],
        tail: None,
    },
    Twin {
        name: "unknown_never_closes_a_turn",
        rust: tr::unknown_never_closes_a_turn,
        files: &["unknown-never-closes-a-turn.seq"],
        tail: None,
    },
    Twin {
        name: "prose_question_closes_asked_heuristic",
        rust: tr::prose_question_closes_asked_heuristic,
        files: &["prose-question-closes-asked.seq"],
        tail: None,
    },
    Twin {
        name: "ctl_turn_and_input_attribute_the_opening_turn",
        rust: tr::ctl_turn_and_input_attribute_the_opening_turn,
        files: &["ctl-turn-attributes-the-next-turn.seq"],
        tail: None,
    },
    Twin {
        name: "ctl_turn_while_resting_starts_its_own_turn",
        rust: tr::ctl_turn_while_resting_starts_its_own_turn,
        files: &["ctl-turn-while-resting.seq"],
        tail: None,
    },
    Twin {
        name: "a_stale_ledger_labels_no_later_turn",
        rust: tr::a_stale_ledger_labels_no_later_turn,
        files: &[
            "stale-ledger-labels-no-later-turn.seq",
            "stale-ledger-past-the-window.seq",
        ],
        tail: None,
    },
];

use crate::transcript_replays as xr;

/// The hand sequences of `transcript_replays.rs` the corpus holds, against
/// its files. Not held, and why: `a_turn_end_closes_the_turn_and_then_amends_its_numbers`
/// (it writes a mail mark into the log by hand, no host call),
/// `a_closed_run_takes_no_transcript_row` (the run's close, no host call),
/// `the_worst_tracker_line_fits_a_carry_line` (no sequence; kept, T5).
pub const TX_TWINS: &[Twin] = &[
    Twin {
        name: "a_prompt_confirms_the_screens_turn_in_its_window",
        rust: xr::a_prompt_confirms_the_screens_turn_in_its_window,
        files: &["tx-twin-a-prompt-confirms-the-screens-turn.seq"],
        tail: None,
    },
    Twin {
        name: "a_prompt_6s_after_the_turn_opened_does_not_attach",
        rust: xr::a_prompt_6s_after_the_turn_opened_does_not_attach,
        files: &["tx-twin-a-prompt-6s-after-does-not-attach.seq"],
        tail: None,
    },
    Twin {
        name: "a_prompt_into_a_turn_holding_another_vendor_turn_supersedes_it",
        rust: xr::a_prompt_into_a_turn_holding_another_vendor_turn_supersedes_it,
        files: &["tx-twin-a-prompt-into-a-held-vendor-turn-supersedes-it.seq"],
        tail: None,
    },
    Twin {
        name: "an_approval_open_when_v2s_prompt_comes_keeps_its_turn",
        rust: xr::an_approval_open_when_v2s_prompt_comes_keeps_its_turn,
        files: &["tx-twin-an-approval-open-when-v2s-prompt-comes.seq"],
        tail: None,
    },
    Twin {
        name: "a_prompt_after_the_settle_confirms_the_closed_turn_late",
        rust: xr::a_prompt_after_the_settle_confirms_the_closed_turn_late,
        files: &[
            "tx-twin-a-prompt-after-the-settle-confirms-late.seq",
            "tx-twin-a-prompt-6s-after-the-settle-opens-its-own.seq",
        ],
        tail: None,
    },
    Twin {
        name: "a_prompt_older_than_the_open_turn_does_not_name_it",
        rust: xr::a_prompt_older_than_the_open_turn_does_not_name_it,
        files: &[
            "tx-twin-a-prompt-older-than-the-open-turn-on.seq",
            "tx-twin-a-prompt-older-than-the-open-turn-partial.seq",
            "tx-twin-a-late-prompt-names-the-nearer-turn.seq",
        ],
        tail: None,
    },
    Twin {
        name: "a_prompt_read_before_the_screens_turn_names_it",
        rust: xr::a_prompt_read_before_the_screens_turn_names_it,
        files: &[
            "tx-twin-a-prompt-read-before-the-screens-turn.seq",
            "tx-twin-a-prompt-read-past-the-window.seq",
        ],
        tail: None,
    },
    Twin {
        name: "a_prompt_the_screen_missed_opens_the_vendor_turn",
        rust: xr::a_prompt_the_screen_missed_opens_the_vendor_turn,
        files: &["tx-twin-a-prompt-the-screen-missed.seq"],
        tail: None,
    },
    Twin {
        name: "tool_calls_open_under_the_mapped_turn_and_results_close_them",
        rust: xr::tool_calls_open_under_the_mapped_turn_and_results_close_them,
        files: &["tx-twin-tool-calls-under-the-mapped-turn.seq"],
        tail: None,
    },
    Twin {
        name: "a_tool_use_for_a_closed_or_unmapped_turn_never_opens_a_step",
        rust: xr::a_tool_use_for_a_closed_or_unmapped_turn_never_opens_a_step,
        files: &[
            "tx-twin-a-tool-use-for-a-closed-turn.seq",
            "tx-twin-a-tool-use-for-an-unmapped-turn.seq",
        ],
        tail: None,
    },
    Twin {
        name: "a_turn_end_with_a_screen_step_open_only_amends",
        rust: xr::a_turn_end_with_a_screen_step_open_only_amends,
        files: &[
            "tx-twin-a-turn-end-with-a-screen-step-open.seq",
            "tx-twin-a-turn-end-for-a-closed-turn-amends.seq",
        ],
        tail: None,
    },
    Twin {
        name: "a_turn_end_takes_the_settles_outcome_and_the_interrupt",
        rust: xr::a_turn_end_takes_the_settles_outcome_and_the_interrupt,
        files: &[
            "tx-twin-a-turn-end-takes-the-asked-settle.seq",
            "tx-twin-an-interrupt-ends-the-vendor-turn.seq",
        ],
        tail: None,
    },
    Twin {
        name: "an_interrupted_vendor_turn_is_continued_by_no_later_turn",
        rust: xr::an_interrupted_vendor_turn_is_continued_by_no_later_turn,
        files: &[
            "tx-twin-an-interrupted-vendor-turn-continues-nothing-on.seq",
            "tx-twin-an-interrupted-vendor-turn-continues-nothing-partial.seq",
        ],
        tail: None,
    },
    Twin {
        name: "a_settles_close_is_never_dated_before_a_transcript_child",
        rust: xr::a_settles_close_is_never_dated_before_a_transcript_child,
        files: &[
            "tx-twin-a-settles-close-after-its-children-on.seq",
            "tx-twin-a-settles-close-after-its-children-partial.seq",
            "tx-twin-a-settles-close-after-its-children-partial-no-child.seq",
        ],
        tail: None,
    },
    Twin {
        name: "a_transcript_close_is_never_dated_before_a_child",
        rust: xr::a_transcript_close_is_never_dated_before_a_child,
        files: &["tx-twin-a-transcript-close-after-its-child.seq"],
        tail: None,
    },
    Twin {
        name: "the_screen_busy_again_continues_a_running_vendor_turn",
        rust: xr::the_screen_busy_again_continues_a_running_vendor_turn,
        files: &[
            "tx-twin-busy-again-continues-a-running-vendor-turn.seq",
            "tx-twin-an-ended-vendor-turn-continues-nothing.seq",
        ],
        tail: None,
    },
    Twin {
        name: "a_row_at_the_watermark_is_never_applied_again",
        rust: xr::a_row_at_the_watermark_is_never_applied_again,
        files: &["tx-twin-a-row-at-the-watermark.seq"],
        tail: None,
    },
    Twin {
        name: "the_judge_confirms_the_screens_wall_once",
        rust: xr::the_judge_confirms_the_screens_wall_once,
        files: &["tx-twin-the-judge-confirms-once.seq"],
        tail: None,
    },
    Twin {
        name: "a_refuted_wall_shuts_and_the_turn_closes_at_the_next_word",
        rust: xr::a_refuted_wall_shuts_and_the_turn_closes_at_the_next_word,
        files: &["tx-twin-a-refuted-wall-shuts.seq"],
        tail: None,
    },
    Twin {
        name: "a_refute_of_a_carried_wall_refines_its_turn_to_done",
        rust: xr::a_refute_of_a_carried_wall_refines_its_turn_to_done,
        files: &["tx-twin-a-refute-of-a-carried-wall.seq"],
        tail: None,
    },
    Twin {
        name: "a_judge_for_another_wall_or_a_partial_build_writes_nothing",
        rust: xr::a_judge_for_another_wall_or_a_partial_build_writes_nothing,
        files: &["tx-twin-a-judge-for-another-wall.seq"],
        tail: None,
    },
    Twin {
        name: "the_vendor_turns_own_notice_refines_the_closed_turn",
        rust: xr::the_vendor_turns_own_notice_refines_the_closed_turn,
        files: &["tx-endwall-refines-the-closed-turn.seq"],
        tail: None,
    },
    Twin {
        name: "a_bind_seeds_past_the_history_and_says_so_once",
        rust: xr::a_bind_seeds_past_the_history_and_says_so_once,
        files: &["tx-twin-a-bind-seeds-past-the-history.seq"],
        tail: None,
    },
    Twin {
        name: "an_adoption_without_a_watermark_replays_nothing",
        rust: xr::an_adoption_without_a_watermark_replays_nothing,
        files: &["tx-twin-an-adoption-without-a-watermark.seq"],
        tail: None,
    },
    Twin {
        name: "an_overflow_desyncs_the_join_until_the_next_prompt",
        rust: xr::an_overflow_desyncs_the_join_until_the_next_prompt,
        files: &["tx-twin-an-overflow-desyncs-the-join.seq"],
        tail: None,
    },
    Twin {
        name: "a_ninth_parallel_tool_call_is_counted_never_opened",
        rust: xr::a_ninth_parallel_tool_call_is_counted_never_opened,
        files: &["tx-twin-a-ninth-parallel-tool-call.seq"],
        tail: None,
    },
    Twin {
        name: "lost_rows_are_counted_on_the_source_mark_never_as_ring_loss",
        rust: xr::lost_rows_are_counted_on_the_source_mark_never_as_ring_loss,
        files: &["tx-twin-lost-rows-are-counted.seq"],
        tail: None,
    },
    Twin {
        name: "a_loss_in_a_steady_session_is_said_once_at_the_batchs_end",
        rust: xr::a_loss_in_a_steady_session_is_said_once_at_the_batchs_end,
        files: &["tx-twin-a-loss-in-a-steady-session.seq"],
        tail: None,
    },
    Twin {
        name: "the_watermark_is_keyed_on_the_files_identity",
        rust: xr::the_watermark_is_keyed_on_the_files_identity,
        files: &["tx-twin-the-watermark-is-the-files-identity.seq"],
        tail: None,
    },
    Twin {
        name: "a_partial_build_adds_and_never_overrules",
        rust: xr::a_partial_build_adds_and_never_overrules,
        files: &["tx-twin-a-partial-build-adds-and-never-overrules.seq"],
        tail: None,
    },
    Twin {
        name: "the_runs_model_is_amended_only_when_it_moves",
        rust: xr::the_runs_model_is_amended_only_when_it_moves,
        files: &["tx-twin-the-runs-model-moves.seq"],
        tail: None,
    },
    Twin {
        name: "a_compaction_is_an_instant_step",
        rust: xr::a_compaction_is_an_instant_step,
        files: &["tx-twin-a-compaction-is-an-instant-step.seq"],
        tail: None,
    },
    Twin {
        name: "the_carry_keeps_the_join_and_the_successor_rebuilds_its_steps",
        rust: xr::the_carry_keeps_the_join_and_the_successor_rebuilds_its_steps,
        files: &["tx-twin-the-carry-keeps-the-join.seq"],
        tail: None,
    },
];

/// Every ported hand sequence takes its Rust twin's path, step for step,
/// and ends in its window: every `turn_replays` sequence (14 tests, 15
/// rigs) and 33 of `transcript_replays`' 36 tests (44 rigs).
#[test]
fn each_ported_sequence_folds_as_its_rust_twin() {
    let all = corpus();
    let screen = check_twins(TURN_TWINS, &all).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!((TURN_TWINS.len(), screen.len()), (14, 15), "{screen:?}");
    let tx = check_twins(TX_TWINS, &all).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!((TX_TWINS.len(), tx.len()), (33, 44), "{tx:?}");
    for f in screen.iter().chain(&tx) {
        let seq = all.iter().find(|s| s.name == *f).expect("held");
        let rig = if f.starts_with("tx-") {
            RigKind::Transcript
        } else {
            RigKind::Screen
        };
        assert_eq!(seq.rig, rig, "{f}: a twin of the other rig");
    }
}

/// NEGATIVE CONTROLS: a ported file with one step altered, or its last
/// step dropped, no longer takes its twin's path; nor does a file against
/// another test's twin.
#[test]
fn an_altered_step_fails_its_twin() {
    use crate::seq_replay::Ev;
    let all = corpus();
    let twin = TURN_TWINS
        .iter()
        .find(|t| t.name == "claude_turn_with_an_approval_box")
        .expect("the twin");
    let seq = all
        .iter()
        .find(|s| s.name == twin.files[0])
        .expect("its file");
    let [rust] = <[Path; 1]>::try_from(record(twin.rust)).expect("one rig");
    same(&seq_path(seq).expect("replays"), &rust, true).expect("the shipping twin");
    let altered = |f: &dyn Fn(&mut Seq)| {
        let mut s = seq.clone();
        f(&mut s);
        seq_path(&s).and_then(|p| same(&p, &rust, true))
    };
    // A half for an elapse: the same time, no wake.
    let swapped = altered(&|s| {
        let i = s
            .lines
            .iter()
            .position(|l| matches!(l.ev, Ev::Half))
            .expect("a half");
        s.lines[i].ev = Ev::Elapse;
    });
    assert!(swapped.is_err(), "an altered step");
    let dropped = altered(&|s| {
        let i = s
            .lines
            .iter()
            .rposition(|l| !matches!(l.ev, Ev::Expect(_) | Ev::Nothing))
            .expect("a step");
        s.lines.truncate(i);
    });
    assert!(dropped.is_err(), "the last step dropped");
    let other = all
        .iter()
        .find(|s| s.name == "prompt-question-dialog.seq")
        .expect("another file");
    assert!(
        seq_path(other).and_then(|p| same(&p, &rust, true)).is_err(),
        "another test's file"
    );
    // A transcript twin whose row is read at another time.
    let tx = TX_TWINS
        .iter()
        .find(|t| t.name == "a_prompt_confirms_the_screens_turn_in_its_window")
        .expect("the twin");
    let [txr] = <[Path; 1]>::try_from(record(tx.rust)).expect("one rig");
    let txs = all
        .iter()
        .find(|s| s.name == tx.files[0])
        .expect("its file");
    same(&seq_path(txs).expect("replays"), &txr, true).expect("the shipping twin");
    let mut moved = txs.clone();
    let i = moved
        .lines
        .iter()
        .position(|l| matches!(l.ev, Ev::Pass(_)))
        .expect("a pass");
    moved.lines[i].ev = Ev::Pass(4000);
    assert!(
        seq_path(&moved).and_then(|p| same(&p, &txr, true)).is_err(),
        "a row read at another time"
    );
    // Paths recorded with nothing on them are no twins.
    let empty = Path {
        steps: Vec::new(),
        lines: txr.lines.clone(),
    };
    assert!(same(&empty, &empty, true).is_err(), "an empty path");
}
