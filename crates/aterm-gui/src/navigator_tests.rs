// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use super::*;
use crate::native_ui::{LogicalRect, UiContent};
use crate::task_record::TaskState;

fn row(session: u64, creator: Option<u64>) -> NavRow {
    NavRow {
        session,
        title: format!("s{session}"),
        agent: Some("claude".to_owned()),
        word: Word::Quiet,
        line: Some(format!("said {session}")),
        moved_ms: 0,
        creator,
    }
}

fn rows(all: &[(u64, Option<u64>)]) -> HashMap<u64, Arc<NavRow>> {
    all.iter()
        .map(|(session, creator)| (*session, Arc::new(row(*session, *creator))))
        .collect()
}

/// `(session, depth, descendants)` of each entry, in the tree's order.
fn shape(tree: &NavTree) -> Vec<(u64, u16, u32)> {
    tree.entries
        .iter()
        .map(|entry| (entry.row.session, entry.depth, entry.under))
        .collect()
}

fn fold<'a>(
    person_in: Option<u64>,
    opened: &'a BTreeSet<u64>,
    closed: &'a BTreeSet<u64>,
    filter: &'a str,
) -> Fold<'a> {
    Fold {
        person_in,
        opened,
        closed,
        filter,
    }
}

/// The sessions a view lists, in order.
fn sessions(tree: &NavTree, listed: &[Listed]) -> Vec<u64> {
    listed
        .iter()
        .map(|row| tree.entries[row.at].row.session)
        .collect()
}

/// THE FIVE WORDS, FIRST MATCH IN THE ORDER stuck, ended, working, quiet, idle (decision ST-D23):
/// a task that stalled, a task that failed with no turn in progress, or anything stopped to ask
/// a person (the published need) is `stuck`; an agent whose program is gone is `ended`; a turn
/// in progress or a shell's running command is `working`; an agent or a task at rest is `quiet`;
/// a person's shell at its prompt is `idle`. A box the harness answers itself (`question`,
/// `prompt`, a wall, with no published need) is no error; a task a person took off Autopilot
/// did not fail; a turn in progress outranks a failed task. NEGATIVE CONTROLS: the same facts
/// without the ask, the take-over, the turn or the running command read their own word.
#[test]
fn a_row_reads_one_of_five_words_first_match_in_order() {
    use TaskState::{Done, Exited, Failed, Stalled, Working};
    // (task, taken off Autopilot, asks, agent word, shell running, word)
    for (task, taken_off, asks, agent_word, running, word) in [
        (Some(Failed), false, false, "idle", false, Word::Stuck),
        (Some(Stalled), false, false, "busy", false, Word::Stuck),
        (Some(Working), false, true, "busy", false, Word::Stuck),
        (None, false, true, "-", false, Word::Stuck),
        (None, false, true, "question", false, Word::Stuck),
        (Some(Exited), false, true, "-", false, Word::Stuck),
        (Some(Exited), false, false, "-", false, Word::Ended),
        (Some(Exited), false, false, "busy", false, Word::Ended),
        (Some(Working), false, false, "idle", false, Word::Working),
        (
            Some(Working),
            false,
            false,
            "question",
            false,
            Word::Working,
        ),
        (Some(Done), false, false, "busy", false, Word::Working),
        (None, false, false, "busy", false, Word::Working),
        // A turn in progress outranks a task that failed; at rest the failure reads.
        (Some(Failed), false, false, "busy", false, Word::Working),
        // Taken off Autopilot: the conversation's own word, never the error.
        (Some(Failed), true, false, "busy", false, Word::Working),
        (Some(Failed), true, false, "idle", false, Word::Quiet),
        // A shell's running command; at its prompt it is idle.
        (None, false, false, "-", true, Word::Working),
        (None, false, false, "-", false, Word::Idle),
        // An agent is the shell's running command for as long as it lives: its word is its own.
        (None, false, false, "idle", true, Word::Quiet),
        (Some(Done), false, false, "idle", false, Word::Quiet),
        (Some(Done), false, false, "-", false, Word::Quiet),
        (None, false, false, "idle", false, Word::Quiet),
        (None, false, false, "unknown", false, Word::Quiet),
        // A box with no published need is one the harness answers: no error.
        (None, false, false, "question", false, Word::Quiet),
        (None, false, false, "prompt", false, Word::Quiet),
        (None, false, false, "wall:auth", false, Word::Quiet),
    ] {
        let facts = WordFacts {
            task,
            taken_off,
            asks,
            agent_word,
            running,
        };
        assert_eq!(word_of(&facts), word, "{facts:?}");
    }
}

#[test]
fn the_five_words_are_lower_case_and_there_is_no_other() {
    let words: Vec<&str> = Word::ALL.iter().map(|word| word.word()).collect();
    assert_eq!(words, ["working", "quiet", "idle", "ended", "stuck"]);
    for word in words {
        assert_eq!(word, word.to_lowercase());
        assert!(
            !word.contains(' ') && !word.contains(':'),
            "one word: {word}"
        );
    }
}

/// A PARENT'S FOLD LINE IS A COUNT AND NOTHING ELSE: no state, no need, no mark.
#[test]
fn the_fold_line_is_a_count_only() {
    assert_eq!(under_words(1), "1 under");
    assert_eq!(under_words(3), "3 under");
    assert_eq!(under_words(120), "120 under");
}

#[test]
fn the_time_since_a_stream_moved_is_one_coarse_word() {
    const MIN: u64 = 60_000;
    let now = 1_700_000_000_000;
    assert_eq!(age_words(now, now), "now");
    assert_eq!(age_words(now, now - 59_999), "now");
    assert_eq!(age_words(now, now - 12 * MIN), "12m");
    assert_eq!(age_words(now, now - 3 * 60 * MIN), "3h");
    assert_eq!(age_words(now, now - 2 * 24 * 60 * MIN), "2d");
    assert_eq!(
        age_words(now, now + 5_000),
        "now",
        "a clock a minute behind"
    );
    assert_eq!(
        age_words(now, 0),
        "",
        "a stream that never moved has no age"
    );
}

/// WHEN A STREAM MOVED IS NOT PART OF A ROW'S EQUALITY: a row that printed again is the same row
/// until one of its words differs, so the list does not repaint for each line of output.
#[test]
fn a_row_that_only_moved_is_the_same_row() {
    let held = row(1, None);
    let mut moved = held.clone();
    moved.moved_ms = 1_700_000_000_000;
    assert_eq!(held, moved);
    let mut said = held.clone();
    said.line = Some("something new".to_owned());
    assert_ne!(held, said);
    let mut stuck = held.clone();
    stuck.word = Word::Stuck;
    assert_ne!(held, stuck);
}

/// THE ORDER (design §3): pinned sessions in layout order, each with the unpinned sessions
/// created under it through live creators; then every session no pinned session leads to, each
/// with its own. A pinned session with a live creator stays at its own tab's place.
#[test]
fn pinned_rows_come_first_each_with_its_own_then_the_machines() {
    // 1, 2, 3 pinned (3 was created by 1 and stays where its tab is). 10 under 1, 11 under 10,
    // 12 from nobody, 13 from a creator that is gone (no live row: `None`), 14 under 12.
    let all = rows(&[
        (1, None),
        (2, None),
        (3, Some(1)),
        (10, Some(1)),
        (11, Some(10)),
        (12, None),
        (13, None),
        (14, Some(12)),
    ]);
    let tree = arrange("m11", &[1, 2, 3], &[10, 11, 12, 13, 14], &all, None);
    assert_eq!(tree.machine, "m11");
    assert_eq!(
        shape(&tree),
        [
            (1, 0, 2),
            (10, 1, 1),
            (11, 2, 0),
            (2, 0, 0),
            (3, 0, 0),
            (12, 0, 1),
            (14, 1, 0),
            (13, 0, 0),
        ]
    );
    let groups: Vec<Group> = tree.entries.iter().map(|entry| entry.group).collect();
    assert_eq!(
        groups,
        [
            Group::Pinned,
            Group::Pinned,
            Group::Pinned,
            Group::Pinned,
            Group::Pinned,
            Group::Machine,
            Group::Machine,
            Group::Machine,
        ]
    );
    assert_eq!(tree.index_of(14), Some(6));
    assert_eq!(tree.entries[6].parent, Some(5));
    assert_eq!(tree.entries[0].parent, None);
}

#[test]
fn a_cycle_of_creators_breaks_and_each_session_is_listed_once() {
    // 10 and 11 name each other; 12 names itself; 13 names a session with no row.
    let all = rows(&[
        (1, None),
        (10, Some(11)),
        (11, Some(10)),
        (12, Some(12)),
        (13, Some(99)),
    ]);
    let tree = arrange("m11", &[1], &[10, 11, 12, 13], &all, None);
    let mut listed: Vec<u64> = tree.sessions();
    assert_eq!(listed.len(), 5);
    listed.sort_unstable();
    assert_eq!(listed, [1, 10, 11, 12, 13], "each once");
    assert_eq!(
        shape(&tree),
        [(1, 0, 0), (12, 0, 0), (13, 0, 0), (10, 0, 1), (11, 1, 0)],
        "the cycle breaks at its first session, after the roots"
    );
    // A session named in a walk with no row is not listed.
    let tree = arrange("m11", &[1, 7], &[8], &all, None);
    assert_eq!(tree.sessions(), [1]);
}

#[test]
fn steady_keeps_survivors_in_held_order_and_appends_newcomers() {
    assert_eq!(steady(&[1, 2, 3], &[3, 1, 2, 4]), [1, 2, 3, 4]);
    assert_eq!(steady(&[1, 2, 3], &[3, 2]), [2, 3]);
    assert_eq!(steady(&[], &[5, 4]), [5, 4]);
    assert_eq!(steady(&[9], &[]), Vec::<u64>::new());
}

/// WHILE THE LIST HAS THE KEYS ITS SIBLINGS KEEP THEIR ORDER: an arrangement over the held tree
/// keeps each group's rows where they were, newcomers after them. NEGATIVE CONTROL: without a
/// held tree the same walk is in the walk's order.
#[test]
fn an_arrangement_over_a_held_tree_keeps_each_siblings_order() {
    let all = rows(&[
        (1, None),
        (2, None),
        (3, None),
        (10, Some(1)),
        (11, Some(1)),
    ]);
    let held = arrange("m11", &[1, 2, 3], &[10, 11], &all, None);
    let more = rows(&[
        (1, None),
        (2, None),
        (3, None),
        (4, None),
        (10, Some(1)),
        (11, Some(1)),
        (12, Some(1)),
    ]);
    let kept = arrange("m11", &[3, 4, 1, 2], &[12, 11, 10], &more, Some(&held));
    assert_eq!(kept.sessions(), [1, 10, 11, 12, 2, 3, 4]);
    let fresh = arrange("m11", &[3, 4, 1, 2], &[12, 11, 10], &more, None);
    assert_eq!(fresh.sessions(), [3, 4, 1, 12, 11, 10, 2]);
}

/// A ROW WITH SESSIONS UNDER IT STARTS CLOSED, EXCEPT THE ROW OF THE SESSION THE PERSON IS IN and
/// each row above it. A closed row lists none of its own and says how many it holds: every
/// descendant.
#[test]
fn a_parent_starts_closed_except_on_the_way_to_the_persons_session() {
    let all = rows(&[
        (1, None),
        (2, None),
        (10, Some(1)),
        (11, Some(10)),
        (20, Some(2)),
    ]);
    let tree = arrange("m11", &[1, 2], &[10, 11, 20], &all, None);
    let none = BTreeSet::new();

    let closed = listed(&tree, &fold(None, &none, &none, ""));
    assert_eq!(sessions(&tree, &closed), [1, 2]);
    assert_eq!(
        closed
            .iter()
            .map(|row| (row.open, row.under))
            .collect::<Vec<_>>(),
        [(false, 2), (false, 1)]
    );

    // The person is in 10: 1 (above it) and 10 itself are open, 2 is not.
    let inside = listed(&tree, &fold(Some(10), &none, &none, ""));
    assert_eq!(sessions(&tree, &inside), [1, 10, 11, 2]);
    assert!(inside[0].open && inside[1].open && !inside[3].open);
    assert_eq!(
        inside[2].under, 0,
        "a row with nothing under it has no fold"
    );

    // A person's Step Out stands over the default, and a Step In opens what was closed.
    let shut: BTreeSet<u64> = [1].into();
    let open: BTreeSet<u64> = [2].into();
    let chosen = listed(&tree, &fold(Some(10), &open, &shut, ""));
    assert_eq!(sessions(&tree, &chosen), [1, 2, 20]);
}

/// THE FILTER KEEPS A MATCH AND THE ROWS ABOVE IT, SHOWN OPEN, whatever was folded: a match on
/// the title, the agent, the word or the latest words, in any case.
#[test]
fn the_filter_keeps_a_match_with_the_rows_above_it_open() {
    let mut all = rows(&[(1, None), (2, None), (10, Some(1)), (11, Some(10))]);
    let mut deep = row(11, Some(10));
    deep.line = Some("Reading the Chrome notes".to_owned());
    all.insert(11, Arc::new(deep));
    let mut stuck = row(2, None);
    stuck.word = Word::Stuck;
    stuck.agent = Some("codex".to_owned());
    all.insert(2, Arc::new(stuck));
    let tree = arrange("m11", &[1, 2], &[10, 11], &all, None);
    let none = BTreeSet::new();
    let shut: BTreeSet<u64> = [1, 10].into();

    let found = listed(&tree, &fold(None, &none, &shut, "chrome"));
    assert_eq!(sessions(&tree, &found), [1, 10, 11]);
    assert!(found[0].open && found[1].open);
    assert_eq!(
        sessions(&tree, &listed(&tree, &fold(None, &none, &none, "STUCK"))),
        [2]
    );
    assert_eq!(
        sessions(&tree, &listed(&tree, &fold(None, &none, &none, "codex"))),
        [2]
    );
    assert_eq!(
        sessions(&tree, &listed(&tree, &fold(None, &none, &none, "s10"))),
        [1, 10]
    );
    assert!(listed(&tree, &fold(None, &none, &none, "no such words")).is_empty());
}

fn app_of(tree: NavTree, clock_ms: u64) -> NavigatorApp {
    let mut app = NavigatorApp::default();
    app.publish(Arc::new(tree));
    app.set_clock(clock_ms);
    app
}

/// Every painted text of the compiled page, by key.
fn texts(compiled: &crate::native_ui::CompiledUi) -> HashMap<String, String> {
    compiled
        .paint
        .iter()
        .filter_map(|node| match &node.content {
            UiContent::Text(text) => Some((node.key.as_str().to_owned(), text.text.clone())),
            _ => None,
        })
        .collect()
}

/// ONE ROW TEMPLATE AT EVERY WIDTH, IN LOGICAL PIXELS: narrow (a 30-cell split) drops the agent
/// and keeps the time; wide says the agent and the time. Both say the one word, the latest
/// words, and on a closed parent the count. No text is a state phrase with a reason.
#[test]
fn a_row_is_its_title_and_time_over_its_word_and_latest_words_at_both_widths() {
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    const MIN: u64 = 60_000;
    let now = 1_700_000_000_000;
    let mut all = rows(&[(1, None), (10, Some(1))]);
    let mut parent = row(1, None);
    parent.title = "Streams design".to_owned();
    parent.word = Word::Working;
    parent.line = Some("Reading the chrome\nnotes".to_owned());
    parent.moved_ms = now - 41 * MIN;
    all.insert(1, Arc::new(parent));
    let mut stuck = row(2, None);
    stuck.word = Word::Stuck;
    stuck.agent = None;
    stuck.moved_ms = now - 3 * 60 * MIN;
    all.insert(2, Arc::new(stuck));
    let tree = arrange("m11", &[1, 2], &[10], &all, None);
    let app = app_of(tree, now);
    let view = NavigatorViewState::default();

    for (width, meta) in [(216.0, "41m"), (860.0, "claude \u{00b7} 41m")] {
        let compiled = page(&app, &view, width, 600.0)
            .compile(LogicalRect::new(0.0, 0.0, width, 600.0))
            .expect("the page compiles");
        let texts = texts(&compiled);
        assert_eq!(texts["navigator/title"], "m11", "{width}");
        assert_eq!(texts["navigator/item/1/meta"], meta, "{width}");
        assert_eq!(texts["navigator/item/1/word"], "working", "{width}");
        assert_eq!(texts["navigator/item/1/under"], "1 under", "{width}");
        assert_eq!(texts["navigator/item/2/meta"], "3h", "a shell has no agent");
        assert_eq!(texts["navigator/item/2/word"], "stuck");
        assert!(
            !texts.contains_key("navigator/item/2/under"),
            "nothing under"
        );
        assert!(
            !texts.contains_key("navigator/item/10/word"),
            "a closed parent lists none of its own"
        );
        if width > 480.0 {
            assert_eq!(
                texts["navigator/item/1/line"], "Reading the chrome notes",
                "one line, its breaks flattened"
            );
        }
        for (key, text) in &texts {
            assert!(
                !text.contains("Needs you") && !text.contains("need"),
                "{key}: {text}"
            );
            let inside = compiled
                .paint
                .iter()
                .find(|node| node.key.as_str() == key)
                .expect("painted");
            assert!(
                inside.rect.right() <= width + 0.5,
                "{key} is drawn past the pane's edge at {width}"
            );
        }
        // The stuck word is the one text in the danger ink; the calm words share one ink.
        let style_of = |key: &str| {
            compiled
                .paint
                .iter()
                .find_map(|node| match &node.content {
                    UiContent::Text(text) if node.key.as_str() == key => Some(text.style),
                    _ => None,
                })
                .expect("a word")
        };
        assert_eq!(
            style_of("navigator/item/2/word"),
            crate::native_ui::StyleRef::Danger
        );
        assert_eq!(
            style_of("navigator/item/1/word"),
            crate::native_ui::StyleRef::Plain
        );
    }
}

/// HUNDREDS OF SESSIONS PAINT ONE SCREEN: the page paints the same number of nodes for 30 and for
/// 300 sessions in one viewport, and every session still has its control for `inspect`, Tab and
/// the arrows.
#[test]
fn three_hundred_sessions_paint_one_screen() {
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let painted = |count: u64| {
        let pinned: Vec<u64> = (1..=count).collect();
        let all = rows(&pinned.iter().map(|id| (*id, None)).collect::<Vec<_>>());
        let tree = arrange("m11", &pinned, &[], &all, None);
        let app = app_of(tree, 0);
        let view = NavigatorViewState::default();
        let compiled = page(&app, &view, 700.0, 500.0)
            .compile(LogicalRect::new(0.0, 0.0, 700.0, 500.0))
            .expect("the page compiles");
        for id in &pinned {
            assert!(
                compiled
                    .semantic(&crate::native_ui::UiKey::new(format!("navigator/row/{id}")))
                    .is_some(),
                "session {id} of {count} has its control"
            );
        }
        compiled.paint.len()
    };
    assert_eq!(painted(30), painted(300));
}

#[test]
fn an_empty_list_and_an_empty_filter_say_so_in_one_line() {
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let compiled = |app: &NavigatorApp, view: &NavigatorViewState| {
        texts(
            &page(app, view, 700.0, 500.0)
                .compile(LogicalRect::new(0.0, 0.0, 700.0, 500.0))
                .expect("the page compiles"),
        )
    };
    let none = app_of(arrange("m11", &[], &[], &HashMap::new(), None), 0);
    assert_eq!(
        compiled(&none, &NavigatorViewState::default())["navigator/empty"],
        "No live sessions"
    );
    let one = app_of(arrange("m11", &[1], &[], &rows(&[(1, None)]), None), 0);
    let mut view = NavigatorViewState::default();
    view.set_filter("zzz");
    assert_eq!(
        compiled(&one, &view)["navigator/empty"],
        "No sessions match"
    );
}

/// THE MACHINE'S NAME HEADS ITS ROWS OR IS NOT DRAWN, AND A LIST THAT RUNS ON FADES: where the
/// room's edge leaves the heading with less than a peek of its first row under it, the heading
/// is not painted (it keeps its place, so nothing above it moves); and what the edge cuts fades
/// out through the page's bottom margin, so a cut row reads as cut and never as the list's end.
/// NEGATIVE CONTROLS: a page tall enough for the row paints the heading; a list that fits has
/// no fade.
#[test]
fn a_heading_with_no_row_under_it_is_not_drawn_and_a_cut_list_fades() {
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let pinned: Vec<u64> = (1..=4).collect();
    let parked = [10, 11];
    let all = rows(
        &pinned
            .iter()
            .chain(&parked)
            .map(|id| (*id, None))
            .collect::<Vec<_>>(),
    );
    let app = app_of(arrange("m11", &pinned, &parked, &all, None), 0);
    let view = NavigatorViewState::default();
    let width = 300.0;
    let at = |height: f32| {
        page(&app, &view, width, height)
            .compile(LogicalRect::new(0.0, 0.0, width, height))
            .expect("the page compiles")
    };
    let heads = |compiled: &crate::native_ui::CompiledUi| {
        compiled
            .paint
            .iter()
            .any(|node| node.key.as_str() == "navigator/machine")
    };
    let fades = |compiled: &crate::native_ui::CompiledUi| {
        compiled.paint.iter().any(|node| node.fade.is_some())
    };

    let whole = at(900.0);
    assert!(heads(&whole) && !fades(&whole), "NEGATIVE CONTROL: it fits");

    let frame = Frame::of(&app.tree, &view.fold(), width, 900.0, false);
    let first = frame.tops[frame.machine_from.expect("a machine group")];
    let room_to =
        |row_shows: f32| frame.m.pad * 2.0 + frame.m.header_h + ROOT_GAP + first + row_shows;
    // The room ends 8 px into the machine's first row: the heading would stand over a sliver.
    let cut = at(room_to(8.0));
    assert!(!heads(&cut), "no heading with nothing under it");
    assert!(fades(&cut), "the list that runs on fades at its edge");
    assert!(
        texts(&cut)
            .keys()
            .any(|key| key.starts_with("navigator/item/4/")),
        "the last row above the heading is still drawn"
    );
    // A peek of the row is enough to head.
    let peek = at(room_to(crate::native_ui::LIST_PEEK_MIN + 2.0));
    assert!(heads(&peek) && fades(&peek));
}

/// A NEW VIEW SCROLLS ITS SELECTED ROW INTO THE ROOM ONCE IT HAS A SIZE, and not again: the
/// host's first word on the view's size reveals the row; a later one leaves a scroll the person
/// made alone.
#[test]
fn the_first_size_reveals_the_selected_row_and_a_later_one_does_not() {
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let pinned: Vec<u64> = (1..=60).collect();
    let all = rows(&pinned.iter().map(|id| (*id, None)).collect::<Vec<_>>());
    let app = app_of(arrange("m11", &pinned, &[], &all, None), 0);
    let mut view = NavigatorViewState::starting_in(Some(60));
    assert!(
        view.row_span(&app.tree, 60, 300.0, 500.0).is_none(),
        "PRECONDITION"
    );
    assert!(view.lay(&app.tree, 300.0, 500.0), "the list moved");
    assert!(view.row_span(&app.tree, 60, 300.0, 500.0).is_some());
    view.scroll = 0.0;
    assert!(!view.lay(&app.tree, 300.0, 520.0));
    assert!(view.row_span(&app.tree, 60, 300.0, 520.0).is_none());
}
