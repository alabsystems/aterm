// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn live_grid(
    rows: u16,
    cols: u16,
    bytes: &[u8],
    cursor: Option<(usize, usize)>,
) -> crate::accessibility::AccessibleSnapshot {
    let mut term = aterm_core::terminal::Terminal::new(rows, cols);
    term.process(bytes);
    let cells: Vec<_> = (0..usize::from(rows)).map(|r| term.render_row(r)).collect();
    crate::accessibility::AccessibleSnapshot::from_cells(&cells, usize::from(cols), cursor)
}

fn node(update: &TreeUpdate, id: NodeId) -> &Node {
    &update.nodes.iter().find(|(n, _)| *n == id).unwrap().1
}

#[test]
fn character_metadata_allocates_only_its_owned_slice() {
    let mut observed = Vec::new();
    for text in [
        String::new(),
        "\n".to_string(),
        "hello".to_string(),
        "中é🦀".to_string(),
        "é".repeat(257),
        "x".repeat(1024),
    ] {
        let mut run = Node::new(Role::TextRun);
        let (_, allocations) = aterm_heap_count::allocations_in(|| {
            run.set_character_lengths(character_lengths_of(std::hint::black_box(&text)));
            std::hint::black_box(run.character_lengths());
        });
        println!(
            "bytes={} characters={} allocations={allocations}",
            text.len(),
            run.character_lengths().len()
        );
        observed.push(allocations);
    }
    assert_eq!(observed, [0, 1, 1, 1, 1, 1]);
}

#[test]
fn character_metadata_preserves_every_scalar_and_byte() {
    for (text, expected) in [
        (String::new(), vec![]),
        ("a\n".to_string(), vec![1, 1]),
        ("中é🦀".to_string(), vec![3, 2, 4]),
        ("e\u{301}".to_string(), vec![1, 2]),
        ("é".repeat(257), vec![2; 257]),
        ("x".repeat(1024), vec![1; 1024]),
    ] {
        let mut run = Node::new(Role::TextRun);
        run.set_character_lengths(character_lengths_of(&text));
        assert_eq!(run.character_lengths(), expected);
        assert_eq!(
            run.character_lengths()
                .iter()
                .map(|&n| usize::from(n))
                .sum::<usize>(),
            text.len()
        );
    }
}

#[test]
#[ignore = "measures character metadata construction; run alone"]
fn character_metadata_construction_cost() {
    use std::hint::black_box;
    use std::time::Instant;
    const CALLS: usize = 200_000;
    fn vector_reference(text: &str) -> Box<[u8]> {
        text.chars()
            .map(|c| u8::try_from(c.len_utf8()).unwrap_or(4))
            .collect::<Vec<u8>>()
            .into_boxed_slice()
    }
    for (case, text) in [
        ("empty", String::new()),
        ("newline", "\n".to_string()),
        ("ascii", "hello".to_string()),
        ("ascii_long", "x".repeat(1024)),
        ("unicode", "中é🦀".to_string()),
        ("unicode_long", "é".repeat(1024)),
        ("combining", "e\u{301}".repeat(128)),
    ] {
        assert_eq!(character_lengths_of(&text), vector_reference(&text));
        for sample in 0..5 {
            for reference in if sample % 2 == 0 {
                [true, false]
            } else {
                [false, true]
            } {
                let make: fn(&str) -> Box<[u8]> = if reference {
                    vector_reference
                } else {
                    character_lengths_of
                };
                for _ in 0..100 {
                    drop(black_box(make(black_box(&text))));
                }
                let start = Instant::now();
                for _ in 0..CALLS {
                    let mut run = Node::new(Role::TextRun);
                    run.set_character_lengths(make(black_box(&text)));
                    black_box(run.character_lengths());
                }
                println!(
                    "case={case} calls={CALLS} sample={sample} reference={reference} elapsed_ns={}",
                    start.elapsed().as_nanos()
                );
            }
        }
    }
}

#[test]
fn word_metadata_allocates_only_its_owned_slice() {
    let mut observed = Vec::new();
    for line in [
        "hello".to_string(),
        "hello world".to_string(),
        " 中".repeat(300),
    ] {
        let mut run = Node::new(Role::TextRun);
        let (_, allocations) = aterm_heap_count::allocations_in(|| {
            run.set_word_starts(word_starts_of(std::hint::black_box(&line)));
            std::hint::black_box(run.word_starts());
        });
        println!(
            "characters={} entries={} allocations={allocations}",
            line.chars().count(),
            run.word_starts().len()
        );
        observed.push(allocations);
    }
    assert_eq!(
        observed,
        [1, 1, 1],
        "only the final node slice needs storage"
    );
}

#[test]
#[ignore = "measures word metadata construction; run alone"]
fn word_metadata_construction_cost() {
    use std::hint::black_box;
    use std::time::Instant;
    const CALLS: usize = 200_000;
    fn vector_reference(line: &str) -> Box<[u8]> {
        let mut starts = vec![0u8];
        let mut prev_ws = false;
        for (i, ch) in line.chars().enumerate() {
            if i > 0 && prev_ws && !ch.is_whitespace() {
                let Ok(index) = u8::try_from(i) else { break };
                starts.push(index);
            }
            prev_ws = ch.is_whitespace();
        }
        starts.into_boxed_slice()
    }
    for (case, line) in [
        ("empty", String::new()),
        ("single", "hello".to_string()),
        ("long_single", "x".repeat(1024)),
        ("two", "hello world".to_string()),
        ("unicode_limit", " 中".repeat(300)),
    ] {
        assert_eq!(word_starts_of(&line), vector_reference(&line));
        for sample in 0..5 {
            for reference in if sample % 2 == 0 {
                [true, false]
            } else {
                [false, true]
            } {
                let make: fn(&str) -> Box<[u8]> = if reference {
                    vector_reference
                } else {
                    word_starts_of
                };
                for _ in 0..100 {
                    drop(black_box(make(black_box(&line))));
                }
                let start = Instant::now();
                for _ in 0..CALLS {
                    let mut run = Node::new(Role::TextRun);
                    run.set_word_starts(make(black_box(&line)));
                    black_box(run.word_starts());
                }
                println!(
                    "case={case} calls={CALLS} sample={sample} reference={reference} elapsed_ns={}",
                    start.elapsed().as_nanos()
                );
            }
        }
    }
}

#[test]
fn word_metadata_preserves_scalar_offsets_and_the_index_limit() {
    for (line, expected) in [
        ("".to_string(), vec![0]),
        (" \u{a0}\u{2003}".to_string(), vec![0]),
        (" \u{a0}中\u{2003}é".to_string(), vec![0, 2, 4]),
        (format!("{} a", "x".repeat(254)), vec![0, 255]),
        (format!("{} a", "x".repeat(255)), vec![0]),
        (format!("{} tail word", "x".repeat(1024)), vec![0]),
        (
            " 中".repeat(300),
            std::iter::once(0)
                .chain((0..128).map(|i| (2 * i + 1) as u8))
                .collect(),
        ),
    ] {
        let mut run = Node::new(Role::TextRun);
        run.set_word_starts(word_starts_of(&line));
        assert_eq!(run.word_starts(), expected);
    }
}

#[test]
fn text_runs_borrow_rows_without_intermediate_allocations() {
    for line in ["abc\n".to_string(), "中".repeat(257), "é".repeat(65_537)] {
        let ((chunks, bytes), allocations) = aterm_heap_count::allocations_in(|| {
            row_chunks(&line).fold((0, 0), |(chunks, bytes), chunk| {
                (chunks + 1, bytes + std::hint::black_box(chunk).len())
            })
        });
        assert_eq!(bytes, line.len());
        assert!((1..=RUNS_PER_ROW as usize).contains(&chunks));
        assert_eq!(allocations, 0, "text-run projection must borrow its source");
    }
}

#[test]
fn text_runs_preserve_unicode_and_the_bounded_address_space() {
    for count in [0usize, 1, 255, 256, 257, 65_280, 65_536, 65_537] {
        for newline in [false, true] {
            let mut line = "中".repeat(count);
            if newline {
                line.push('\n');
            }
            let chunks: Vec<_> = row_chunks(&line).collect();
            assert_eq!(chunks.iter().copied().collect::<String>(), line);
            assert_eq!(
                chunks.len(),
                (count + usize::from(newline))
                    .div_ceil(RUN_CHARS)
                    .clamp(1, RUNS_PER_ROW as usize)
            );
            for chunk in chunks.iter().take(chunks.len() - 1) {
                assert_eq!(chunk.chars().count(), RUN_CHARS);
            }
        }
    }
    let text = format!("{}\n", "中".repeat(65_537));
    let snap = crate::accessibility::AccessibleSnapshot {
        text: text.clone(),
        rows: 1,
        cols: 65_537,
        cursor: Some((0, 65_536)),
    };
    let update = grid_tree(&snap, None, &GridFrame::default());
    let terminal = node(&update, GRID);
    assert_eq!(terminal.children().len(), RUNS_PER_ROW as usize);
    let joined: String = terminal
        .children()
        .iter()
        .map(|id| node(&update, *id).value().unwrap())
        .collect();
    assert_eq!(joined, text);
    let caret = terminal.text_selection().unwrap().focus;
    assert_eq!(caret.node, row_run_id(0, 0, 255));
    assert_eq!(caret.character_index, 256);
}

#[test]
#[ignore = "measures selection lookup cost; run optimized and alone"]
fn unselected_document_lookup_cost() {
    use std::hint::black_box;
    use std::time::Instant;

    for (rows, cols) in [(24, 80), (120, 240)] {
        let mut term = aterm_core::terminal::Terminal::new(rows, cols);
        for row in 0..rows {
            term.process(
                format!("\x1b[{};1H{}", row + 1, "x".repeat(cols as usize - 1)).as_bytes(),
            );
        }
        let mut cells = aterm_core::terminal::TextFrame::default();
        term.text_frame_into(&mut cells, rows as usize, cols as usize);
        let snap = crate::accessibility::AccessibleSnapshot::from_cells(
            cells.cells(),
            cols as usize,
            None,
        );
        for sample in 0..5 {
            let started = Instant::now();
            let mut selected = 0;
            for _ in 0..20_000 {
                selected += usize::from(
                    black_box(selected_span(black_box(&cells), black_box(&snap))).is_some(),
                );
            }
            let elapsed = started.elapsed();
            assert_eq!(selected, 0);
            println!(
                "rows={rows} cols={cols} bytes={} calls=20000 sample={sample} elapsed_ns={}",
                snap.value().len(),
                elapsed.as_nanos()
            );
        }
    }
}

const GEOM: GridGeometry = GridGeometry {
    origin_x: 8.0,
    origin_y: 30.0,
    cell_w: 9.0,
    cell_h: 18.0,
    strip_y: 12.0,
    strip_h: 18.0,
};

#[test]
fn grid_tree_publishes_the_terminal_grid() {
    let snap = live_grid(2, 10, b"hello", Some((0, 5)));
    let update = grid_tree(&snap, None, &GridFrame::default());
    assert_eq!(update.focus, GRID);
    let root = node(&update, ROOT);
    let grid = node(&update, GRID);
    assert_eq!(root.role(), Role::Window);
    assert_eq!(root.children(), [GRID], "no strip published without tabs");
    assert_eq!(grid.role(), Role::Terminal);
    assert_eq!(
        grid.value(),
        Some(snap.value()),
        "the node carries the live grid text"
    );
    assert_eq!(grid.label(), Some(crate::accessibility::LABEL));
    assert!(grid.is_read_only());
    assert!(
        snap.value().starts_with("hello"),
        "sanity: real text, got {:?}",
        snap.value()
    );
}

#[test]
fn terminal_node_publishes_one_text_run_per_row() {
    let snap = live_grid(3, 12, b"alpha\r\nbeta", Some((1, 4)));
    let update = grid_tree(&snap, Some(GEOM), &GridFrame::default());
    let grid = node(&update, GRID);
    let rows: Vec<NodeId> = (0..3)
        .map(|r| NodeId(ROW_BASE + r * RUNS_PER_ROW))
        .collect();
    assert_eq!(grid.children(), rows, "one text run per visible row");

    let mut rebuilt = String::new();
    for (r, id) in rows.iter().enumerate() {
        let run = node(&update, *id);
        assert_eq!(run.role(), Role::TextRun, "row {r} is a text run");
        let value = run.value().unwrap();
        assert!(
            value.ends_with('\n'),
            "row {r} carries its line break: {value:?}"
        );
        assert_eq!(
            run.character_lengths().len(),
            value.chars().count(),
            "row {r}: one character length per character, line break included"
        );
        assert_eq!(
            run.character_lengths()
                .iter()
                .map(|n| *n as usize)
                .sum::<usize>(),
            value.len(),
            "row {r}: character lengths cover every UTF-8 byte"
        );
        rebuilt.push_str(value);
    }
    assert_eq!(
        rebuilt,
        snap.value(),
        "the runs ARE the snapshot text — the a11y document and the SIGUSR1 capture \
         cannot drift"
    );
    assert!(rebuilt.starts_with("alpha\nbeta\n"), "sanity: {rebuilt:?}");
}

#[test]
fn caret_is_a_degenerate_selection_at_the_cursor() {
    let snap = live_grid(3, 12, b"alpha\r\nbeta", Some((1, 4)));
    let update = grid_tree(&snap, Some(GEOM), &GridFrame::default());
    let selection = node(&update, GRID).text_selection().copied().unwrap();
    assert_eq!(
        selection.anchor, selection.focus,
        "a caret, not a selection"
    );
    assert_eq!(
        selection.focus.node,
        NodeId(ROW_BASE + RUNS_PER_ROW),
        "on row 1's run"
    );
    assert_eq!(selection.focus.character_index, 4);
}

#[test]
fn caret_in_trailing_blanks_clamps_onto_the_line_break() {
    let snap = live_grid(2, 20, b"hi", Some((0, 17)));
    let update = grid_tree(&snap, None, &GridFrame::default());
    let selection = node(&update, GRID).text_selection().copied().unwrap();
    assert_eq!(selection.focus.node, NodeId(ROW_BASE));
    assert_eq!(
        selection.focus.character_index, 2,
        "clamped to the trimmed row length — the index of the '\\n'"
    );
}

#[test]
fn hidden_cursor_publishes_no_caret() {
    let snap = live_grid(2, 10, b"hello", None);
    let update = grid_tree(&snap, None, &GridFrame::default());
    assert_eq!(node(&update, GRID).text_selection(), None);
}

#[test]
fn a_row_wider_than_the_index_space_is_split_into_addressable_runs() {
    let cols: u16 = 300;
    let text: Vec<u8> = std::iter::repeat_n(b'w', 290).collect();
    let snap = live_grid(2, cols, &text, Some((0, 281)));
    let update = grid_tree(&snap, None, &GridFrame::default());
    let grid = node(&update, GRID);

    let runs: Vec<NodeId> = grid.children().to_vec();
    let row0: Vec<&NodeId> = runs
        .iter()
        .filter(|id| id.0 >= ROW_BASE && id.0 < ROW_BASE + RUNS_PER_ROW)
        .collect();
    assert!(
        row0.len() >= 2,
        "a {cols}-column row must not ride one run: {row0:?}"
    );
    for id in &runs {
        let run = node(&update, *id);
        let chars = run.value().map_or(0, |v| v.chars().count());
        assert!(
            chars <= RUN_CHARS,
            "run {id:?} is {chars} characters — past the u8 query space"
        );
    }

    let sel = grid.text_selection().copied().expect("a caret");
    assert_eq!(
        sel.focus.node,
        NodeId(ROW_BASE + 1),
        "the SECOND run of row 0"
    );
    assert_eq!(sel.focus.character_index, 281 - RUN_CHARS);
}

#[test]
fn character_geometry_tracks_the_cell_grid() {
    let snap = live_grid(2, 10, b"abc", Some((0, 3)));
    let update = grid_tree(&snap, Some(GEOM), &GridFrame::default());
    let run = node(&update, NodeId(ROW_BASE));
    assert_eq!(run.value(), Some("abc\n"));
    assert_eq!(run.character_positions().unwrap(), [0.0, 9.0, 18.0, 27.0]);
    assert_eq!(
        run.character_widths().unwrap(),
        [9.0, 9.0, 9.0, 0.0],
        "the line break occupies no cell"
    );
    assert_eq!(
        run.bounds().unwrap(),
        Rect {
            x0: 8.0,
            y0: 30.0,
            x1: 8.0 + 10.0 * 9.0,
            y1: 48.0
        },
        "row 0 spans the full grid width, one cell tall"
    );
    assert_eq!(
        node(&update, GRID).bounds().unwrap(),
        Rect {
            x0: 8.0,
            y0: 30.0,
            x1: 98.0,
            y1: 66.0
        },
        "the terminal node covers every visible row"
    );
}

#[test]
fn missing_geometry_degrades_only_the_geometry() {
    let snap = live_grid(2, 10, b"abc", Some((0, 3)));
    let update = grid_tree(&snap, None, &GridFrame::default());
    let run = node(&update, NodeId(ROW_BASE));
    assert_eq!(run.value(), Some("abc\n"));
    assert_eq!(run.character_lengths().len(), 4);
    assert_eq!(run.character_positions(), None);
    assert_eq!(run.bounds(), None);
    assert_eq!(node(&update, GRID).bounds(), None);
    assert!(
        node(&update, GRID).text_selection().is_some(),
        "caret still published"
    );
}

#[test]
fn word_starts_follow_the_model_definition() {
    assert_eq!(&*word_starts_of("hello world"), &[0, 6]);
    assert_eq!(&*word_starts_of("  ab"), &[0, 2]);
    assert_eq!(&*word_starts_of(""), &[0]);
    assert_eq!(&*word_starts_of("   "), &[0]);
    assert_eq!(&*word_starts_of("$ echo hi"), &[0, 2, 7]);
    let update = grid_tree(
        &live_grid(1, 20, b"echo hi", Some((0, 7))),
        None,
        &GridFrame::default(),
    );
    assert_eq!(node(&update, NodeId(ROW_BASE)).word_starts(), [0, 5]);
}

#[test]
fn tab_strip_publishes_as_a_tab_list() {
    let snap = live_grid(2, 40, b"hi", Some((0, 2)));
    let tabs = vec![
        GridTab {
            index: 0,
            id: (1 << 60) + 11,
            title: "build".into(),
            selected: false,
            start_col: 0,
            end_col: 10,
            theme: Some("Hearth".into()),
        },
        GridTab {
            index: 1,
            id: (1 << 60) + 12,
            title: "logs".into(),
            selected: true,
            start_col: 10,
            end_col: 20,
            theme: None,
        },
    ];
    let update = grid_tree(
        &snap,
        Some(GEOM),
        &GridFrame {
            tabs: &tabs,
            ..GridFrame::default()
        },
    );
    assert_eq!(
        node(&update, ROOT).children(),
        [TAB_LIST, GRID],
        "the strip is announced before the grid, as it is painted"
    );
    let list = node(&update, TAB_LIST);
    assert_eq!(list.role(), Role::TabList);
    assert_eq!(list.children(), [tab_node_id(0), tab_node_id(1)]);
    let logs = node(&update, tab_node_id(1));
    assert_eq!(logs.role(), Role::Tab);
    assert_eq!(logs.label(), Some("logs"));
    assert_eq!(logs.is_selected(), Some(true));
    assert!(logs.supports_action(Action::Click));
    assert!(logs.supports_action(Action::Focus));
    assert_eq!(node(&update, tab_node_id(0)).is_selected(), Some(false));
    assert_eq!(
        node(&update, tab_node_id(0)).description(),
        Some("theme Hearth")
    );
    assert_eq!(logs.description(), None);
    assert_eq!(
        logs.bounds().unwrap(),
        Rect {
            x0: 8.0 + 10.0 * 9.0,
            y0: 12.0,
            x1: 8.0 + 20.0 * 9.0,
            y1: 30.0
        },
        "the announced rectangle is the clickable segment"
    );
    assert_eq!(tab_index_for(tab_node_id(1)), Some(1));
    assert_eq!(tab_index_for(GRID), None, "the grid is not a tab");
    assert_eq!(tab_index_for(ROOT), None);
    assert_eq!(
        tab_index_for(NodeId(ROW_BASE)),
        None,
        "a row run is not a tab"
    );
    let mut request = ActionRequest {
        context: logs.action_context(),
        action: Action::Click,
        target_tree: TreeId::ROOT,
        target_node: tab_node_id(1),
        data: None,
    };
    let lookup = |index: usize| tabs.get(index).map(|tab| tab.id);
    assert_eq!(tab_action_index(&request, lookup), Some(1));
    request.action = Action::Focus;
    assert_eq!(tab_action_index(&request, lookup), Some(1));
    assert_eq!(request.context, Some(ActionContext::tab(tabs[1].id)));
    let mut reordered = tabs.clone();
    reordered.swap(0, 1);
    assert_eq!(
        tab_action_index(&request, |index| reordered.get(index).map(|tab| tab.id)),
        None
    );
    assert_eq!(tab_action_index(&request, |_| None), None, "closed tab");
    for context in [None, Some(ActionContext::new(tabs[1].id, 0))] {
        request.context = context;
        assert_eq!(
            tab_action_index(&request, lookup),
            None,
            "unbound or wrong domain"
        );
    }
}

#[test]
fn a_painted_selection_is_published_and_outranks_the_caret() {
    let snap = live_grid(3, 20, b"alpha\r\nbeta gamma", Some((2, 0)));
    let selected = grid_tree(
        &snap,
        None,
        &GridFrame {
            selection: Some(GridSpan {
                anchor: (1, 5),
                focus: (1, 10),
            }),
            ..GridFrame::default()
        },
    );
    let sel = node(&selected, GRID).text_selection().copied().unwrap();
    assert_eq!(sel.anchor.node, NodeId(ROW_BASE + RUNS_PER_ROW), "row 1");
    assert_eq!(sel.anchor.character_index, 5);
    assert_eq!(sel.focus.node, NodeId(ROW_BASE + RUNS_PER_ROW));
    assert_eq!(sel.focus.character_index, 10);
    assert_ne!(
        sel.anchor, sel.focus,
        "a real range, not the degenerate caret"
    );

    let caret_only = grid_tree(&snap, None, &GridFrame::default());
    let caret = node(&caret_only, GRID).text_selection().copied().unwrap();
    assert_eq!(caret.anchor, caret.focus, "the caret alone");
    assert_eq!(caret.focus.node, NodeId(ROW_BASE + 2 * RUNS_PER_ROW));
}

#[test]
fn a_split_frame_publishes_one_terminal_document_per_pane() {
    let composed = live_grid(2, 21, b"left\x1b[12Gright", Some((0, 16)));
    let first_line = composed.value().lines().next().unwrap();
    assert!(
        first_line.contains("left") && first_line.contains("right"),
        "sanity: ONE composed row carries both panes: {first_line:?}"
    );
    let panes = vec![
        GridPane {
            slot: 0,
            label: "aterm terminal — pane 1 of 2".into(),
            snap: live_grid(2, 10, b"left", None),
            row_off: 0,
            col_off: 0,
            focused: false,
            selection: None,
        },
        GridPane {
            slot: 1,
            label: "aterm terminal — pane 2 of 2".into(),
            snap: live_grid(2, 10, b"right", Some((0, 5))),
            row_off: 0,
            col_off: 11,
            focused: true,
            selection: None,
        },
    ];
    let update = grid_tree(
        &composed,
        Some(GEOM),
        &GridFrame {
            panes: &panes,
            ..GridFrame::default()
        },
    );
    let left = NodeId(PANE_BASE);
    let right = NodeId(PANE_BASE + 1);
    assert_eq!(
        node(&update, ROOT).children(),
        [left, right],
        "one document per pane, in layout order"
    );
    assert!(
        !update.nodes.iter().any(|(id, _)| *id == GRID),
        "the spliced whole-screen document is NOT also published"
    );
    assert_eq!(node(&update, left).role(), Role::Terminal);
    assert_eq!(
        node(&update, left).label(),
        Some("aterm terminal — pane 1 of 2")
    );
    assert_eq!(
        node(&update, left).value(),
        Some("left\n\n"),
        "the left pane's own text, without its neighbour"
    );
    assert_eq!(node(&update, right).value(), Some("right\n\n"));
    assert_eq!(
        update.focus, right,
        "focus names the pane the keyboard is in"
    );

    assert_eq!(
        node(&update, left).children()[0],
        NodeId(ROW_BASE + PANE_STRIDE)
    );
    assert_eq!(
        node(&update, right).children()[0],
        NodeId(ROW_BASE + 2 * PANE_STRIDE)
    );
    assert_eq!(
        node(&update, right).bounds().unwrap().x0,
        GEOM.origin_x + 11.0 * GEOM.cell_w
    );
    assert_eq!(
        tab_index_for(right),
        None,
        "a pane id is never decoded as a tab switch"
    );
}

fn native_composite(roots: &[NodeId], focus: Option<NodeId>) -> TreeUpdate {
    let window_root = NodeId(0xA11E);
    let mut nodes: Vec<(NodeId, Node)> = roots
        .iter()
        .map(|id| (*id, Node::new(Role::Group)))
        .collect();
    let mut window = Node::new(Role::Window);
    window.set_children(roots.to_vec());
    nodes.push((window_root, window));
    TreeUpdate {
        nodes,
        tree: Some(Tree::new(window_root)),
        tree_id: TreeId::ROOT,
        focus: focus.unwrap_or(window_root),
    }
}

fn pane(slot: usize, text: &[u8], focused: bool) -> GridPane {
    GridPane {
        slot,
        label: format!("aterm terminal — pane {} of 4", slot + 1),
        snap: live_grid(2, 10, text, focused.then_some((0, 0))),
        row_off: 0,
        col_off: slot * 11,
        focused,
        selection: None,
    }
}

#[test]
fn a_mixed_tab_s_terminal_panes_take_their_layout_slots() {
    let (a, b) = (NodeId(0xA), NodeId(0xB));
    let mut update = native_composite(&[a, b], None);
    let window_root = update.tree.as_ref().unwrap().root;
    let panes = [pane(0, b"first", true), pane(2, b"third", false)];
    splice_terminal_panes(&mut update, &panes, Some(GEOM), 4).expect("no collision");
    let (first, third) = (NodeId(PANE_BASE), NodeId(PANE_BASE + 2));
    assert_eq!(
        node(&update, window_root).children(),
        [first, a, third, b],
        "terminal and native leaves interleave in layout order"
    );
    assert_eq!(node(&update, first).value(), Some("first\n\n"));
    assert_eq!(node(&update, third).value(), Some("third\n\n"));
    assert_eq!(
        node(&update, third).bounds().unwrap().x0,
        GEOM.origin_x + 22.0 * GEOM.cell_w
    );
    assert_eq!(
        update.focus, first,
        "the focused pane takes the root's focus"
    );
    assert_eq!(
        node(&update, first).children()[0],
        NodeId(ROW_BASE + PANE_STRIDE),
        "pane rows keep the split grid's ids"
    );
}

#[test]
fn a_native_focus_keeps_its_node_when_terminal_panes_join() {
    let (a, b) = (NodeId(0xA), NodeId(0xB));
    let mut update = native_composite(&[a, b], Some(b));
    splice_terminal_panes(&mut update, &[pane(1, b"mid", true)], None, 3).expect("no collision");
    assert_eq!(update.focus, b, "a native control's focus is never taken");
}

#[test]
fn a_native_tab_s_chrome_reads_first_and_takes_focus_only_when_open() {
    let native = NodeId(0xA);
    let find = GridFind {
        query: "needle".into(),
        caret: 6,
        status: String::new(),
    };
    let messages = [message(ChromeMessage::LinkCaption, "alab.systems")];
    let mut update = native_composite(&[native], Some(native));
    let window_root = update.tree.as_ref().unwrap().root;
    let quiet = GridFrame {
        messages: &messages,
        ..GridFrame::default()
    };
    splice_chrome(&mut update, Some(GEOM), &quiet, 20).expect("no collision");
    let caption = ChromeMessage::LinkCaption.node_id();
    assert_eq!(node(&update, window_root).children(), [caption, native]);
    assert_eq!(node(&update, caption).label(), Some("alab.systems"));
    assert_eq!(
        update.focus, native,
        "a message never takes a control's focus"
    );

    let mut update = native_composite(&[native], Some(native));
    let finding = GridFrame {
        messages: &messages,
        find: Some(&find),
        ..GridFrame::default()
    };
    splice_chrome(&mut update, Some(GEOM), &finding, 20).expect("no collision");
    assert_eq!(
        node(&update, window_root).children(),
        [caption, FIND, native]
    );
    assert_eq!(update.focus, FIND, "an open find field owns the keyboard");

    let mut update = native_composite(&[native], Some(native));
    let before = update.clone();
    splice_chrome(&mut update, Some(GEOM), &GridFrame::default(), 20).expect("nothing to add");
    assert_eq!(
        update, before,
        "no chrome on screen leaves the tree as composed"
    );
}

#[test]
fn a_chrome_id_that_collides_leaves_the_native_tree_unchanged() {
    let find = GridFind {
        query: String::new(),
        caret: 0,
        status: String::new(),
    };
    let mut update = native_composite(&[NodeId(0xA), FIND], None);
    let before = update.clone();
    let frame = GridFrame {
        find: Some(&find),
        ..GridFrame::default()
    };
    assert_eq!(splice_chrome(&mut update, None, &frame, 20), Err(FIND));
    assert_eq!(update, before);
}

#[test]
fn a_terminal_pane_id_that_collides_leaves_the_native_tree_unchanged() {
    let clash = NodeId(PANE_BASE + 1);
    let mut update = native_composite(&[NodeId(0xA), clash], None);
    let before = update.clone();
    assert_eq!(
        splice_terminal_panes(&mut update, &[pane(1, b"x", true)], None, 3),
        Err(clash)
    );
    assert_eq!(update, before);
}

#[test]
fn the_find_field_is_a_text_input_that_owns_focus() {
    let snap = live_grid(2, 20, b"hello", Some((0, 5)));
    let find = GridFind {
        query: "hell".into(),
        caret: 4,
        status: "match 1 of 3; case sensitive".into(),
    };
    let update = grid_tree(
        &snap,
        None,
        &GridFrame {
            find: Some(&find),
            ..GridFrame::default()
        },
    );
    assert_eq!(
        node(&update, ROOT).children(),
        [FIND, GRID],
        "the panel is announced above the grid, as it is painted"
    );
    let field = node(&update, FIND);
    assert_eq!(field.role(), Role::SearchInput);
    assert_eq!(field.label(), Some("Find"));
    assert_eq!(field.value(), Some("hell"));
    assert_eq!(field.description(), Some("match 1 of 3; case sensitive"));
    assert_eq!(field.children(), [FIND_RUN]);
    assert!(field.supports_action(Action::Focus));
    let run = node(&update, FIND_RUN);
    assert_eq!(run.role(), Role::TextRun);
    assert_eq!(run.value(), Some("hell"));
    assert_eq!(run.character_lengths().len(), 4);
    let caret = field.text_selection().copied().unwrap();
    assert_eq!(caret.focus.node, FIND_RUN);
    assert_eq!(caret.focus.character_index, 4);
    assert_eq!(update.focus, FIND, "the keyboard is in the query field");

    let closed = grid_tree(&snap, None, &GridFrame::default());
    assert_eq!(closed.focus, GRID);
    assert!(!closed.nodes.iter().any(|(id, _)| *id == FIND));
}

#[test]
fn the_stats_deck_is_a_group_of_meters_and_a_group_of_cores() {
    let snap = live_grid(2, 20, b"", None);
    let meter = |name: &str, value: Option<f32>| StatsMeter {
        name: name.into(),
        value,
        words: format!("{name} said"),
    };
    let stats = GridStats {
        group: StatsGroup {
            name: "Stats deck, newest first",
            gauge: Some(meter("Strain", Some(100.0))),
            meters: vec![meter("CPU", Some(98.0)), meter("Tokens", None)],
            cores_name: "2 CPU cores".into(),
            cores: vec![
                meter("Super core 1", Some(54.0)),
                meter("Super core 2", None),
            ],
        },
        bounds: None,
        lens: Some(StatsLens {
            keys: true,
            name: "CPU close-up".into(),
            explainer: "Heat is the share of the cores in use.".into(),
            wells: vec![("Now".into(), "CPU 98 percent of capacity.".into())],
            bounds: None,
        }),
    };
    let frame = GridFrame {
        stats: Some(&stats),
        ..GridFrame::default()
    };
    let update = grid_tree(&snap, Some(GEOM), &frame);
    let group = node(&update, NodeId(STATS_BASE));
    assert_eq!(group.role(), Role::Group);
    assert_eq!(group.label(), Some("Stats deck, newest first"));
    let kids = group.children();
    assert_eq!(kids.len(), 4, "the gauge, two meters and the cores");
    let gauge = node(&update, kids[0]);
    assert_eq!(
        (gauge.role(), gauge.label(), gauge.numeric_value()),
        (Role::Meter, Some("Strain"), Some(100.0)),
        "the gauge at the deck's top left is read first"
    );
    assert_eq!(gauge.description(), Some("Strain said"));
    let cpu = node(&update, kids[1]);
    assert_eq!(
        (cpu.role(), cpu.label(), cpu.numeric_value()),
        (Role::Meter, Some("CPU"), Some(98.0))
    );
    assert_eq!(
        (cpu.min_numeric_value(), cpu.max_numeric_value()),
        (Some(0.0), Some(100.0))
    );
    assert_eq!(cpu.description(), Some("CPU said"));
    let tokens = node(&update, kids[2]);
    assert_eq!(
        (tokens.label(), tokens.numeric_value()),
        (Some("Tokens"), None)
    );
    assert_eq!(tokens.description(), Some("Tokens said"));
    let cores = node(&update, kids[3]);
    assert_eq!(
        (cores.role(), cores.label()),
        (Role::Group, Some("2 CPU cores"))
    );
    let cells: Vec<_> = cores
        .children()
        .iter()
        .map(|&id| {
            let n = node(&update, id);
            (n.role(), n.label(), n.numeric_value())
        })
        .collect();
    assert_eq!(
        cells,
        [
            (Role::Meter, Some("Super core 1"), Some(54.0)),
            (Role::Meter, Some("Super core 2"), None)
        ]
    );
    let lens = node(&update, NodeId(STATS_LENS));
    assert_eq!(lens.label(), Some("CPU close-up"));
    assert_eq!(update.focus, lens.children()[0], "the lens's first well");
    assert_eq!(
        node(&update, update.focus).label(),
        Some("Now"),
        "the well is named by its heading"
    );
}

fn message(message: ChromeMessage, text: &str) -> GridMessage {
    GridMessage {
        message,
        text: text.to_string(),
        detail: None,
    }
}

#[test]
fn every_chrome_message_announces_the_same_sentence_by_name_and_by_value() {
    let snap = live_grid(2, 20, b"hi", None);
    let messages: Vec<GridMessage> = ChromeMessage::ORDER
        .iter()
        .rev()
        .map(|kind| message(*kind, &format!("{kind:?} speaks")))
        .collect();
    let update = grid_tree(
        &snap,
        None,
        &GridFrame {
            messages: &messages,
            ..GridFrame::default()
        },
    );
    let without = grid_tree(&snap, None, &GridFrame::default());
    assert_eq!(
        update.nodes.len(),
        without.nodes.len() + ChromeMessage::ORDER.len(),
        "each message adds exactly its own node"
    );
    assert_eq!(
        node(&update, ROOT).children(),
        [
            ChromeMessage::PasteConfirm.node_id(),
            ChromeMessage::CloseConfirm.node_id(),
            ChromeMessage::LinkCaption.node_id(),
            GRID,
        ],
        "the messages in ORDER, whatever order the frame lists them in, then the grid"
    );
    for kind in ChromeMessage::ORDER {
        let node = node(&update, NodeId(MESSAGE_BASE + kind.slot()));
        let spoken = format!("{kind:?} speaks");
        assert_eq!(node.label(), Some(spoken.as_str()), "{kind:?} name");
        assert_eq!(node.value(), Some(spoken.as_str()), "{kind:?} value");
        assert_eq!(node.live(), Some(kind.politeness()), "{kind:?} politeness");
    }
}

#[test]
fn the_paste_confirmation_interrupts_without_stealing_the_keyboard() {
    let snap = live_grid(2, 20, b"hi", Some((0, 2)));
    let asked = crate::paste_banner::question("one\ntwo\nthree\nfour\nfive\nsix\nseven");
    let messages = vec![GridMessage {
        detail: Some(crate::paste_banner::announcement_detail("one\ntwo")),
        ..message(ChromeMessage::PasteConfirm, &asked)
    }];
    let update = grid_tree(
        &snap,
        None,
        &GridFrame {
            messages: &messages,
            ..GridFrame::default()
        },
    );
    let ask = node(
        &update,
        NodeId(MESSAGE_BASE + ChromeMessage::PasteConfirm.slot()),
    );
    assert_eq!(ask.role(), Role::AlertDialog, "AT-SPI Alert");
    assert_eq!(ask.live(), Some(Live::Assertive));
    assert_eq!(ask.label(), Some(asked.as_str()));
    assert_eq!(
        ask.description(),
        Some(crate::paste_banner::announcement_detail("one\ntwo").as_str())
    );
    assert!(
        !ask.supports_action(Action::Click),
        "a generic activate names neither answer"
    );
    assert_eq!(update.focus, GRID, "Enter still reaches the terminal");
}

/// A message's id is a function of its kind alone, one slot each, never read as a tab, a chip
/// or a menu row, and never a slot a surface that is gone held: the notice (0), the config
/// warning (2), the message band's rows (3, 4, 6) and overflow link (7), the presence status
/// (5), and the band's capsules (16 to 27, four slots a row).
#[test]
fn message_ids_are_stable_and_never_reuse_a_retired_slot_or_read_as_a_tab() {
    let snap = live_grid(2, 20, b"hi", None);
    let retired: Vec<u64> = [0, 2, 3, 4, 5, 6, 7].into_iter().chain(16..=27).collect();
    let mut slots = std::collections::HashSet::new();
    for kind in ChromeMessage::ORDER {
        let id = kind.node_id();
        assert_eq!(id, NodeId(MESSAGE_BASE + kind.slot()), "{kind:?}");
        assert!(slots.insert(kind.slot()), "{kind:?}: a slot of its own");
        assert!(
            !retired.contains(&kind.slot()),
            "{kind:?} is published at a retired slot"
        );
        assert_eq!(tab_index_for(id), None, "{kind:?} is not a tab");
        assert_eq!(chip_for(id), None, "{kind:?} is not a chip");
        assert_eq!(menu_item_for(id), None, "{kind:?} is not a menu row");
    }

    let first = grid_tree(
        &snap,
        None,
        &GridFrame {
            messages: &[message(ChromeMessage::PasteConfirm, "one")],
            ..GridFrame::default()
        },
    );
    let second = grid_tree(
        &snap,
        None,
        &GridFrame {
            messages: &[message(ChromeMessage::PasteConfirm, "two")],
            ..GridFrame::default()
        },
    );
    let id_of = |u: &TreeUpdate| {
        u.nodes
            .iter()
            .find(|(_, n)| n.role() == Role::AlertDialog)
            .map(|(id, _)| *id)
            .unwrap()
    };
    assert_eq!(id_of(&first), id_of(&second), "one node, new words");
}

#[test]
fn the_window_is_named_what_its_titlebar_names_it() {
    let snap = live_grid(2, 20, b"hi", None);
    let titled = grid_tree(
        &snap,
        None,
        &GridFrame {
            window_title: "vim src/main.rs \u{2014} ~/aterm",
            ..GridFrame::default()
        },
    );
    assert_eq!(
        node(&titled, ROOT).label(),
        Some("vim src/main.rs \u{2014} ~/aterm")
    );
    let untitled = grid_tree(&snap, None, &GridFrame::default());
    assert_eq!(node(&untitled, ROOT).label(), Some("aterm"));
}

fn spoken(text: &str) -> bool {
    text.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '.' | ':' | ','))
}

#[test]
fn the_harness_chips_are_buttons_named_in_words() {
    let snap = live_grid(3, 40, b"hi", Some((0, 2)));
    let chips = [
        GridChip {
            session: 4,
            chip: HarnessChip::Autopilot,
            label: "Autopilot: cruise. Lands at 18:00.".to_string(),
            description: Some("Last: approved at 14:05.".to_string()),
            toggled: None,
            row: 2,
            col: 4,
            width: 1,
        },
        GridChip {
            session: 4,
            chip: HarnessChip::Team,
            label: "Worker of fmt".to_string(),
            description: Some("Goes to its manager.".to_string()),
            toggled: None,
            row: 2,
            col: 8,
            width: 1,
        },
    ];
    let update = grid_tree(
        &snap,
        Some(GEOM),
        &GridFrame {
            chips: &chips,
            ..GridFrame::default()
        },
    );
    let ids: Vec<NodeId> = chips
        .iter()
        .map(|c| chip_node_id(c.session, c.chip).expect("an id"))
        .collect();
    assert_eq!(node(&update, ROOT).children(), [ids[0], ids[1], GRID]);
    for (chip, id) in chips.iter().zip(&ids) {
        let n = node(&update, *id);
        assert_eq!(n.role(), Role::Button);
        assert!(n.supports_action(Action::Click), "clickable");
        assert_eq!(n.label(), Some(chip.label.as_str()));
        assert!(spoken(n.label().unwrap()), "{:?}", n.label());
        assert_eq!(n.description(), chip.description.as_deref());
        assert_eq!(n.live(), None, "never a live region");
        assert_eq!(
            n.bounds(),
            Some(Rect {
                x0: GEOM.origin_x + chip.col as f64 * GEOM.cell_w,
                y0: GEOM.origin_y + 2.0 * GEOM.cell_h,
                x1: GEOM.origin_x + (chip.col + 1) as f64 * GEOM.cell_w,
                y1: GEOM.origin_y + 3.0 * GEOM.cell_h,
            })
        );
        assert_eq!(chip_for(*id), Some((chip.session, chip.chip)));
        assert_eq!(tab_index_for(*id), None);
        // The Autopilot chip is a three-way toggle: only a fact's node has a menu.
        let menu = chip.chip.fact().is_some();
        assert_eq!(
            n.has_popup(),
            menu.then_some(aterm_a11y::model::HasPopup::Menu),
            "{:?}",
            chip.chip
        );
        assert_eq!(
            n.supports_action(Action::ShowContextMenu),
            menu,
            "{:?}",
            chip.chip
        );
    }
    assert!(spoken(chips[0].description.as_deref().unwrap()));
    assert_eq!(chip_for(GRID), None);
    assert_eq!(chip_for(tab_node_id(3)), None);
    assert!(!spoken("\u{221e} cruise"));
    let bare = grid_tree(&snap, Some(GEOM), &GridFrame::default());
    assert!(bare.nodes.iter().all(|(id, _)| chip_for(*id).is_none()));
}

/// `chrome`'s `a11y` lines read the built tree's Autopilot and Account chip nodes, each with
/// its role, label and description as the adapter is handed them, in the tree's order, and
/// nothing else: a Team chip, a chrome message and the grid itself are left out, and a tree with
/// no chip says nothing. The build says which kinds it reports once per read, apart from any
/// window ([`harness_kinds_line`]), so a window with an overlay open, whose tree is the
/// overlay's, cannot take that line with it. The Account lines
/// are what `lab autopilot-captures` waits on before it shoots a Claude Code scene: line E's
/// quota row, its label its view's sentences (`Accounts. …`, the week's chain once its
/// projection is drawn), and line D's fallback unit, its label the account fact's (`Account:
/// …`); the lab reads line E's by that head and judges nothing where no kind is `account`.
/// CONTROLS (run by hand, 2026-11-08): the Autopilot chip alone kept, as before, fails it, and
/// so does the kinds line left out (2026-11-08, after review) or printed per window again
/// (2026-11-09, after recheck).
#[test]
fn the_harness_node_lines_read_the_autopilot_and_account_chips_off_the_built_tree() {
    let snap = live_grid(3, 60, b"hi", Some((0, 2)));
    let chip = |chip, label: &str, description: Option<&str>| GridChip {
        session: 4,
        chip,
        label: label.to_string(),
        description: description.map(str::to_string),
        toggled: None,
        row: 2,
        col: 4,
        width: 1,
    };
    let chips = [
        chip(
            HarnessChip::Autopilot,
            "Autopilot: land.",
            Some("Stopped typing: the same reply twice, with no work."),
        ),
        chip(
            HarnessChip::Team,
            "Worker of fmt",
            Some("Goes to its manager."),
        ),
        chip(
            HarnessChip::Account,
            "Accounts. main, this tab, 80 percent of its week used. Next 7 days: main",
            Some("Activate for the accounts outlook."),
        ),
    ];
    let messages = [GridMessage {
        detail: Some("Paste 3 lines into the terminal".to_string()),
        ..message(ChromeMessage::PasteConfirm, "Paste 3 lines?")
    }];
    let update = grid_tree(
        &snap,
        Some(GEOM),
        &GridFrame {
            chips: &chips,
            messages: &messages,
            ..GridFrame::default()
        },
    );
    let lines = harness_node_lines(2, &update, |session| {
        (session == 4).then(|| "s-4".to_string())
    });
    assert_eq!(
        lines,
        [
            "a11y window=2 autopilot sid=s-4 role=Button label=\"Autopilot: land.\" \
             description=\"Stopped typing: the same reply twice, with no work.\"",
            "a11y window=2 account sid=s-4 role=Button label=\"Accounts. main, this tab, 80 \
             percent of its week used. Next 7 days: main\" description=\"Activate for the \
             accounts outlook.\"",
        ]
    );
    let bare = grid_tree(&snap, Some(GEOM), &GridFrame::default());
    assert!(
        harness_node_lines(2, &bare, |_| None).is_empty(),
        "a window with no chip drawn says nothing"
    );
    assert_eq!(
        harness_kinds_line(),
        "a11y kinds=autopilot,account",
        "the build says what it reports, once per read, apart from any window"
    );
}

/// A SUPERVISED CODEX SESSION'S ACCOUNT CHIP names the CLI and the account
/// (`Codex CLI · codex-1`), the node a button under that label; Claude Code's names no
/// product, its footer fact's own words standing (CONTROL: `claude_facts`' golden).
#[test]
fn another_clis_account_chip_names_its_product_and_claudes_keeps_its_footer_words() {
    use aterm_agent::harness::accounts::Cli;
    assert_eq!(
        account_chip_label(Cli::Codex, Some("codex-1")).as_deref(),
        Some("Codex CLI \u{b7} codex-1")
    );
    assert_eq!(
        account_chip_label(Cli::Kimi, None).as_deref(),
        Some("Kimi Code CLI"),
        "no judged label: the product alone"
    );
    assert_eq!(account_chip_label(Cli::Claude, Some("m16")), None);
    let snap = live_grid(3, 40, b"hi", Some((0, 2)));
    let label = account_chip_label(Cli::Codex, Some("codex-1")).expect("a codex label");
    let chips = [GridChip {
        session: 5,
        chip: HarnessChip::Account,
        label: label.clone(),
        description: Some("Activate for the accounts menu.".to_string()),
        toggled: None,
        row: 2,
        col: 30,
        width: 9,
    }];
    let update = grid_tree(
        &snap,
        Some(GEOM),
        &GridFrame {
            chips: &chips,
            ..GridFrame::default()
        },
    );
    let n = node(
        &update,
        chip_node_id(5, HarnessChip::Account).expect("an id"),
    );
    assert_eq!(n.role(), Role::Button);
    assert_eq!(n.label(), Some("Codex CLI \u{b7} codex-1"));
    assert_eq!(
        chip_for(chip_node_id(5, HarnessChip::Account).unwrap()),
        Some((5, HarnessChip::Account))
    );
}

#[test]
fn the_light_the_switch_and_the_title_slot_have_ids_of_their_own() {
    let all = [
        HarnessChip::Autopilot,
        HarnessChip::Team,
        HarnessChip::TopEffort,
        HarnessChip::Model,
        HarnessChip::Effort,
        HarnessChip::Path,
        HarnessChip::Branch,
        HarnessChip::Usage,
        HarnessChip::Mode,
        HarnessChip::Switch,
        HarnessChip::Title,
        HarnessChip::Context,
        HarnessChip::Account,
        HarnessChip::Mark,
        HarnessChip::CardMark,
        HarnessChip::Upgrade,
    ];
    let mut ids = std::collections::HashSet::new();
    for session in [0, 1, 7] {
        for chip in all {
            let id = chip_node_id(session, chip).expect("an id");
            assert_eq!(chip_for(id), Some((session, chip)));
            assert!(ids.insert(id), "{session} {chip:?}: one id each");
        }
    }
    for free in u64::try_from(all.len()).unwrap()..HarnessChip::SLOTS {
        assert_eq!(
            chip_for(NodeId(CHIP_BASE + HarnessChip::SLOTS + free)),
            None,
            "slot {free}: free"
        );
    }
    let snap = live_grid(3, 40, b"hi", Some((0, 2)));
    let at = |chip, label: &str, col| GridChip {
        session: 2,
        chip,
        label: label.to_string(),
        description: None,
        row: 2,
        col,
        width: 4,
        toggled: None,
    };
    let chips = [
        at(HarnessChip::Mode, "Permission mode: yolo off.", 2),
        at(HarnessChip::Title, "plan mode on \u{b7} click: yolo", 8),
        at(HarnessChip::Mark, "ALab, alab.systems", 28),
        at(HarnessChip::CardMark, "ALab", 34),
        at(HarnessChip::Upgrade, "Update Claude Code to 2.1.310", 22),
    ];
    let update = grid_tree(
        &snap,
        Some(GEOM),
        &GridFrame {
            chips: &chips,
            ..GridFrame::default()
        },
    );
    for chip in &chips {
        let n = node(&update, chip_node_id(2, chip.chip).expect("an id"));
        let title = chip.chip == HarnessChip::Title;
        let role = match chip.chip {
            HarnessChip::Title => Role::Status,
            HarnessChip::Mark | HarnessChip::CardMark => Role::Link,
            _ => Role::Button,
        };
        assert_eq!(n.role(), role, "{:?}", chip.chip);
        assert_eq!(n.supports_action(Action::Click), !title, "{:?}", chip.chip);
        assert!(
            !n.supports_action(Action::ShowContextMenu),
            "{:?}",
            chip.chip
        );
        assert_eq!(n.label(), Some(chip.label.as_str()));
        assert_eq!(n.live(), None, "never a live region");
    }
}

/// A menu runs as long as what it lists: none of its rows may fall off what a client reads.
#[test]
fn a_long_menu_keeps_every_row() {
    let snap = live_grid(12, 40, b"hi", Some((0, 2)));
    let items = (0..200)
        .map(|row| GridMenuItem {
            label: format!("row {row}"),
            checked: None,
            disabled: false,
            row,
            slider: None,
        })
        .collect();
    let menu = GridMenu {
        label: "Tab menu".to_string(),
        epoch: 1,
        items,
        selected: None,
        row: 1,
        col: 4,
        width: 30,
        height: 8,
    };
    let update = grid_tree(
        &snap,
        Some(GEOM),
        &GridFrame {
            menu: Some(&menu),
            ..GridFrame::default()
        },
    );
    assert_eq!(node(&update, NodeId(MENU_BASE)).children().len(), 200);
}

#[test]
fn an_open_grid_menu_is_a_menu_of_items_in_words() {
    let snap = live_grid(12, 40, b"hi", Some((0, 2)));
    let item = |label: &str, checked, disabled, row| GridMenuItem {
        label: label.to_string(),
        checked,
        disabled,
        row,
        slider: None,
    };
    let menu = GridMenu {
        label: "Autopilot, tab 3, fix the footer".to_string(),
        epoch: 0xa_5a5a,
        items: vec![
            item("Cruise", Some(true), false, 2),
            item("Land now", Some(false), false, 3),
            item("Off", Some(false), false, 4),
            item("Cruise for 1 hour, then land", None, true, 6),
            item("Harness settings", None, false, 8),
        ],
        selected: Some(1),
        row: 1,
        col: 4,
        width: 30,
        height: 8,
    };
    let update = grid_tree(
        &snap,
        Some(GEOM),
        &GridFrame {
            menu: Some(&menu),
            ..GridFrame::default()
        },
    );
    let root = node(&update, ROOT);
    assert_eq!(root.children(), [NodeId(MENU_BASE), GRID]);
    let m = node(&update, NodeId(MENU_BASE));
    assert_eq!(m.role(), Role::Menu);
    assert_eq!(m.label(), Some(menu.label.as_str()));
    assert_eq!(m.children().len(), menu.items.len());
    for (i, (id, want)) in m.children().iter().zip(&menu.items).enumerate() {
        let n = node(&update, *id);
        assert_eq!(menu_item_for(*id), Some((menu.epoch, i)));
        assert!(id.0 > MENU_BASE && id.0 < CHIP_BASE, "{id:?}");
        assert_eq!(chip_for(*id), None);
        assert_eq!(tab_index_for(*id), None);
        assert_eq!(n.label(), Some(want.label.as_str()));
        assert!(spoken(&want.label), "{}", want.label);
        match want.checked {
            Some(checked) => {
                assert_eq!(n.role(), Role::MenuItemRadio);
                assert_eq!(
                    n.toggled(),
                    Some(if checked {
                        aterm_a11y::model::Toggled::True
                    } else {
                        aterm_a11y::model::Toggled::False
                    })
                );
            }
            None => {
                assert_eq!(n.role(), Role::MenuItem);
                assert_eq!(n.toggled(), None);
            }
        }
        assert_eq!(n.is_disabled(), want.disabled, "{}", want.label);
        assert_eq!(n.supports_action(Action::Click), !want.disabled);
    }
    assert_eq!(update.focus, m.children()[1], "the highlighted row");
    assert_eq!(menu_item_for(NodeId(MENU_BASE)), None, "the menu is no row");
    let moved = grid_tree(
        &snap,
        Some(GEOM),
        &GridFrame {
            menu: Some(&GridMenu {
                epoch: menu.epoch + 1,
                ..menu.clone()
            }),
            ..GridFrame::default()
        },
    );
    let moved_items = node(&moved, NodeId(MENU_BASE)).children().to_vec();
    for (old, new) in m.children().iter().zip(&moved_items) {
        assert_ne!(old, new);
        assert_eq!(menu_item_for(*new).map(|(e, _)| e), Some(menu.epoch + 1));
    }
    let last = menu_item_node_id(u32::MAX, (MENU_ROWS - 1) as usize).unwrap();
    assert!(last.0 < CHIP_BASE, "{last:?}");
    assert_eq!(
        menu_item_for(last),
        Some((menu_epoch_from(u64::MAX), (MENU_ROWS - 1) as usize))
    );
    assert_eq!(menu_item_node_id(0, MENU_ROWS as usize), None);
    assert_eq!(menu_item_for(NodeId(CHIP_BASE)), None);
    let bare = grid_tree(&snap, Some(GEOM), &GridFrame::default());
    assert_eq!(bare.focus, GRID);
    assert!(bare.nodes.iter().all(|(id, _)| id.0 != MENU_BASE));
}

/// A SLIDER ROW IS PUBLISHED AS A SLIDER: its value in words and as the 1-based stop of its
/// stops, stepped by the reader's increment and decrement; a value that is none of its stops
/// has no number, and a slider nothing can pick offers no step. Control: the row beside it is
/// a menu item with no value.
#[test]
fn a_menus_slider_row_is_a_slider_with_its_value_in_words() {
    let snap = live_grid(12, 40, b"hi", Some((0, 2)));
    let slider = |at, words: &str, disabled| GridMenuItem {
        label: "Effort".to_string(),
        checked: None,
        disabled,
        row: 3,
        slider: Some(GridSlider {
            at,
            stops: 3,
            words: words.to_string(),
        }),
    };
    let tree = |first: GridMenuItem| {
        let menu = GridMenu {
            label: "Effort".to_string(),
            epoch: 7,
            items: vec![
                first,
                GridMenuItem {
                    label: "Hide effort".to_string(),
                    checked: None,
                    disabled: false,
                    row: 5,
                    slider: None,
                },
            ],
            selected: Some(0),
            row: 1,
            col: 4,
            width: 30,
            height: 6,
        };
        grid_tree(
            &snap,
            Some(GEOM),
            &GridFrame {
                menu: Some(&menu),
                ..GridFrame::default()
            },
        )
    };
    let update = tree(slider(Some(2), "high", false));
    let rows = node(&update, NodeId(MENU_BASE)).children().to_vec();
    let n = node(&update, rows[0]);
    assert_eq!(n.role(), Role::Slider);
    assert_eq!(n.label(), Some("Effort"));
    assert_eq!(n.value(), Some("high"));
    assert_eq!(
        (
            n.numeric_value(),
            n.min_numeric_value(),
            n.max_numeric_value()
        ),
        (Some(2.0), Some(1.0), Some(3.0))
    );
    assert!(n.supports_action(Action::Increment) && n.supports_action(Action::Decrement));
    assert!(
        !n.supports_action(Action::Click),
        "a slider is stepped, never clicked"
    );
    assert_eq!(update.focus, rows[0], "the highlighted slider");
    let item = node(&update, rows[1]);
    assert_eq!(item.role(), Role::MenuItem, "control: a choice beside it");
    assert!(item.supports_action(Action::Click), "control: a choice is");
    assert_eq!((item.value(), item.numeric_value()), (None, None));
    assert!(!item.supports_action(Action::Increment));
    let update = tree(slider(None, "max", false));
    let n = node(&update, node(&update, NodeId(MENU_BASE)).children()[0]);
    assert_eq!((n.value(), n.numeric_value()), (Some("max"), None));
    let update = tree(slider(Some(1), "medium", true));
    let n = node(&update, node(&update, NodeId(MENU_BASE)).children()[0]);
    assert!(n.is_disabled());
    assert!(!n.supports_action(Action::Increment) && !n.supports_action(Action::Decrement));
}
