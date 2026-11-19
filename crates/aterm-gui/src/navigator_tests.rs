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
        shortcut: None,
        title: format!("s{session}"),
        original_topic: None,
        named: false,
        program: Some("claude".to_owned()),
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

/// A recorded operation failure or stall takes precedence over the usual activity word.
/// An active turn can follow a failed task. Taking a task off Autopilot clears its error,
/// a shell's running command reads working and an agent between turns reads quiet.
#[test]
fn a_row_reads_one_of_five_words_first_match_in_order() {
    use TaskState::{Done, Exited, Failed, Stalled, Working};
    // (task, taken off Autopilot, operation has a confirmed obstacle, agent word, shell running, word)
    for (task, taken_off, obstructed, agent_word, running, word) in [
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
            need: obstructed.then_some(aterm_types::needs::NeedWord::Stopped),
            agent_word,
            running,
        };
        assert_eq!(word_of(&facts), word, "{facts:?}");
    }
}

#[test]
fn a_session_can_work_while_an_input_request_is_open() {
    use aterm_types::needs::NeedWord;
    for request in [NeedWord::Ask, NeedWord::Question] {
        let facts = WordFacts {
            task: Some(TaskState::Working),
            taken_off: false,
            need: Some(request),
            agent_word: "busy",
            running: true,
        };
        assert_eq!(word_of(&facts), Word::Working);
        assert_eq!(
            word_of(&WordFacts {
                need: Some(NeedWord::Wall),
                ..facts
            }),
            Word::Stuck
        );
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
    stuck.program = Some("codex".to_owned());
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

#[test]
fn a_long_creation_topic_is_readable_in_details_and_speech_after_a_rename() {
    let (mut app, mut panel) = chart_fixture();
    let mut all = rows(&[(7, None), (8, None)]);
    let first = Arc::make_mut(all.get_mut(&7).unwrap());
    // A path offers no spaces at which to wrap. Preserve every scalar across visual lines.
    let path = "crates/aterm-gui/src/app_input_native_keyboard_boundary_tests.rs::visible_local_panel_digits_select_sessions_from_an_editor_and_a_terminal";
    first.original_topic = Some(path.into());
    first.title = "Test shortcuts".into();
    first.named = true;
    first.shortcut = Some(1);
    first.program = Some("targo".into());
    let task = "Prove that reopening a parked terminal preserves the original Lean command and its output stream after restoring the workspace";
    let second = Arc::make_mut(all.get_mut(&8).unwrap());
    second.original_topic = Some(task.into());
    second.title = "Lean stdout".into();
    second.shortcut = Some(2);
    app.publish(Arc::new(arrange("Local machine", &[7, 8], &[], &all, None)));
    let rect = LogicalRect::new(0., 0., 210., 1400.);
    let render = |panel: &NavigatorViewState| {
        page(&app, panel, rect.width, rect.height)
            .compile(rect)
            .unwrap()
    };
    let compact = render(&panel);
    let row = UiKey::new("navigator/row/7");
    let title = "navigator/item/7/title";
    let origin = "navigator/item/7/origin/";
    assert_eq!(texts(&compact)[title], "Test shortcuts");
    assert!(texts(&compact)["navigator/item/8/title-continued"].ends_with('…'));
    assert!(
        compact
            .semantic(&UiKey::new("navigator/row/8"))
            .unwrap()
            .label
            .contains(task)
    );
    assert!(compact.semantic(&row).unwrap().label.contains(path));
    assert!(
        !compact
            .paint
            .iter()
            .any(|node| node.key.as_str().starts_with(origin))
    );
    panel.chart_details = true;
    let detailed = render(&panel);
    let text = texts(&detailed);
    assert_eq!(text["navigator/item/7/origin-label"], "Original topic");
    let mut restored = String::new();
    let mut lines = 0;
    for i in 0.. {
        let Some(part) = text.get(&format!("{origin}{i}")) else {
            break;
        };
        assert!(!part.is_empty());
        restored.push_str(part);
        lines += 1;
    }
    assert!(lines > 2);
    assert_eq!(restored, path);
    let words: Vec<_> = (0..)
        .map_while(|i| text.get(&format!("navigator/item/8/origin/{i}")))
        .map(String::as_str)
        .collect();
    assert_eq!(words.join(" "), task);
    capture_panel(
        &detailed,
        210,
        850,
        "session-topic-details.png",
        crate::native_appearance::forced_chrome_theme(aterm_render::Theme::default(), false),
    );
}

#[test]
fn the_session_panel_keeps_visible_and_hidden_order_independent_of_shortcuts_and_folds() {
    let mut all = rows(&[(8, None), (19, Some(8)), (21, None)]);
    Arc::make_mut(all.get_mut(&8).unwrap()).shortcut = Some(2);
    Arc::make_mut(all.get_mut(&19).unwrap()).shortcut = Some(1);
    let tree = arrange("host", &[8], &[19, 21], &all, None);
    let mut standalone = NavigatorViewState::starting_in(None);
    standalone.close(8);
    assert_eq!(sessions(&tree, &standalone.listing(&tree)), [8, 21]);
    standalone.set_filter("s21");
    assert_eq!(sessions(&tree, &standalone.listing(&tree)), [21]);

    let mut panel = NavigatorViewState::in_dock(None);
    panel.sessions = DockSessions {
        order: vec![8, 21, 19],
        shown: 1,
        numbered: true,
        ..DockSessions::default()
    };
    let listed = panel.listing(&tree);
    assert_eq!(sessions(&tree, &listed), [8, 21, 19]);
    assert!(listed.iter().all(|row| row.under == 0));
    let frame = Frame::of(&tree, &panel, 240.0, 200.0);
    assert_eq!(frame.machine_from, Some(1));
    assert_eq!(frame.top_of(&tree, 19), Some(frame.tops[2]));
}

#[test]
fn the_session_panel_paints_and_speaks_its_assigned_shortcuts() {
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let mut all = rows(&[(25, None), (81, None)]);
    Arc::make_mut(all.get_mut(&25).unwrap()).shortcut = Some(2);
    let app = app_of(arrange("host", &[25, 81], &[], &all, None), 0);
    let panel = |numbered| {
        let mut view = NavigatorViewState::in_dock(None);
        view.sessions = DockSessions {
            order: vec![25, 81],
            shown: 2,
            numbered,
            ..DockSessions::default()
        };
        view
    };
    let shortcut = if cfg!(target_os = "macos") {
        "⌘2"
    } else {
        "Ctrl+Shift+2"
    };
    let spoken = if cfg!(target_os = "macos") {
        "Command 2"
    } else {
        "Control Shift 2"
    };
    for (view, numbered) in [
        (&panel(true), true),
        (&panel(false), false),
        (&NavigatorViewState::starting_in(None), false),
    ] {
        let compiled = page(&app, view, 230.0, 400.0)
            .compile(LogicalRect::new(0.0, 0.0, 230.0, 400.0))
            .unwrap();
        let labels = texts(&compiled);
        assert_eq!(
            labels.get("navigator/item/25/shortcut").map(String::as_str),
            numbered.then_some(shortcut)
        );
        assert_eq!(
            compiled
                .semantic(&UiKey::new("navigator/row/25"))
                .unwrap()
                .label
                .contains(spoken),
            numbered
        );
        assert!(!labels.contains_key("navigator/item/81/shortcut"));
    }
}

fn capture_panel(
    ui: &crate::native_ui::CompiledUi,
    width: u32,
    height: u32,
    filename: &str,
    theme: aterm_render::Theme,
) {
    let Some(dir) = std::env::var_os("ATERM_SESSION_PANEL_GALLERY") else {
        return;
    };
    let tray = ui.tray(theme, 13.);
    let [_, r, g, b] = theme.bg.to_be_bytes();
    let (pixels, w, h) =
        crate::tray_raster::rasterize_tray(&tray.prims, width, height, 2., [r, g, b, 255]);
    let png = crate::app_introspect::encode_rgba8_png(&pixels, w, h).unwrap();
    std::fs::write(std::path::Path::new(&dir).join(filename), png).unwrap();
}

fn chart_fixture() -> (NavigatorApp, NavigatorViewState) {
    use aterm_vitals::pane_cpu::Reading;
    use resources::Kind::{Cpu, Input, Output};
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let mut all = rows(&[(7, None), (8, None)]);
    for (id, title, short) in [(7, "Compile Trust", 1), (8, "Check Lean proofs", 2)] {
        let row = Arc::make_mut(all.get_mut(&id).unwrap());
        row.title = title.into();
        row.shortcut = Some(short);
        row.word = Word::Working;
        row.program = Some(if id == 7 { "codex" } else { "lean" }.into());
        row.line = Some(
            if id == 7 {
                "Building trustc"
            } else {
                "Checking arithmetic lemmas"
            }
            .into(),
        );
    }
    let mut app = app_of(arrange("Local machine", &[7, 7], &[8], &all, None), 0);
    let now = 65_000_000_000;
    app.resources_at_ns = now;
    for (id, rate) in [(7, 0.02), (8, 1.8)] {
        let mut r = resources::Readings::default();
        r.cpu_read = Some(Reading {
            at_ns: now - 1_000_000_000,
            window_ns: 2_000_000_000,
            cores: rate,
        });
        r.traces[Cpu as usize].values[0] = Some(0.);
        r.traces[Cpu as usize].values[11] = Some(rate);
        r.traces[Input as usize].values[11] = Some(10.);
        r.traces[Output as usize].values[11] = Some(200.);
        if id == 8 {
            r.traces[Cpu as usize].values =
                [0., 0.18, 0.45, 0.9, 1.8, 1.6, 1.2, 0., 0.9, 1.2, 1.4, rate].map(Some);
            r.traces[Cpu as usize].values[7] = None;
            r.traces[Input as usize].values =
                [0., 2., 15., 5., 0., 24., 50., 10., 2., 55., 20., 10.].map(Some);
            r.traces[Output as usize].values = [
                0., 24., 52., 120., 200., 178., 155., 48., 20., 85., 132., 200.,
            ]
            .map(Some);
        }
        for (kind, value) in [(Cpu, rate), (Input, 10.), (Output, 200.)] {
            r.traces[kind as usize].latest = Some(value);
        }
        app.resources.insert(id, r);
    }
    let mut panel = NavigatorViewState::in_dock(None);
    panel.sessions = DockSessions {
        order: vec![7, 8],
        shown: 2,
        numbered: true,
        ..Default::default()
    };
    (app, panel)
}

#[test]
fn local_panel_cpu_stream_narrow_rows_plot_and_describe_the_same_minute() {
    use crate::native_ui::AuditedCustomNode;
    let (app, panel) = chart_fixture();
    let rect = LogicalRect::new(0., 0., 246., 390.);
    let compiled = page(&app, &panel, rect.width, rect.height)
        .compile(rect)
        .unwrap();
    let charts: HashMap<_, _> = compiled
        .paint
        .iter()
        .filter_map(|node| match &node.content {
            UiContent::Custom(AuditedCustomNode {
                audit_id: resources::AUDIT,
                paint: Some(code),
                ..
            }) => {
                let visible = node.clip.intersect(node.rect).unwrap();
                assert!(
                    (visible.width - node.rect.width).abs() < 0.01
                        && (visible.height - node.rect.height).abs() < 0.01,
                    "the whole trace is visible at this size"
                );
                Some((node.key.as_str(), code.as_str()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        charts.len(),
        16,
        "a mirrored session still has one set of traces"
    );
    assert_eq!(charts["navigator/item/7/CPU/history"], "A----------B");
    assert_eq!(charts["navigator/item/8/CPU/history"], "ACGMYVQ-MQTY");
    assert_eq!(charts["navigator/item/7/In/history"], "-----------B");
    assert_eq!(charts["navigator/item/7/Out/history"], "-----------Y");
    let labels = texts(&compiled);
    assert_eq!(labels["navigator/item/7/title"], "Compile Trust");
    assert_eq!(labels["navigator/item/7/CPU/value"], "0.02c");
    assert_eq!(labels["navigator/item/8/CPU/value"], "1.8c");
    let ax = compiled
        .semantic(&UiKey::new("navigator/row/7"))
        .unwrap()
        .description
        .as_deref()
        .unwrap();
    for fragment in [
        "Twelve closed five-second buckets, oldest first",
        "CPU 0.02 reported cores, scale 0 to 1.8 reported cores, 2 of 12 observed",
        "0, gap, gap, gap, gap, gap, gap, gap, gap, gap, gap, 0.02",
        "Terminal input 10 bytes per second, scale 0 to 200 bytes per second",
        "Terminal output 200 bytes per second, scale 0 to 200 bytes per second",
        "1.0 seconds ago over 2.00 seconds",
    ] {
        assert!(ax.contains(fragment), "{ax}");
    }
    capture_panel(
        &compiled,
        246,
        390,
        "narrow-session-panel.png",
        aterm_render::Theme::default(),
    );
    let normal = page(
        &app,
        &NavigatorViewState::default(),
        rect.width,
        rect.height,
    )
    .compile(rect)
    .unwrap();
    assert!(
        !normal
            .paint
            .iter()
            .any(|p| matches!(&p.content, UiContent::Custom(c) if c.audit_id == resources::AUDIT))
    );
}

#[test]
fn local_panel_mem_disk_tok_two_sessions_share_scales_and_keep_directions_and_coverage() {
    use crate::native_ui::AuditedCustomNode;
    use resources::Kind::{DiskRead, DiskWrite, Memory, TokenIn, TokenOut};
    let (mut app, mut panel) = chart_fixture();
    let now = app.resources_at_ns;
    for (sid, mib, read, write, tokens) in [(7, 64., 0., None, 2.), (8, 256., 512., Some(256.), 8.)]
    {
        let r = app.resources.get_mut(&sid).unwrap();
        r.process_read = Some(aterm_vitals::pane_resources::Reading {
            at_ns: now - 1_000_000_000,
            footprint: Some((mib * 1_048_576.) as u64),
            footprint_unread: u32::from(sid == 7),
            members: 3,
            membership_complete: true,
            read_bps: Some(read),
            write_bps: write,
            window_ns: Some(2_000_000_000),
        });
        r.token_read = Some(aterm_vitals::pane_tokens::Reading {
            at_ns: now - 2_000_000_000,
            collected_ns: now - 1_000_000_000,
            window_ns: 5_000_000_000,
            input_per_s: None,
            output_per_s: Some(tokens),
        });
        for (kind, value) in [
            (Memory, Some(mib)),
            (DiskRead, Some(read)),
            (DiskWrite, write),
            (TokenIn, None),
            (TokenOut, Some(tokens)),
        ] {
            let trace = &mut r.traces[kind as usize];
            trace.latest = value;
            trace.values[11] = value;
        }
        r.memory_floors[11] = sid == 7;
        if sid == 8 {
            r.traces[Memory as usize].values[0..4].copy_from_slice(&[
                Some(16.),
                Some(32.),
                Some(96.),
                Some(128.),
            ]);
            r.traces[DiskRead as usize].values[6..10].copy_from_slice(&[
                Some(0.),
                Some(256.),
                Some(384.),
                Some(64.),
            ]);
            r.traces[TokenOut as usize].values[7..10].copy_from_slice(&[
                Some(0.),
                Some(4.),
                Some(2.),
            ]);
        }
    }
    for width in [210, 246, 480] {
        let rect = LogicalRect::new(0., 0., width as f32, 420.);
        let ui = page(&app, &panel, rect.width, rect.height)
            .compile(rect)
            .unwrap();
        let text = texts(&ui);
        assert_eq!(text["navigator/item/7/Mem/value"], "≥64MiB");
        assert_eq!(text["navigator/item/7/DiskRead/value"], "0B/s");
        assert_eq!(text["navigator/item/7/DiskWrite/value"], "—");
        assert_eq!(text["navigator/item/7/TokenIn/value"], "—");
        assert_eq!(text["navigator/item/7/TokenOut/value"], "2/s");
        let ax = ui
            .semantic(&UiKey::new("navigator/row/7"))
            .unwrap()
            .description
            .as_deref()
            .unwrap();
        for words in [
            "Memory footprint at least 64 MiB, scale 0 to 256 MiB",
            "Footprints unread for 1 of 3 observed processes",
            "Process sweep 1.0 seconds ago",
            "Disk read 0 bytes per second, scale 0 to 512",
            "Disk write unavailable",
            "Model input unavailable",
            "Model output 2 tokens per second, scale 0 to 8",
            "Last token interval ended 2.0 seconds ago and covered 5.00 seconds. Collected 1.0 seconds ago",
        ] {
            assert!(ax.contains(words), "{ax}");
        }
        let charts: HashMap<_, _> = ui
            .paint
            .iter()
            .filter_map(|n| match &n.content {
                UiContent::Custom(AuditedCustomNode {
                    audit_id: resources::AUDIT,
                    paint: Some(c),
                    ..
                }) => {
                    let shown = n.clip.intersect(n.rect).unwrap();
                    assert!(
                        (shown.width - n.rect.width).abs() < 0.01
                            && (shown.height - n.rect.height).abs() < 0.01,
                        "clipped plot {} at {width}",
                        n.key.as_str()
                    );
                    Some((n.key.as_str(), c.as_str()))
                }
                _ => None,
            })
            .collect();
        assert_eq!(charts.len(), 16);
        assert_eq!(charts["navigator/item/7/Mem/history"], "-----------g");
        assert_eq!(
            charts["navigator/item/8/Mem/history"].chars().last(),
            Some('Y')
        );
        assert_eq!(charts["navigator/item/7/DiskRead/history"], "-----------A");
        assert_eq!(charts["navigator/item/7/DiskWrite/history"], "------------");
        assert_eq!(charts["navigator/item/7/TokenOut/history"], "-----------G");
        capture_panel(
            &ui,
            width,
            420,
            &format!("session-resources-{width}.png"),
            aterm_render::Theme::default(),
        );
    }
    let first = UiKey::new("navigator/row/7");
    app.act(&mut panel, AppEvent::FocusChanged(Some(first.clone())));
    app.act(&mut panel, AppEvent::StepFocus(-1));
    assert_eq!(panel.common.last_focus, Some(UiKey::new(CHART_DETAILS)));
    app.act(&mut panel, AppEvent::TextInput(TextInputEvent::Submit));
    let rect = LogicalRect::new(0., 0., 210., 680.);
    let detailed = page(&app, &panel, rect.width, rect.height)
        .compile(rect)
        .unwrap();
    let text = texts(&detailed);
    assert_eq!(text["navigator/item/7/Mem/range"], "0–256MiB");
    assert_eq!(text["navigator/item/7/Mem/collected"], "1s ago");
    assert_eq!(text["navigator/item/7/DiskRead/range"], "0–512B/s");
    assert_eq!(text["navigator/item/7/TokenOut/range"], "0–8/s");
    assert_eq!(text["navigator/item/7/TokenOut/collected"], "1s ago");
    assert_eq!(text["navigator/item/7/TokenOut/value"], "2/s");
    capture_panel(
        &detailed,
        210,
        680,
        "session-resource-details.png",
        crate::native_appearance::forced_chrome_theme(aterm_render::Theme::default(), false),
    );
    let tokens = &mut app.resources.get_mut(&7).unwrap().traces[TokenOut as usize];
    tokens.latest = None;
    tokens.values = Default::default();
    app.resources
        .get_mut(&7)
        .unwrap()
        .token_read
        .as_mut()
        .unwrap()
        .output_per_s = Some(200.);
    let older = page(&app, &panel, rect.width, rect.height)
        .compile(rect)
        .unwrap();
    let text = texts(&older);
    assert_eq!(text["navigator/item/7/TokenOut/value"], "—");
    assert_eq!(text["navigator/item/7/TokenOut/interval"], "Last 200/s");
    assert_eq!(text["navigator/item/7/TokenOut/range"], "0–8/s");
    panel.lay(&app.tree, 210., 150.);
    panel.select(8, &app.tree);
    let frame = panel.frame(&app.tree).unwrap();
    assert_eq!(panel.scroll, frame.top_of(&app.tree, 8).unwrap());
    panel.lay(&app.tree, 210., 680.);
    app.act(
        &mut panel,
        AppEvent::FocusChanged(Some(UiKey::new(CHART_DETAILS))),
    );
    app.act(
        &mut panel,
        AppEvent::Action(crate::native_app::ActionInvocation {
            id: ActionId::new(CHART_DETAILS),
            value: None,
        }),
    );
    app.act(&mut panel, AppEvent::StepFocus(1));
    assert_eq!(panel.common.last_focus, Some(first));
    assert!(!panel.chart_details);
    app.resources.get_mut(&7).unwrap().traces[DiskRead as usize].latest = Some(0.005);
    let pref = crate::native_appearance::current_preferences();
    crate::native_appearance::install_preferences(
        crate::native_appearance::AppearancePreferences {
            text_scale: 1.5,
            ..pref
        },
    );
    let large = page(&app, &panel, rect.width, rect.height)
        .compile(rect)
        .unwrap();
    let mem = texts(&large);
    let large_scale = crate::native_appearance::text_scale();
    let heading = large
        .paint
        .iter()
        .find(|p| p.key.as_str() == "navigator/title")
        .unwrap();
    let heading_fits =
        text_width("Sessions", SemanticRole::Heading, StyleRef::Primary) <= heading.rect.width;
    let tiny_width = text_width("5e-3B/s", SemanticRole::Text, StyleRef::Note);
    capture_panel(
        &large,
        210,
        680,
        "session-resources-large-text.png",
        crate::native_appearance::forced_chrome_theme(aterm_render::Theme::default(), false),
    );
    crate::native_appearance::install_preferences(pref);
    assert_eq!(large_scale, 1.5);
    assert!(heading_fits);
    assert_eq!(mem["navigator/item/7/DiskRead/value"], "5e-3B/s");
    let tiny = large
        .paint
        .iter()
        .find(|p| p.key.as_str() == "navigator/item/7/DiskRead/value")
        .unwrap();
    assert!(tiny_width <= tiny.rect.width);
    assert_eq!(mem["navigator/item/7/Mem/value"], "≥64MiB");
}

/// Narrow rows keep a known program's name, and wide rows include its age. A row with
/// no observed program uses that space for its age. Speech includes both at every width.
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
    stuck.program = None;
    stuck.moved_ms = now - 3 * 60 * MIN;
    all.insert(2, Arc::new(stuck));
    let mut build = row(3, None);
    let executable = "instrumented-compiler-frontend-".repeat(4);
    build.program = Some(executable.clone());
    build.moved_ms = now - 2 * MIN;
    all.insert(3, Arc::new(build));
    let tree = arrange("m11", &[1, 2, 3], &[10], &all, None);
    let app = app_of(tree, now);
    let view = NavigatorViewState::default();

    for (width, meta) in [(216.0, "claude"), (860.0, "claude \u{00b7} 41m")] {
        let compiled = page(&app, &view, width, 600.0)
            .compile(LogicalRect::new(0.0, 0.0, width, 600.0))
            .expect("the page compiles");
        let texts = texts(&compiled);
        assert_eq!(texts["navigator/title"], "m11", "{width}");
        assert_eq!(texts["navigator/item/1/meta"], meta, "{width}");
        let spoken = &compiled
            .semantic(&UiKey::new("navigator/row/1"))
            .expect("the session button")
            .label;
        assert!(spoken.contains("claude, 41m"), "{width}: {spoken}");
        assert_eq!(texts["navigator/item/1/word"], "working", "{width}");
        assert_eq!(texts["navigator/item/1/under"], "1 under", "{width}");
        assert_eq!(
            texts["navigator/item/2/meta"], "3h",
            "no program was observed"
        );
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
            let build = &texts["navigator/item/3/meta"];
            assert!(build.ends_with("… · 2m"), "{build}");
            let spoken = &compiled
                .semantic(&UiKey::new("navigator/row/3"))
                .expect("the build session")
                .label;
            assert!(spoken.contains(&format!("{executable}, 2m")));
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

    let frame = Frame::of(&app.tree, &view, width, 900.0);
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

#[test]
fn a_window_below_the_sessions_can_be_revealed_and_opened_with_the_keyboard() {
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let mut app = app_of(arrange("m11", &[1], &[], &rows(&[(1, None)]), None), 0);
    let mut view = NavigatorViewState::in_dock(Some(1));
    view.sessions.order.push(1);
    view.sessions.shown = 1;
    let window = crate::tab_model::ViewId::from_stored(8);
    view.sessions.windows.push(OrganizerWindow {
        view: window,
        label: "Tables".into(),
        selected: false,
    });
    view.lay(&app.tree, 260.0, 120.0);
    app.act(&mut view, AppEvent::StepFocus(1));
    assert_eq!(
        view.common.last_focus.as_ref().unwrap().as_str(),
        "navigator/window/8"
    );
    assert!(view.scroll > 0.0);
    app.act(&mut view, AppEvent::TextInput(TextInputEvent::Submit));
    assert!(matches!(app.actions.pop_front(), Some(NavAction::Window(v)) if v == window));
    app.act(&mut view, AppEvent::StepFocus(-1));
    assert_eq!(view.focused_row(), Some(1));
}
