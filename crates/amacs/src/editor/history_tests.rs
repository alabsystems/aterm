// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests of the one-writer history: undo across agent edits and reloads, the
//! out-of-order agent undo, the bounds, and the Tier-1 bind to `AmacsUndo`.

use super::*;
use crate::editor::{EditorCommand, Selection, editor};

fn text(store: &DocumentStore, document: DocumentId) -> String {
    store.snapshot(document).unwrap().text.to_string()
}

fn seq(store: &DocumentStore, document: DocumentId) -> Seq {
    store.snapshot(document).unwrap().seq
}

fn agent(label: &str) -> Origin {
    Origin::Agent {
        principal: "claude".to_string(),
        label: label.to_string(),
    }
}

/// An agent's request at the head, with the view carried through it as a
/// host carries every view.
fn agent_edit(
    store: &mut DocumentStore,
    workspace: &mut EditorWorkspace,
    view: &mut EditorBufferView,
    range: std::ops::Range<usize>,
    insert: &str,
    label: &str,
) {
    let document = view.document;
    let (seq, deltas) = workspace
        .apply(
            store,
            document,
            seq(store, document),
            Request {
                edits: vec![TextEdit {
                    range,
                    insert: insert.to_string(),
                }],
                origin: agent(label),
            },
        )
        .unwrap();
    view.observe_external(seq, &deltas);
}

fn reload(
    store: &mut DocumentStore,
    workspace: &mut EditorWorkspace,
    view: &mut EditorBufferView,
    disk: &str,
) {
    let document = view.document;
    let (seq, deltas) = workspace
        .replace_text(store, document, seq(store, document), disk, Origin::Disk)
        .unwrap();
    view.observe_external(seq, &deltas);
}

fn status(effects: &[EditorEffect]) -> String {
    effects
        .iter()
        .find_map(|effect| match effect {
            EditorEffect::Status(words) => Some(words.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

#[test]
fn undo_redo_round_trips_atomic_frame() {
    let (mut store, mut workspace, mut view, document) = editor("abc");
    view.selections = vec![Selection { anchor: 1, head: 2 }];
    workspace.insert_text(&mut store, &mut view, "XYZ").unwrap();
    assert_eq!(text(&store, document), "aXYZc");
    workspace
        .execute(&mut store, &mut view, EditorCommand::Undo)
        .unwrap();
    assert_eq!(text(&store, document), "abc");
    workspace
        .execute(&mut store, &mut view, EditorCommand::Redo)
        .unwrap();
    assert_eq!(text(&store, document), "aXYZc");
}

#[test]
fn undo_and_redo_step_through_more_than_one_edit() {
    let (mut store, mut workspace, mut view, document) = editor("abc");
    // Two characters at once never join a run of typing.
    workspace.insert_text(&mut store, &mut view, "xx").unwrap();
    workspace.insert_text(&mut store, &mut view, "yy").unwrap();
    assert_eq!(text(&store, document), "xxyyabc");
    for expected in ["xxabc", "abc"] {
        workspace
            .execute(&mut store, &mut view, EditorCommand::Undo)
            .unwrap();
        assert_eq!(text(&store, document), expected);
    }
    for expected in ["xxabc", "xxyyabc"] {
        workspace
            .execute(&mut store, &mut view, EditorCommand::Redo)
            .unwrap();
        assert_eq!(text(&store, document), expected);
    }
}

#[test]
fn undo_redo_availability_tracks_the_real_document_stacks() {
    let (mut store, mut workspace, mut view, document) = editor("abc");
    assert!(!workspace.can_undo(document));
    assert!(!workspace.can_redo(document));

    workspace.insert_text(&mut store, &mut view, "x").unwrap();
    assert!(workspace.can_undo(document));
    assert!(!workspace.can_redo(document));

    workspace
        .execute(&mut store, &mut view, EditorCommand::Undo)
        .unwrap();
    assert!(!workspace.can_undo(document));
    assert!(workspace.can_redo(document));

    workspace
        .execute(&mut store, &mut view, EditorCommand::Redo)
        .unwrap();
    assert!(workspace.can_undo(document));
    assert!(!workspace.can_redo(document));
}

#[test]
fn commit_publication_keeps_document_and_undo_redo_transaction_boundaries() {
    let (mut store, mut workspace, mut view, document) = editor("abc");
    let other_document = store.open("mem://other".to_string(), "other".to_string());
    let mut other = workspace
        .attach(&mut store, other_document, crate::store::DocumentViewId(92))
        .unwrap();
    workspace.insert_text(&mut store, &mut view, "x").unwrap();
    let inserted = seq(&store, document);
    workspace.insert_text(&mut store, &mut other, "y").unwrap();
    let other_seq = seq(&store, other_document);
    workspace
        .execute(&mut store, &mut view, EditorCommand::Undo)
        .unwrap();
    let undone = seq(&store, document);
    workspace
        .execute(&mut store, &mut view, EditorCommand::Redo)
        .unwrap();
    let redone = seq(&store, document);

    assert_eq!(
        workspace.take_commits(document),
        vec![
            (
                inserted,
                vec![EditDelta {
                    old: 0..0,
                    inserted_len: 1
                }]
            ),
            (
                undone,
                vec![EditDelta {
                    old: 0..1,
                    inserted_len: 0
                }]
            ),
            (
                redone,
                vec![EditDelta {
                    old: 0..0,
                    inserted_len: 1
                }]
            ),
        ]
    );
    assert!(workspace.take_commits(document).is_empty());
    assert_eq!(
        workspace.take_commits(other_document),
        vec![(
            other_seq,
            vec![EditDelta {
                old: 0..0,
                inserted_len: 1
            }]
        )]
    );
    assert!(workspace.take_commits(other_document).is_empty());
    assert_eq!(text(&store, document), "xabc");
    assert_eq!(text(&store, other_document), "yother");
}

/// C-/ walks back through a reload from disk and an agent's edit as through
/// the person's own typing, each a labelled frame, and redo walks forward.
#[test]
fn undo_reaches_agent_edits_and_reloads_as_labelled_frames() {
    let (mut store, mut workspace, mut view, document) = editor("one\ntwo\n");
    view.selections = vec![Selection::caret(3)];
    workspace.insert_text(&mut store, &mut view, "!").unwrap();
    agent_edit(&mut store, &mut workspace, &mut view, 5..8, "TWO", "shout");
    reload(&mut store, &mut workspace, &mut view, "one!\nTWO\nthree\n");
    assert_eq!(text(&store, document), "one!\nTWO\nthree\n");
    assert_eq!(
        workspace
            .frame_probe(document)
            .into_iter()
            .map(|frame| frame.origin)
            .collect::<Vec<_>>(),
        [
            Origin::Keyboard {
                view: view.document_view
            },
            agent("shout"),
            Origin::Disk
        ]
    );

    let said = workspace
        .execute(&mut store, &mut view, EditorCommand::Undo)
        .unwrap();
    assert_eq!(text(&store, document), "one!\nTWO\n");
    assert!(
        status(&said).starts_with("Undid the reload from disk"),
        "{said:?}"
    );
    let said = workspace
        .execute(&mut store, &mut view, EditorCommand::Undo)
        .unwrap();
    assert_eq!(text(&store, document), "one!\ntwo\n");
    assert!(
        status(&said).starts_with("Undid agent edit 'shout'"),
        "{said:?}"
    );
    let said = workspace
        .execute(&mut store, &mut view, EditorCommand::Undo)
        .unwrap();
    assert_eq!(text(&store, document), "one\ntwo\n");
    assert!(
        said.is_empty(),
        "a person's own undo says nothing: {said:?}"
    );
    assert_eq!(
        workspace.execute(&mut store, &mut view, EditorCommand::Undo),
        Err(EditorError::NothingToUndo)
    );
    for expected in ["one!\ntwo\n", "one!\nTWO\n", "one!\nTWO\nthree\n"] {
        workspace
            .execute(&mut store, &mut view, EditorCommand::Redo)
            .unwrap();
        assert_eq!(text(&store, document), expected);
    }
}

/// A reload over unsaved typing is one frame: undoing it brings the typing
/// back, so taking the disk's version never loses the person's.
#[test]
fn undoing_a_reload_brings_the_unsaved_text_back() {
    let (mut store, mut workspace, mut view, document) = editor("alpha\nbeta\n");
    view.selections = vec![Selection::caret(5)];
    workspace
        .insert_text(&mut store, &mut view, " mine")
        .unwrap();
    reload(
        &mut store,
        &mut workspace,
        &mut view,
        "alpha\nbeta theirs\n",
    );
    assert_eq!(text(&store, document), "alpha\nbeta theirs\n");
    workspace
        .execute(&mut store, &mut view, EditorCommand::Undo)
        .unwrap();
    assert_eq!(text(&store, document), "alpha mine\nbeta\n");
}

/// C-c u undoes the newest agent edit under the person's later typing, as a
/// frame of its own: C-/ puts the agent's edit back.
#[test]
fn undo_agent_edit_undoes_it_out_of_order_and_c_slash_puts_it_back() {
    let (mut store, mut workspace, mut view, document) = editor("use a;\nfn main() {}\n");
    agent_edit(
        &mut store,
        &mut workspace,
        &mut view,
        0..6,
        "use a::b;",
        "fix imports",
    );
    view.selections = vec![Selection::caret(text(&store, document).len())];
    workspace
        .insert_text(&mut store, &mut view, "// end\n")
        .unwrap();
    assert_eq!(text(&store, document), "use a::b;\nfn main() {}\n// end\n");

    let said = workspace
        .execute(&mut store, &mut view, EditorCommand::UndoAgentEdit)
        .unwrap();
    assert_eq!(text(&store, document), "use a;\nfn main() {}\n// end\n");
    assert!(
        status(&said).starts_with("Undid agent edit 'fix imports'"),
        "{said:?}"
    );
    assert_eq!(
        workspace.execute(&mut store, &mut view, EditorCommand::UndoAgentEdit),
        Err(EditorError::UndoRefused {
            why: "No agent edit to undo".to_string()
        }),
        "an undone agent edit is not undone twice"
    );

    workspace
        .execute(&mut store, &mut view, EditorCommand::Undo)
        .unwrap();
    assert_eq!(text(&store, document), "use a::b;\nfn main() {}\n// end\n");
    workspace
        .execute(&mut store, &mut view, EditorCommand::Undo)
        .unwrap();
    assert_eq!(text(&store, document), "use a::b;\nfn main() {}\n");
}

/// A person's typing on the agent's text blocks C-c u, which names it; C-/
/// takes the typing back, and then C-c u can.
#[test]
fn a_blocked_agent_undo_names_its_blocker_and_never_applies() {
    let (mut store, mut workspace, mut view, document) = editor("x = 1\n");
    agent_edit(&mut store, &mut workspace, &mut view, 4..5, "2", "bump");
    view.selections = vec![Selection { anchor: 4, head: 5 }];
    workspace.insert_text(&mut store, &mut view, "3").unwrap();
    let typed = seq(&store, document);
    let refused = workspace.execute(&mut store, &mut view, EditorCommand::UndoAgentEdit);
    assert_eq!(
        refused,
        Err(EditorError::UndoRefused {
            why: format!(
                "Undo of agent edit 'bump' blocked by your edit (seq {}); C-/ undoes that first",
                typed.0
            )
        })
    );
    assert_eq!(text(&store, document), "x = 3\n", "nothing was applied");

    workspace
        .execute(&mut store, &mut view, EditorCommand::Undo)
        .unwrap();
    assert_eq!(text(&store, document), "x = 2\n");
    workspace
        .execute(&mut store, &mut view, EditorCommand::UndoAgentEdit)
        .unwrap();
    assert_eq!(text(&store, document), "x = 1\n");
}

/// A commit that bypassed the writer is never an error a person meets as
/// "the history diverged": every frame it may have moved is blocked and
/// named, and nothing is applied at stale coordinates.
#[test]
fn a_change_outside_the_writer_blocks_rather_than_diverges() {
    let (mut store, mut workspace, mut view, document) = editor("abc");
    workspace.insert_text(&mut store, &mut view, "x").unwrap();
    let head = seq(&store, document);
    let DocumentTxnOutcome::Committed { seq: outside, .. } = store.transact(
        document,
        head,
        vec![TextEdit {
            range: 0..0,
            insert: "!".to_string(),
        }],
    ) else {
        panic!("the bypassing commit lands")
    };
    let snapshot = store.snapshot(document).unwrap();
    view.observe_external(snapshot.seq, &[]);
    assert_eq!(
        workspace.execute(&mut store, &mut view, EditorCommand::Undo),
        Err(EditorError::UndoRefused {
            why: format!(
                "Undo blocked by a change made outside the editor (seq {})",
                outside.0
            )
        })
    );
    assert_eq!(text(&store, document), "!xabc");
}

/// The history keeps at most `UNDO_FRAME_LIMIT` frames: the oldest go, a
/// status line says so once (not at every key after), again only once an
/// undo took the history under its bound and it filled again, and the rest
/// still undo.
#[test]
fn the_frame_bound_drops_the_oldest_and_says_so_once() {
    let (mut store, mut workspace, mut view, document) = editor("");
    let full = |dropped: &str| {
        vec![EditorEffect::Status(format!(
            "Undo history is full: the oldest {dropped} can no longer be undone"
        ))]
    };
    let mut notes = Vec::new();
    // Two characters at once: each is its own frame.
    for _ in 0..UNDO_FRAME_LIMIT + 2 {
        let said = workspace.insert_text(&mut store, &mut view, "ab").unwrap();
        notes.extend(said);
    }
    assert_eq!(workspace.frame_probe(document).len(), UNDO_FRAME_LIMIT);
    assert_eq!(
        notes,
        full("1 edit"),
        "said at the first drop, not the second"
    );

    workspace
        .execute(&mut store, &mut view, EditorCommand::Undo)
        .unwrap();
    let mut notes = Vec::new();
    for _ in 0..2 {
        let said = workspace.insert_text(&mut store, &mut view, "ab").unwrap();
        notes.extend(said);
    }
    assert_eq!(
        notes,
        full("1 edit"),
        "full again after the undo: said again"
    );
    while workspace
        .execute(&mut store, &mut view, EditorCommand::Undo)
        .is_ok()
    {}
    assert_eq!(
        text(&store, document),
        "ababab",
        "the three dropped edits stay"
    );
}

/// Consecutive self-inserts of one character join one frame, up to
/// `AMALGAM_CHARS`, so C-/ takes back a run of typing; a command, a newline,
/// a paste, another view's typing or another commit between ends the run.
#[test]
fn typing_joins_one_frame_until_twenty_characters_or_something_between() {
    let typed = |workspace: &mut EditorWorkspace,
                 store: &mut DocumentStore,
                 view: &mut EditorBufferView,
                 keys: &str| {
        for key in keys.chars() {
            workspace
                .insert_text(store, view, &key.to_string())
                .unwrap();
        }
    };
    let (mut store, mut workspace, mut view, document) = editor("");
    typed(
        &mut workspace,
        &mut store,
        &mut view,
        "abcdefghijklmnopqrstuvwxy",
    );
    assert_eq!(
        workspace.frame_probe(document).len(),
        2,
        "25 keys are two frames: 20 and 5"
    );
    workspace
        .execute(&mut store, &mut view, EditorCommand::Undo)
        .unwrap();
    assert_eq!(text(&store, document), "abcdefghijklmnopqrst");
    workspace
        .execute(&mut store, &mut view, EditorCommand::Undo)
        .unwrap();
    assert_eq!(text(&store, document), "");
    workspace
        .execute(&mut store, &mut view, EditorCommand::Redo)
        .unwrap();
    assert_eq!(
        text(&store, document),
        "abcdefghijklmnopqrst",
        "redo puts the run back whole"
    );

    // Each of these ends the run: the next key is a frame of its own.
    let (mut store, mut workspace, mut view, document) = editor("");
    let frames = |workspace: &EditorWorkspace| workspace.frame_probe(document).len();
    typed(&mut workspace, &mut store, &mut view, "ab");
    assert_eq!(frames(&workspace), 1);
    workspace
        .execute(&mut store, &mut view, EditorCommand::MoveBackward)
        .unwrap();
    workspace
        .execute(&mut store, &mut view, EditorCommand::MoveForward)
        .unwrap();
    typed(&mut workspace, &mut store, &mut view, "c");
    assert_eq!(
        frames(&workspace),
        2,
        "a command between, caret back in place"
    );
    workspace.insert_text(&mut store, &mut view, "\n").unwrap();
    typed(&mut workspace, &mut store, &mut view, "d");
    assert_eq!(frames(&workspace), 4, "a newline is its own frame");
    workspace.insert_paste(&mut store, &mut view, "e").unwrap();
    typed(&mut workspace, &mut store, &mut view, "f");
    assert_eq!(frames(&workspace), 6, "a paste is its own frame");
    let end = text(&store, document).len();
    agent_edit(&mut store, &mut workspace, &mut view, end..end, "!", "bang");
    view.selections = vec![Selection::caret(end)];
    typed(&mut workspace, &mut store, &mut view, "g");
    assert_eq!(frames(&workspace), 8, "another commit between");
    view.selections = vec![Selection::caret(0)];
    typed(&mut workspace, &mut store, &mut view, "h");
    assert_eq!(frames(&workspace), 9, "the caret moved by a click");
    let mut other = workspace
        .attach(&mut store, document, crate::store::DocumentViewId(93))
        .unwrap();
    other.selections = vec![Selection::caret(1)];
    typed(&mut workspace, &mut store, &mut other, "i");
    assert_eq!(frames(&workspace), 10, "another view's typing");
    assert_eq!(text(&store, document), "hiabc\ndefg!");
}

/// A frame blocked by a later commit keeps the coordinates it was blocked
/// at: a commit after that moves its text, but every commit after the
/// blocker is undone before the blocker is, so its coordinates are exact
/// again when it is free. NEGATIVE CONTROL: the rig that carries blocked
/// frames through later commits puts the text back in the wrong place.
#[test]
fn a_blocked_frame_keeps_its_coordinates_through_later_commits() {
    let run = |mutant: Option<HistoryMutant>| {
        let (mut store, mut workspace, mut view, document) = editor("a\nbb\n");
        workspace.history_mutant = mutant;
        let mut texts = vec![text(&store, document)];
        // B twice (the second blocks the first), then A, which moves B.
        for (range, insert) in [(2..4, "BBB"), (2..5, "BBBB"), (0..1, "AAAAA")] {
            view.selections = vec![Selection {
                anchor: range.start,
                head: range.end,
            }];
            workspace
                .insert_text(&mut store, &mut view, insert)
                .unwrap();
            texts.push(text(&store, document));
        }
        texts.pop();
        let mut undone = Vec::new();
        while workspace
            .execute(&mut store, &mut view, EditorCommand::Undo)
            .is_ok()
        {
            undone.push(text(&store, document));
        }
        texts.reverse();
        (texts, undone)
    };
    let (expected, undone) = run(None);
    assert_eq!(undone, expected, "each undo gives back the text before");
    let (expected, undone) = run(Some(HistoryMutant::MapBlocked));
    assert_ne!(undone, expected, "the MapBlocked rig undid exactly");
}

/// A recovered draft is where undo starts: C-/ stops at its frame and says
/// how to take the file's text instead, so one key never leaves the file's
/// text over the only copy of the draft. Typing after it still undoes.
#[test]
fn undo_stops_at_a_recovered_draft() {
    let (mut store, mut workspace, mut view, document) = editor("disk\n");
    let head = seq(&store, document);
    let (seq_after, deltas) = workspace
        .replace_text(
            &mut store,
            document,
            head,
            "draft disk\n",
            Origin::Recovered,
        )
        .unwrap();
    view.observe_external(seq_after, &deltas);
    assert!(!workspace.can_undo(document), "Undo is off at the draft");
    assert_eq!(
        workspace.execute(&mut store, &mut view, EditorCommand::Undo),
        Err(EditorError::UndoRefused {
            why: RECOVERED_FLOOR.to_string()
        })
    );
    assert_eq!(text(&store, document), "draft disk\n", "nothing was undone");

    view.selections = vec![Selection::caret(0)];
    workspace.insert_text(&mut store, &mut view, "x").unwrap();
    assert!(workspace.can_undo(document));
    workspace
        .execute(&mut store, &mut view, EditorCommand::Undo)
        .unwrap();
    assert_eq!(
        text(&store, document),
        "draft disk\n",
        "the typing after it undoes"
    );
    assert!(
        workspace
            .execute(&mut store, &mut view, EditorCommand::Undo)
            .is_err()
    );
    assert_eq!(text(&store, document), "draft disk\n");
}

/// The history holds at most `UNDO_BYTE_LIMIT` bytes to put back, but always
/// its newest frame, however large.
#[test]
fn the_byte_bound_drops_old_frames_but_keeps_the_newest() {
    let big = "b".repeat(UNDO_BYTE_LIMIT / 2 + 1);
    let (mut store, mut workspace, mut view, document) = editor(&big);
    for replacement in ["c", "d"] {
        let len = text(&store, document).len();
        view.selections = vec![Selection {
            anchor: 0,
            head: len,
        }];
        workspace
            .insert_text(
                &mut store,
                &mut view,
                &replacement.repeat(UNDO_BYTE_LIMIT / 2 + 1),
            )
            .unwrap();
    }
    assert_eq!(workspace.frame_probe(document).len(), 1);
    workspace
        .execute(&mut store, &mut view, EditorCommand::Undo)
        .unwrap();
    assert!(text(&store, document).starts_with('c'));
    assert_eq!(
        workspace.execute(&mut store, &mut view, EditorCommand::Undo),
        Err(EditorError::NothingToUndo)
    );
}

mod fuzz {
    //! Seeded runs of random edits (a person's, an agent's, the disk's),
    //! undos, redos and agent undos, each checked against the texts the
    //! frames were committed over. Deterministic: the seed is fixed.

    use super::*;

    /// xorshift64: a fixed seed gives the same run every time.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        fn below(&mut self, n: usize) -> usize {
            usize::try_from(self.next() % u64::try_from(n).unwrap()).unwrap()
        }
    }

    /// One or two ordered, disjoint ranges of a text `len` bytes long (the
    /// texts here are ASCII, so every byte is a character boundary).
    fn ranges(rng: &mut Rng, len: usize) -> Vec<std::ops::Range<usize>> {
        let mut points = [
            rng.below(len + 1),
            rng.below(len + 1),
            rng.below(len + 1),
            rng.below(len + 1),
        ];
        points.sort_unstable();
        let mut ranges = Vec::with_capacity(2);
        ranges.push(points[0]..points[1]);
        if rng.below(2) == 0 && points[2] > points[1] {
            ranges.push(points[2]..points[3]);
        }
        ranges
    }

    /// Fail the run with `why` unless `holds`.
    macro_rules! ensure {
        ($holds:expr, $($why:tt)+) => {
            if !$holds {
                return Err(format!($($why)+));
            }
        };
    }

    /// Linear C-/ and redo are exact: each undo gives back the text its
    /// frame was committed over (a run of typing joined into one frame is
    /// one step), each redo the text it undid, a refused agent undo changes
    /// nothing, and undoing everything gives back the first text. A blocked
    /// frame keeping the coordinates it was blocked at is a rule the
    /// conformance rigs reach only six steps deep; this run reaches it in
    /// many more shapes. NEGATIVE CONTROL: the rigs that map blocked frames
    /// and that never unblock each fail it. (Skipping the rebase is
    /// invisible to undo in order, which undoes every later commit first;
    /// the conformance rigs catch it out of order.)
    #[test]
    fn linear_undo_and_redo_are_exact_over_random_edits() {
        if let Err(why) = linear_run(20_000, None) {
            panic!("{why}");
        }
        for mutant in [HistoryMutant::MapBlocked, HistoryMutant::NoUnblock] {
            assert!(
                linear_run(20_000, Some(mutant)).is_err(),
                "the {mutant:?} rig passed the run"
            );
        }
    }

    /// `rounds` seeded rounds of [`linear_undo_and_redo_are_exact_over_random_edits`]
    /// under `mutant`: the first round that breaks the rule, with its steps.
    fn linear_run(rounds: usize, mutant: Option<HistoryMutant>) -> Result<(), String> {
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
        let initial = "abcdefghij\nklmnop\nqrstuv\n";
        for round in 0..rounds {
            let (mut store, mut workspace, mut view, document) = editor(initial);
            workspace.history_mutant = mutant;
            let mut undo_stack: Vec<String> = Vec::new();
            let mut redo_stack: Vec<String> = Vec::new();
            let mut log: Vec<String> = Vec::new();
            for _ in 0..1 + rng.below(14) {
                let before = text(&store, document);
                let len = before.len();
                match rng.below(9) {
                    0 | 1 => {
                        let ranges = ranges(&mut rng, len);
                        let insert = ["x", "yy", "\n", ""][rng.below(4)];
                        if insert.is_empty() && ranges.iter().all(|range| range.is_empty()) {
                            continue;
                        }
                        view.selections = ranges
                            .iter()
                            .map(|range| Selection {
                                anchor: range.start,
                                head: range.end,
                            })
                            .collect();
                        view.primary = 0;
                        log.push(format!("key {ranges:?} {insert:?}"));
                        let frames = workspace.frame_probe(document).len();
                        workspace
                            .insert_text(&mut store, &mut view, insert)
                            .unwrap();
                        // A key that joined the run before it is one step
                        // with it: its undo goes back past both.
                        if workspace.frame_probe(document).len() > frames {
                            undo_stack.push(before);
                        }
                        redo_stack.clear();
                    }
                    2 | 3 => {
                        let edits: Vec<TextEdit> = ranges(&mut rng, len)
                            .into_iter()
                            .map(|range| TextEdit {
                                range,
                                insert: ["A", "BB", ""][rng.below(3)].to_string(),
                            })
                            .collect();
                        log.push(format!("agent {edits:?}"));
                        let frames = workspace.frame_probe(document).len();
                        let head = seq(&store, document);
                        let (seq, deltas) = workspace
                            .apply(
                                &mut store,
                                document,
                                head,
                                Request {
                                    edits,
                                    origin: agent("fuzz"),
                                },
                            )
                            .unwrap();
                        view.observe_external(seq, &deltas);
                        // An edit that changes nothing is still a frame.
                        if workspace.frame_probe(document).len() > frames {
                            undo_stack.push(before);
                            redo_stack.clear();
                        }
                    }
                    4 => {
                        let mut disk = before.clone();
                        let at = rng.below(len + 1);
                        let end = (at + rng.below(4)).min(len);
                        disk.replace_range(at..end, ["D", "", "dd\n"][rng.below(3)]);
                        log.push(format!("disk {at}..{end} -> {disk:?}"));
                        reload(&mut store, &mut workspace, &mut view, &disk);
                        if disk != before {
                            undo_stack.push(before);
                            redo_stack.clear();
                        }
                    }
                    5 | 6 => {
                        log.push("undo".to_string());
                        let got = workspace.execute(&mut store, &mut view, EditorCommand::Undo);
                        match undo_stack.pop() {
                            Some(expected) => {
                                ensure!(
                                    got.is_ok(),
                                    "round {round}: undo refused {got:?}\n{log:#?}"
                                );
                                ensure!(
                                    text(&store, document) == expected,
                                    "round {round}: undo\n{log:#?}"
                                );
                                redo_stack.push(before);
                            }
                            None => ensure!(got.is_err(), "round {round}\n{log:#?}"),
                        }
                    }
                    7 => {
                        log.push("redo".to_string());
                        let got = workspace.execute(&mut store, &mut view, EditorCommand::Redo);
                        match redo_stack.pop() {
                            Some(expected) => {
                                ensure!(
                                    got.is_ok(),
                                    "round {round}: redo refused {got:?}\n{log:#?}"
                                );
                                ensure!(
                                    text(&store, document) == expected,
                                    "round {round}: redo\n{log:#?}"
                                );
                                undo_stack.push(before);
                            }
                            None => ensure!(got.is_err(), "round {round}\n{log:#?}"),
                        }
                    }
                    _ => {
                        log.push("undo-agent".to_string());
                        let got =
                            workspace.execute(&mut store, &mut view, EditorCommand::UndoAgentEdit);
                        if got.is_ok() {
                            undo_stack.push(before);
                            redo_stack.clear();
                        } else {
                            ensure!(
                                text(&store, document) == before,
                                "round {round}: a refused agent undo changed the text\n{log:#?}"
                            );
                        }
                    }
                }
            }
            while let Some(expected) = undo_stack.pop() {
                let got = workspace.execute(&mut store, &mut view, EditorCommand::Undo);
                ensure!(
                    got.is_ok(),
                    "round {round}: final undo refused {got:?}\n{log:#?}"
                );
                ensure!(
                    text(&store, document) == expected,
                    "round {round}: final undo\n{log:#?}"
                );
            }
            ensure!(text(&store, document) == initial, "round {round}\n{log:#?}");
        }
        Ok(())
    }

    /// C-c u of an agent edit puts back exactly the text it replaced and
    /// keeps a later key typed apart from it; a key that touches it blocks
    /// the undo, which then changes nothing.
    #[test]
    fn an_agent_undo_reverts_only_its_own_region() {
        let mut rng = Rng(0x1234_5678_9abc_def1);
        let initial = "abcdefghij\nklmnop\nqrstuv\n";
        for round in 0..20_000 {
            let (mut store, mut workspace, mut view, document) = editor(initial);
            let len = initial.len();
            let a = rng.below(len + 1);
            let b = (a + rng.below(5)).min(len);
            let insert = ["A", "", "BBB"][rng.below(3)];
            agent_edit(&mut store, &mut workspace, &mut view, a..b, insert, "fuzz");
            // The text C-c u must leave, or None where the key touches the
            // agent's text and must block it.
            let mut expected = Some(initial.to_string());
            if rng.below(2) == 0 {
                let k = rng.below(text(&store, document).len() + 1);
                view.selections = vec![Selection::caret(k)];
                workspace.insert_text(&mut store, &mut view, "k").unwrap();
                let agent_end = a + insert.len();
                expected = if k < a {
                    let mut kept = initial.to_string();
                    kept.insert(k, 'k');
                    Some(kept)
                } else if k > agent_end {
                    let mut kept = initial.to_string();
                    kept.insert(k - insert.len() + (b - a), 'k');
                    Some(kept)
                } else {
                    None
                };
            }
            let before = text(&store, document);
            let got = workspace.execute(&mut store, &mut view, EditorCommand::UndoAgentEdit);
            match expected {
                None => {
                    assert!(got.is_err(), "round {round}: a touching key did not block");
                    assert_eq!(text(&store, document), before, "round {round}");
                }
                Some(expected) => {
                    assert!(got.is_ok(), "round {round}: {got:?}");
                    assert_eq!(
                        text(&store, document),
                        expected,
                        "round {round}: a={a} b={b} insert={insert:?}"
                    );
                }
            }
        }
    }
}

mod conformance {
    //! Tier 1: the history refines `AmacsUndo`
    //! (`aterm_models_native::models_amacs_undo`). The document is two
    //! lines, region A then region B, each holding a version `k` spelled as
    //! the digit `k` `k` times, so every commit changes a region's length and
    //! moves what follows it. Every sequence of up to five operations (a
    //! person's, an agent's or the disk's commit to either region, `C-/`,
    //! `C-c u`) runs on the real workspace; each step must be one the model
    //! admits, each refusal one the model makes too, and each rig that breaks
    //! one rule must be refused and break the law that names it. (A rig
    //! that maps blocked frames needs six steps to show; the fuzz and
    //! `a_blocked_frame_keeps_its_coordinates_through_later_commits` catch
    //! it.)

    use super::super::*;
    use crate::editor::{EditorCommand, Selection, editor};
    use aterm_models_native::models_amacs_undo::amacs_undo_model;
    use aterm_spec::derive::Model;
    use aterm_spec::interp::admits;
    use std::collections::{BTreeMap, BTreeSet};

    type State = BTreeMap<&'static str, i64>;

    const SLOTS: [[&str; 4]; 3] = [
        ["r1", "o1", "old1", "b1"],
        ["r2", "o2", "old2", "b2"],
        ["r3", "o3", "old3", "b3"],
    ];
    const DEPTH: usize = 5;
    const MAX_VER: i64 = 5;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Op {
        /// A commit to `region` (1 = A, 2 = B) by `origin` (1 keyboard, 2
        /// agent, 3 disk).
        Commit {
            origin: i64,
            region: i64,
        },
        Undo,
        UndoAgent,
    }

    const OPS: [Op; 8] = [
        Op::Commit {
            origin: 1,
            region: 1,
        },
        Op::Commit {
            origin: 1,
            region: 2,
        },
        Op::Commit {
            origin: 2,
            region: 1,
        },
        Op::Commit {
            origin: 2,
            region: 2,
        },
        Op::Commit {
            origin: 3,
            region: 1,
        },
        Op::Commit {
            origin: 3,
            region: 2,
        },
        Op::Undo,
        Op::UndoAgent,
    ];

    fn spelled(version: i64) -> String {
        let digit = char::from(b'0' + u8::try_from(version).unwrap());
        std::iter::repeat_n(digit, usize::try_from(version).unwrap()).collect()
    }

    /// The two regions' versions, or `None` for text that is no longer two
    /// lines of one version each.
    fn regions(text: &str) -> Option<(i64, i64)> {
        let mut lines = text.split('\n');
        let mut version = || -> Option<i64> {
            let line = lines.next()?;
            let first = line.chars().next()?;
            let version = i64::from(first.to_digit(10)?);
            (line.chars().all(|c| c == first) && line.len() == usize::try_from(version).ok()?)
                .then_some(version)
        };
        let (a, b) = (version()?, version()?);
        (lines.next() == Some("") && lines.next().is_none()).then_some((a, b))
    }

    fn span(text: &str, region: i64) -> std::ops::Range<usize> {
        let first = text.find('\n').unwrap();
        if region == 1 {
            0..first
        } else {
            let second = first + 1 + text[first + 1..].find('\n').unwrap();
            first + 1..second
        }
    }

    struct Rig {
        store: DocumentStore,
        workspace: EditorWorkspace,
        view: EditorBufferView,
        document: DocumentId,
        ver: i64,
        /// Each frame's region and the version it replaced, by id.
        frames: BTreeMap<u64, (i64, i64)>,
        /// What the last undo step took: the region it must write, the
        /// version it must write there, the version the other region held,
        /// and whether its frame was blocked (all 0 after a commit).
        ur: i64,
        uv: i64,
        kv: i64,
        ub: i64,
    }

    impl Rig {
        fn new(mutant: Option<HistoryMutant>) -> Self {
            let (store, mut workspace, view, document) = editor("1\n22\n");
            workspace.history_mutant = mutant;
            Self {
                store,
                workspace,
                view,
                document,
                ver: 2,
                frames: BTreeMap::new(),
                ur: 0,
                uv: 0,
                kv: 0,
                ub: 0,
            }
        }

        fn text(&self) -> String {
            self.store.snapshot(self.document).unwrap().text.to_string()
        }

        fn head(&self) -> Seq {
            self.store.snapshot(self.document).unwrap().seq
        }

        fn probe(&self) -> Vec<FrameProbe> {
            self.workspace.frame_probe(self.document)
        }

        fn project(&self) -> State {
            let mut state = State::new();
            let (ta, tb) = regions(&self.text()).unwrap_or((0, 0));
            let probe = self.probe();
            state.insert("ta", ta);
            state.insert("tb", tb);
            state.insert("ver", self.ver);
            state.insert("n", i64::try_from(probe.len()).unwrap());
            for (slot, names) in SLOTS.iter().enumerate() {
                let values = probe.get(slot).map_or([0; 4], |frame| {
                    let (region, old) = self.frames[&frame.id];
                    let origin = match (&frame.origin, frame.undoes) {
                        (Origin::Keyboard { .. }, Some(_)) => 4,
                        (Origin::Keyboard { .. }, None) => 1,
                        (Origin::Agent { .. }, _) => 2,
                        (Origin::Disk, _) => 3,
                        _ => 9,
                    };
                    let blocked = frame.blocked_by.map_or(0, |by| {
                        probe
                            .iter()
                            .position(|other| other.id == by)
                            .map_or(9, |at| i64::try_from(at).unwrap() + 1)
                    });
                    [region, origin, old, blocked]
                });
                for (name, value) in names.iter().zip(values) {
                    state.insert(name, value);
                }
            }
            state.insert("ur", self.ur);
            state.insert("uv", self.uv);
            state.insert("kv", self.kv);
            state.insert("ub", self.ub);
            state
        }

        /// Whether `op` is in the model's box here (a commit needs a fresh
        /// version and a free slot; an agent undo pushes a frame).
        fn in_box(&self, op: Op) -> bool {
            let room = self.probe().len() < 3;
            match op {
                Op::Commit { .. } => room && self.ver < MAX_VER,
                Op::UndoAgent => room,
                Op::Undo => true,
            }
        }

        /// Run `op`; whether the real code took it (a refusal changes nothing).
        fn run(&mut self, op: Op) -> bool {
            let before = self.text();
            let held = regions(&before);
            match op {
                Op::Commit { origin, region } => {
                    let version = self.ver + 1;
                    let replaced = held.map_or(0, |(a, b)| if region == 1 { a } else { b });
                    let range = span(&before, region);
                    let insert = spelled(version);
                    match origin {
                        1 => {
                            self.view.selections = vec![Selection {
                                anchor: range.start,
                                head: range.end,
                            }];
                            self.workspace
                                .insert_text(&mut self.store, &mut self.view, &insert)
                                .unwrap();
                        }
                        2 => {
                            let request = Request {
                                edits: vec![TextEdit { range, insert }],
                                origin: Origin::Agent {
                                    principal: "rig".to_string(),
                                    label: format!("v{version}"),
                                },
                            };
                            let head = self.head();
                            let (seq, deltas) = self
                                .workspace
                                .apply(&mut self.store, self.document, head, request)
                                .unwrap();
                            self.view.observe_external(seq, &deltas);
                        }
                        _ => {
                            let mut disk = before.clone();
                            disk.replace_range(range, &insert);
                            let head = self.head();
                            let (seq, deltas) = self
                                .workspace
                                .replace_text(
                                    &mut self.store,
                                    self.document,
                                    head,
                                    &disk,
                                    Origin::Disk,
                                )
                                .unwrap();
                            self.view.observe_external(seq, &deltas);
                        }
                    }
                    self.ver = version;
                    let newest = self.probe().last().unwrap().id;
                    self.frames.insert(newest, (region, replaced));
                    (self.ur, self.uv, self.kv, self.ub) = (0, 0, 0, 0);
                    true
                }
                Op::Undo | Op::UndoAgent => {
                    let probe = self.probe();
                    let target = if op == Op::Undo {
                        probe.last().cloned()
                    } else {
                        probe
                            .iter()
                            .rev()
                            .find(|frame| {
                                matches!(frame.origin, Origin::Agent { .. })
                                    && !probe.iter().any(|undo| {
                                        Some(undo.id) == frame.blocked_by
                                            && undo.undoes == Some(frame.id)
                                    })
                            })
                            .cloned()
                    };
                    let command = if op == Op::Undo {
                        EditorCommand::Undo
                    } else {
                        EditorCommand::UndoAgentEdit
                    };
                    if self
                        .workspace
                        .execute(&mut self.store, &mut self.view, command)
                        .is_err()
                    {
                        assert_eq!(self.text(), before, "a refused undo changes nothing");
                        return false;
                    }
                    let target = target.expect("an undo that ran had a frame to undo");
                    let (region, old) = self.frames[&target.id];
                    self.ur = region;
                    self.uv = old;
                    self.kv = held.map_or(0, |(a, b)| if region == 1 { b } else { a });
                    self.ub = i64::from(target.blocked_by.is_some());
                    if op == Op::UndoAgent {
                        let replaced = held.map_or(0, |(a, b)| if region == 1 { a } else { b });
                        let newest = self.probe().last().unwrap().id;
                        self.frames.insert(newest, (region, replaced));
                    }
                    true
                }
            }
        }
    }

    /// Every sequence of up to [`DEPTH`] operations inside the model's box,
    /// each as the states its steps projected to (a refused step adds none),
    /// with `refused` run on each refused operation and the state it met.
    fn sequences(
        mutant: Option<HistoryMutant>,
        mut refused: impl FnMut(Op, &State),
    ) -> Vec<(Vec<Op>, Vec<State>)> {
        let mut out = Vec::new();
        let mut stack: Vec<Vec<Op>> = vec![Vec::new()];
        while let Some(ops) = stack.pop() {
            let mut rig = Rig::new(mutant);
            let mut states = vec![rig.project()];
            for &op in &ops {
                if rig.run(op) {
                    states.push(rig.project());
                } else {
                    refused(op, states.last().unwrap());
                }
            }
            if ops.len() < DEPTH {
                for op in OPS {
                    if rig.in_box(op) {
                        let mut next = ops.clone();
                        next.push(op);
                        stack.push(next);
                    }
                }
            }
            out.push((ops, states));
        }
        out
    }

    fn refused_step(model: &Model, states: &[State]) -> Option<usize> {
        states
            .windows(2)
            .position(|pair| admits(model, &pair[0], &pair[1]).is_none())
    }

    #[test]
    fn conformance_refines_amacs_undo() {
        let model = amacs_undo_model();
        assert_eq!(Rig::new(None).project(), model.init_state());
        let mut actions = BTreeSet::new();
        let runs = sequences(None, |op, state| {
            let actions: &[&str] = if op == Op::Undo {
                &["UndoAt1", "UndoAt2", "UndoAt3"]
            } else {
                &["UndoAgentAt1", "UndoAgentAt2"]
            };
            for action in actions {
                assert!(
                    !model.action_enabled(action, state),
                    "the real code refused {op:?}, which the model takes ({action}) at {state:?}"
                );
            }
        });
        assert!(runs.len() > 1000, "only {} sequences ran", runs.len());
        for (ops, states) in &runs {
            for pair in states.windows(2) {
                let action = admits(&model, &pair[0], &pair[1]);
                assert!(
                    action.is_some(),
                    "{ops:?}: the model admits no step {:?} -> {:?}",
                    pair[0],
                    pair[1]
                );
                actions.extend(action);
            }
            for law in [
                "UndoInvertsLatestUnblocked",
                "NoSilentDrop",
                "BlockedFrameNeverApplied",
                "RebaseKeepsDisjointFrames",
                "BlockersAreLater",
            ] {
                assert!(
                    states.iter().all(|state| model.check_invariant(law, state)),
                    "{ops:?} breaks {law}"
                );
            }
        }
        for action in model.actions.iter().map(|action| action.name) {
            assert!(actions.contains(action), "no sequence reached {action}");
        }

        let rigs = [
            (HistoryMutant::SkipRebase, "RebaseKeepsDisjointFrames"),
            (HistoryMutant::ApplyBlocked, "BlockedFrameNeverApplied"),
            (HistoryMutant::SnapshotRestore, "NoSilentDrop"),
            (HistoryMutant::NoUnblock, "BlockersAreLater"),
        ];
        for (mutant, law) in rigs {
            let mut refused = 0;
            let mut caught = 0;
            for (_, states) in sequences(Some(mutant), |_, _| {}) {
                if refused_step(&model, &states).is_some() {
                    refused += 1;
                    if states
                        .iter()
                        .any(|state| !model.check_invariant(law, state))
                    {
                        caught += 1;
                    }
                }
            }
            assert!(refused > 0, "the {mutant:?} rig passed as a refinement");
            assert!(caught > 0, "the {mutant:?} rig never broke {law}");
        }
    }
}
