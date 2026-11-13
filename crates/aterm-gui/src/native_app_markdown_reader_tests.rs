// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::native_ui::{MarkdownBlockKind, MarkdownBlockSpec, TextViewportSpec, UiContent};
use aterm_grapheme::GraphemeClusters;

/// The view context every reader and editor test renders with.
fn cx<'a>(
    viewport: LogicalRect,
    document: Option<&'a crate::document_store::DocumentSnapshot>,
) -> ViewCx<'a> {
    ViewCx {
        viewport,
        animation_phase_ms: 720,
        motion: ViewMotionCx::default(),
        terminal_font_px: 12.0,
        terminal_scale: 1.0,
        semantic_font: None,
        document,
    }
}

fn command<'a>(commands: &'a [Command], id: &str) -> &'a Command {
    commands
        .iter()
        .find(|command| command.id.as_str() == id)
        .unwrap_or_else(|| panic!("missing command {id}"))
}

#[test]
fn manual_host_diagnostics_are_exact_revision_latest_and_idempotent() {
    let mut documents = crate::test_document_store();
    let document = documents.open(
        "file:///tmp/aterm.toml".to_string(),
        "theme = \"Default\"\n".to_string(),
    );
    let mut runtime = NativeRuntime::new();
    let instance = runtime
        .insert_instance(NativeApp::Editor(Box::new(EditorApp::new(
            document,
            "aterm.toml".to_string(),
        ))))
        .unwrap();
    let view = ViewId::from_stored(7);
    runtime
        .attach_view(
            view,
            instance,
            AppViewState::Editor(Box::new(EditorViewState {
                buffer: Some(crate::native_editor::EditorBufferView::new(
                    document,
                    crate::document_store::DocumentViewId(7),
                    documents.snapshot(document).unwrap().seq,
                )),
                ..EditorViewState::default()
            })),
        )
        .unwrap();
    assert!(runtime.enable_config_editor(document, "theme = \"Default\"\n", 7));
    assert!(runtime.begin_config_host_analysis(document, 7, 3));
    assert!(!runtime.begin_config_host_analysis(document, 7, 3));

    let warning = crate::native_config_language::ConfigDiagnostic {
        bytes: 8..17,
        line: 1,
        column: 9,
        severity: crate::native_config_language::ConfigDiagnosticSeverity::Warning,
        message: "configured theme is unavailable".to_string(),
    };
    let mut analysis = crate::native_config_language::analyze("theme = \"Default\"\n");
    assert!(analysis.merge_host_diagnostics(vec![warning.clone()]));
    assert!(
        !runtime.finish_config_host_analysis(document, 6, 3, analysis.clone()),
        "an older worker completion must not alter current editor assistance"
    );
    assert!(runtime.finish_config_host_analysis(document, 7, 3, analysis.clone()));
    assert!(
        !runtime.finish_config_host_analysis(document, 7, 3, analysis),
        "a replayed exact completion must not duplicate diagnostics"
    );

    runtime.publish_document(document, "theme = \"Nord\"\n", 8, true);
    assert!(runtime.begin_config_host_analysis(document, 8, 3));
    let valid = crate::native_config_language::analyze("theme = \"Nord\"\n");
    assert!(runtime.finish_config_host_analysis(document, 8, 3, valid.clone()));
    assert!(runtime.config_editor_save_error(document).is_none());
    assert!(
        runtime.begin_config_host_analysis(document, 8, 4),
        "same source revision is rechecked after the host environment advances"
    );
    assert!(
        runtime.config_editor_save_error(document).is_some(),
        "the previous host generation cannot authorize Save during revalidation"
    );
    assert!(
        !command(&runtime.commands(instance, view).unwrap(), "editor/save").enabled,
        "the visible command face must agree with the fail-closed host gate"
    );
    assert!(
        !runtime.finish_config_host_analysis(document, 8, 3, valid.clone()),
        "the old generation remains stale even though the document revision matches"
    );
    assert!(runtime.config_editor_save_error(document).is_some());
    assert!(runtime.finish_config_host_analysis(document, 8, 4, valid));
    assert!(runtime.config_editor_save_error(document).is_none());
    assert!(command(&runtime.commands(instance, view).unwrap(), "editor/save").enabled);
    assert!(runtime.enable_config_editor(document, "theme = \"Nord\n", 9));
    assert!(runtime.begin_config_host_analysis(document, 9, 4));
    let invalid = crate::native_config_language::analyze("theme = \"Nord\n");
    assert!(invalid.has_errors());
    assert!(runtime.finish_config_host_analysis(document, 9, 4, invalid));
}

#[test]
fn manual_host_diagnostic_replacement_at_capacity_reports_presentation_damage() {
    let mut documents = crate::test_document_store();
    let document = documents.open(
        "file:///tmp/aterm-capped.toml".to_string(),
        "theme = \"Default\"\n".to_string(),
    );
    let mut runtime = NativeRuntime::new();
    let instance = runtime
        .insert_instance(NativeApp::Editor(Box::new(EditorApp::new(
            document,
            "aterm.toml".to_string(),
        ))))
        .unwrap();
    assert!(runtime.enable_config_editor(document, "theme = \"Default\"\n", 11));
    let mut analysis = crate::native_config_language::analyze("theme = \"Default\"\n");
    analysis.diagnostics = (0..32)
        .map(|index| crate::native_config_language::ConfigDiagnostic {
            bytes: 0..1,
            line: 1,
            column: 1,
            severity: crate::native_config_language::ConfigDiagnosticSeverity::Warning,
            message: format!("pure warning {index}"),
        })
        .collect();
    assert_eq!(analysis.diagnostics.len(), 32);
    assert!(runtime.begin_config_host_analysis(document, 11, 1));
    assert!(runtime.finish_config_host_analysis(document, 11, 1, analysis.clone()));
    let host = crate::native_config_language::ConfigDiagnostic {
        bytes: 8..17,
        line: 1,
        column: 9,
        severity: crate::native_config_language::ConfigDiagnosticSeverity::Error,
        message: "configured theme asset is invalid".to_string(),
    };
    assert!(analysis.merge_host_diagnostics(vec![host.clone()]));
    assert!(runtime.begin_config_host_analysis(document, 11, 2));

    assert!(
        runtime.finish_config_host_analysis(document, 11, 2, analysis.clone()),
        "a late host error must evict one capped warning and invalidate presentation"
    );
    let NativeApp::Editor(editor) = runtime.instances.get(&instance).unwrap() else {
        panic!("Manual editor instance");
    };
    let diagnostics = &editor.config_analysis.as_ref().unwrap().diagnostics;
    assert_eq!(diagnostics.len(), 32);
    assert!(diagnostics.contains(&host));
    assert!(
        !runtime.finish_config_host_analysis(document, 11, 2, analysis),
        "replayed completion remains presentation-inert"
    );
}

#[test]
fn canonical_document_identity_disambiguates_duplicate_basenames_without_splitting_one_uri() {
    let mut documents = crate::test_document_store();
    let one_uri = "file:///Users//alice/one/README.md";
    let two_uri = "file:///Users//alice/two/README.md";
    let one = documents.open(one_uri.to_string(), "one".to_string());
    let two = documents.open(two_uri.to_string(), "two".to_string());
    let mut runtime = NativeRuntime::new();
    let second_view_one = runtime
        .insert_instance(NativeApp::Editor(Box::new(EditorApp::new_with_uri(
            one,
            "README.md".to_string(),
            one_uri.to_string(),
        ))))
        .unwrap();
    let editor_one = runtime
        .insert_instance(NativeApp::Editor(Box::new(EditorApp::new_with_uri(
            one,
            "README.md".to_string(),
            one_uri.to_string(),
        ))))
        .unwrap();
    let editor_two = runtime
        .insert_instance(NativeApp::Editor(Box::new(EditorApp::new_with_uri(
            two,
            "README.md".to_string(),
            two_uri.to_string(),
        ))))
        .unwrap();
    let views = [
        (ViewId::from_stored(101), second_view_one, AppKind::Editor),
        (ViewId::from_stored(102), editor_one, AppKind::Editor),
        (ViewId::from_stored(103), editor_two, AppKind::Editor),
    ];
    for (view, instance, kind) in views {
        let state = match kind {
            AppKind::Editor => AppViewState::Editor(Box::default()),
            AppKind::Markdown
            | AppKind::Mail
            | AppKind::About
            | AppKind::Settings
            | AppKind::SystemState
            | AppKind::Recovery
            | AppKind::Allocation
            | AppKind::Workers
            | AppKind::Tables => {
                unreachable!()
            }
        };
        runtime.attach_view(view, instance, state).unwrap();
    }

    let changed = runtime.disambiguate_document_titles();
    assert_eq!(changed.len(), 3);
    let identities = runtime.document_identities();
    for (_, _, title, uri) in &identities {
        if uri == one_uri {
            assert_eq!(title, "README.md — one");
        } else if uri == two_uri {
            assert_eq!(title, "README.md — two");
        } else {
            panic!("unexpected canonical URI {uri}");
        }
    }
    assert_eq!(
        identities
            .iter()
            .filter(|(_, _, _, uri)| uri == one_uri)
            .count(),
        2,
        "explicit views of one canonical URI are not false duplicates"
    );
    assert!(runtime.disambiguate_document_titles().is_empty());

    runtime.remove_view(ViewId::from_stored(103));
    runtime.remove_instance(editor_two);
    let reverted = runtime.disambiguate_document_titles();
    assert_eq!(reverted.len(), 2);
    assert!(
        runtime
            .document_identities()
            .iter()
            .all(|(_, _, title, uri)| uri != one_uri || title == "README.md")
    );
    assert_eq!(runtime.document_id(second_view_one), Some(one));
    assert_eq!(runtime.document_id(editor_one), Some(one));
}

#[test]
fn duplicate_title_suffix_is_shortest_unique_unicode_parent_and_bounded() {
    let group = vec![
        (
            AppInstanceId::from_stored(1),
            "file:///tmp/%E6%97%A5%E6%9C%AC/notes.md".to_string(),
        ),
        (
            AppInstanceId::from_stored(2),
            "file:///tmp/other/notes.md".to_string(),
        ),
    ];
    assert_eq!(shortest_unique_document_parent(&group[0].1, &group), "日本");
    assert_eq!(
        shortest_unique_document_parent(&group[1].1, &group),
        "other"
    );
    let long = "a".repeat(200);
    let bounded = bounded_document_parent_label(&long, "file:///long/notes.md");
    assert!(bounded.graphemes().count() <= 44);
    assert!(bounded.contains('…'));
}

/// Exercise the reusable reader directly; document tabs are always Editor instances.
struct ReaderFixture {
    app: MarkdownApp,
    state: MarkdownViewState,
    instance: AppInstanceId,
    view: ViewId,
    next_operation: u64,
    service_generations: BTreeMap<ServiceId, u64>,
}

impl ReaderFixture {
    fn check(&self, instance: AppInstanceId, view: ViewId) -> Result<(), RuntimeError> {
        if instance != self.instance {
            return Err(RuntimeError::UnknownInstance(instance));
        }
        if view != self.view {
            return Err(RuntimeError::UnknownView(view));
        }
        Ok(())
    }

    fn app(&self, instance: AppInstanceId) -> Option<&MarkdownApp> {
        (instance == self.instance).then_some(&self.app)
    }

    fn view_state(&self, view: ViewId) -> Option<&MarkdownViewState> {
        (view == self.view).then_some(&self.state)
    }

    fn dispatch(
        &mut self,
        instance: AppInstanceId,
        view: ViewId,
        event: AppEvent,
    ) -> Result<DispatchOutcome, RuntimeError> {
        self.check(instance, view)?;
        let mut cx = UpdateCx {
            instance,
            view,
            view_generation: 1,
            next_operation: &mut self.next_operation,
            service_generations: &mut self.service_generations,
            effects: Vec::new(),
        };
        let result = self.app.update(&mut self.state, event, &mut cx);
        Ok(DispatchOutcome {
            result,
            effects: cx.effects,
        })
    }

    fn commands(
        &self,
        instance: AppInstanceId,
        view: ViewId,
    ) -> Result<Vec<Command>, RuntimeError> {
        self.check(instance, view)?;
        let mut commands = Vec::new();
        self.app.commands(&self.state, &mut commands);
        Ok(commands)
    }

    fn render(
        &self,
        instance: AppInstanceId,
        view: ViewId,
        cx: &ViewCx<'_>,
    ) -> Result<UiTree, RuntimeError> {
        self.check(instance, view)?;
        let mut tree = self.app.view(&self.state, cx);
        tree.apply_interaction(
            self.state.common.last_focus.as_ref(),
            self.state.common.hovered.as_ref(),
            self.state.common.pressed.as_ref(),
            self.state.common.focus_visible,
        );
        Ok(tree)
    }
}

fn markdown_runtime(source: &str) -> (ReaderFixture, AppInstanceId, ViewId) {
    let mut documents = crate::test_document_store();
    let document = documents.open("file:///Guide.md".to_string(), source.to_string());
    let instance = AppInstanceId::from_stored(1);
    let view = ViewId::from_stored(1);
    let runtime = ReaderFixture {
        app: MarkdownApp::new(document, "Guide.md".to_string(), source),
        state: MarkdownViewState::default(),
        instance,
        view,
        next_operation: 1,
        service_generations: BTreeMap::new(),
    };
    (runtime, instance, view)
}

/// The reader's progress counts what it has shown: paging from the top of a
/// document to its end the share read never falls, and Next is disabled exactly
/// where it reads 100% (the end shown), in the preview and in the source pane, at 1x
/// and 2x, narrow and wide. Before, the share counted from the start of the top
/// block's source line, so the Japanese document a review paged through read "78%
/// read" at its last row; and Next stayed on at "100% read" until the last row was
/// at the top, paging a document that fits one page onto a page of "did." alone.
#[test]
fn the_reader_reads_100_percent_at_the_end_of_a_document() {
    let _scale = RestoreAppearance::at(1.0);
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let source = "# 設定ファイルの読み込み\n\n\
        設定ファイルを読み込めませんでした。テーマ「ドラキュラ」は見つかりません。\
        コーヒーブレイク中にアップデートをダウンロードしています、ちょっと待ってください。\n\n\
        ```\n設定ファイルを読み込めませんでした。テーマ「ドラキュラ」は見つかりません。\n```\n\n\
        The quick brown fox jumps over the lazy dog, and aarch64-apple-darwin wraps \
        at its spaces as it always did.\n";
    let mut documents = crate::test_document_store();
    let document = documents.open("file:///ja.md".to_string(), source.to_string());
    let app = MarkdownApp::new(document, "ja.md".to_string(), source);
    let percent = |status: &str| -> usize {
        let end = status.find("% read").expect("the share read");
        let digits = status[..end]
            .rsplit(|c: char| !c.is_ascii_digit())
            .next()
            .unwrap();
        digits.parse().unwrap()
    };
    let mut ended_below_100_before = false;
    for scale in [1.0_f32, 2.0] {
        let _scale = RestoreAppearance::at(scale);
        for mode in [MarkdownViewMode::Preview, MarkdownViewMode::Source] {
            for (width, height) in [(444.0_f32, 574.0_f32), (934.0, 574.0)] {
                let context = format!("{mode:?} {width}x{height} at {scale}x");
                let mut view = MarkdownViewState {
                    mode,
                    ..MarkdownViewState::default()
                };
                let mut last = 0;
                for step in 0.. {
                    assert!(step < 500, "{context}: the pages end");
                    let (geometry, header) = app.reader_layout(&view, width, height);
                    let read = percent(&header.status_lines.join(" "));
                    assert!(read >= last, "{context}: {read}% after {last}%");
                    last = read;
                    let next = app.page_target(&view, &geometry, 1);
                    let at_end = next == MarkdownApp::location(&view);
                    assert_eq!(
                        at_end,
                        read == 100,
                        "{context}: Next is off exactly where {read}% is read"
                    );
                    if at_end {
                        ended_below_100_before |= view
                            .source_anchor
                            .saturating_mul(100)
                            .checked_div(app.parsed.source_len)
                            .is_some_and(|old| old < 100);
                        break;
                    }
                    view.source_anchor = next.source_anchor;
                    view.visual_row = next.visual_row;
                }
            }
        }
    }
    assert!(ended_below_100_before, "the negative control");
}

/// THE READER PAINTS EVERY ROW OF EVERY BLOCK, AND ITS HEADER WHOLE, AT
/// EVERY TEXT SIZE: the reader lays each block out with the painter's
/// own type and wrap ([`crate::native_markdown::block_frame`]), so a
/// block wholly inside the preview paints all its rows (none elided or
/// sliced), wheeling row by row from the top to the end shows every row
/// of every block, and the end leaves the last row in view. The header
/// measures its status at the text size and gives it rows of its own
/// where it does not fit beside Prev and Next; the wide outline wraps.
/// At 2x the fixed 1x estimate this replaced elided every block and left
/// rows no scroll could reach, and the status lost its words.
#[test]
fn the_reader_paints_every_row_and_its_header_whole_at_every_text_size() {
    use crate::native_appearance::{
        AppearancePreferences, current_preferences, install_preferences,
    };
    struct RestoreAppearance(AppearancePreferences);
    impl Drop for RestoreAppearance {
        fn drop(&mut self) {
            install_preferences(self.0);
        }
    }
    let _restore = RestoreAppearance(current_preferences());
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let source = "# 設定ファイルの読み込み\n\n\
        設定ファイルを読み込めませんでした。テーマ「ドラキュラ」は見つかりません。\
        コーヒーブレイク中にアップデートをダウンロードしています、ちょっと待ってください。\n\n\
        ```\n設定ファイルを読み込めませんでした。テーマ「ドラキュラ」は見つかりません。\n```\n\n\
        ## A second section whose heading runs long enough to wrap\n\n\
        - a list item that wraps across the measure of a phone-width reader\n\n\
        > A quoted paragraph that also wraps when the reader is narrow enough.\n\n\
        ```rust\nfn main() { println!(\"a line of code long enough to wrap\"); }\n```\n\n\
        | Column | Another column |\n|---|---|\n| 1 | a cell long enough to wrap the row |\n\n\
        The quick brown fox jumps over the lazy dog, and aarch64-apple-darwin wraps \
        at its spaces as it always did.\n";
    let field = |line: &str, name: &str| -> String {
        let at = line.find(&format!(" {name}=")).expect(name) + name.len() + 2;
        line[at..].split(' ').next().unwrap().to_string()
    };
    for scale in [0.85_f32, 1.0, 1.5, 2.0] {
        install_preferences(AppearancePreferences {
            text_scale: scale,
            ..AppearancePreferences::default()
        });
        for (width, height) in [(444.0_f32, 574.0_f32), (724.0, 476.0), (934.0, 574.0)] {
            let context = format!("{width}x{height} at {scale}x");
            let viewport = LogicalRect::new(0.0, 0.0, width, height);
            let (mut runtime, instance, view) = markdown_runtime(source);
            let parsed = crate::native_markdown::parse(source);
            let blocks = parsed.blocks.len();
            let mut seen: std::collections::BTreeMap<
                usize,
                (usize, std::collections::BTreeSet<usize>),
            > = std::collections::BTreeMap::new();
            let mut previous = None;
            for _ in 0..2_000 {
                let compiled = runtime
                    .render(instance, view, &cx(viewport, None))
                    .unwrap()
                    .compile(viewport)
                    .unwrap();
                assert!(
                    compiled.fit_failures().is_empty(),
                    "{context}: {:?}",
                    compiled.fit_failures()
                );
                for line in compiled
                    .paint_audit_lines()
                    .into_iter()
                    .filter(|line| line.starts_with("paint-markdown key=\"markdown/block/"))
                {
                    let key = field(&line, "key");
                    let index = key
                        .trim_matches('"')
                        .rsplit('/')
                        .next()
                        .unwrap()
                        .parse::<usize>()
                        .unwrap();
                    let node = compiled
                        .paint
                        .iter()
                        .find(|node| format!("{:?}", node.key.as_str()) == key)
                        .expect("the block's node");
                    let (row, total) = field(&line, "visual-row")
                        .split_once('/')
                        .map(|(row, total)| {
                            (
                                row.parse::<usize>().unwrap(),
                                total.parse::<usize>().unwrap(),
                            )
                        })
                        .unwrap();
                    let wrapped = field(&line, "wrapped-lines").parse::<usize>().unwrap();
                    let visible = field(&line, "visible-lines").parse::<usize>().unwrap();
                    assert!(
                        node.rect.bottom() <= node.clip.bottom() + 0.01
                            && node.rect.y >= node.clip.y - 0.01,
                        "{context}: the preview shows whole blocks and rows: {line}"
                    );
                    assert_eq!(
                        field(&line, "elided"),
                        "false",
                        "{context}: a block paints every row it has room for: {line}"
                    );
                    let frame = crate::native_markdown::block_frame(
                        crate::native_markdown::BlockShape::of(&parsed.blocks[index]),
                        crate::native_ui::reader_type(),
                    );
                    if let Some(frame) = frame {
                        let holds = frame.rows_in(node.rect.height);
                        assert_eq!(wrapped, (total - row).min(holds), "{context}: {line}");
                    }
                    let entry = seen.entry(index).or_insert((total, Default::default()));
                    assert_eq!(entry.0, total, "{context}: block {index} keeps its rows");
                    entry.1.extend(row..row + visible);
                }
                let reader = runtime.view_state(view).unwrap();
                let state = (reader.source_anchor, reader.visual_row);
                if previous == Some(state) {
                    break;
                }
                previous = Some(state);
                runtime
                    .dispatch(
                        instance,
                        view,
                        AppEvent::MarkdownScroll {
                            lines: 1,
                            viewport_width: width,
                            viewport_height: height,
                        },
                    )
                    .unwrap();
            }
            assert_eq!(seen.len(), blocks, "{context}: every block was painted");
            for (index, (total, rows)) in &seen {
                assert_eq!(
                    rows.len(),
                    *total,
                    "{context}: every row of block {index} was in view"
                );
            }
        }
    }
}

/// THE WIDE OUTLINE FITS ITS COLUMN BY ITS ROWS' HEIGHTS AND READS AS A
/// HIERARCHY: each destination is as tall as the lines its name wraps to,
/// and the outline takes the sections nearest the current one (and the
/// links and images in view beside them) only while their heights fit
/// the column, so no destination runs past its bottom or is cut by it.
/// Every line of a name starts at the row's lead, indented by the
/// section's depth, never centred. Counted as single-height rows, the
/// wrapped destinations ran past the column and the last was cut.
#[test]
fn the_wide_outline_fits_its_column_by_its_rows_heights_and_reads_as_a_hierarchy() {
    use crate::native_appearance::{
        AppearancePreferences, current_preferences, install_preferences,
    };
    struct RestoreAppearance(AppearancePreferences);
    impl Drop for RestoreAppearance {
        fn drop(&mut self) {
            install_preferences(self.0);
        }
    }
    let _restore = RestoreAppearance(current_preferences());
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let mut source = String::from(
        "# A philosophy of naming whose title runs long enough to wrap\n\n\
         See [the reroute design document with a long name](#reroute), \
         [the second linked design note](https://example.com/a) and \
         ![a diagram of every lane the toolchain takes](lanes.png).\n\n",
    );
    for section in 1..=14 {
        let hashes = if section % 3 == 0 { "###" } else { "##" };
        source.push_str(&format!(
            "{hashes} {section}. A section heading long enough to wrap in the outline\n\n\
             Body text for section {section}.\n\n"
        ));
    }
    let mut saw_deep = false;
    for scale in [1.0_f32, 2.0] {
        install_preferences(AppearancePreferences {
            text_scale: scale,
            ..AppearancePreferences::default()
        });
        for (width, height) in [(1_074.0_f32, 684.0_f32), (934.0, 420.0)] {
            let context = format!("{width}x{height} at {scale}x");
            let viewport = LogicalRect::new(0.0, 0.0, width, height);
            let (runtime, instance, view) = markdown_runtime(&source);
            let compiled = runtime
                .render(instance, view, &cx(viewport, None))
                .unwrap()
                .compile(viewport)
                .unwrap();
            assert!(
                compiled.fit_failures().is_empty(),
                "{context}: {:?}",
                compiled.fit_failures()
            );
            let column = compiled
                .paint
                .iter()
                .find(|node| node.key.as_str() == "markdown/outline")
                .expect("the wide outline")
                .rect;
            let destinations = compiled
                .paint
                .iter()
                .filter(|node| {
                    let key = node.key.as_str();
                    key.starts_with("markdown/outline-item/")
                        || key.starts_with("markdown/link-item/")
                        || key.starts_with("markdown/image-item/")
                })
                .collect::<Vec<_>>();
            assert!(
                destinations
                    .iter()
                    .any(|node| node.key.as_str().starts_with("markdown/outline-item/")),
                "{context}: the sections nearest the current one"
            );
            let prims = compiled.tray(aterm_render::Theme::default(), 13.0).prims;
            for node in &destinations {
                assert!(
                    node.rect.bottom() <= column.bottom() - 16.0 + 0.01 && !node.clipped(),
                    "{context}: {} runs past the outline's column {column:?}: {:?}",
                    node.key.as_str(),
                    node.rect
                );
                let UiContent::Button(control) = &node.content else {
                    panic!("{context}: a destination is a button");
                };
                let lines = control
                    .spec
                    .visual_label
                    .as_deref()
                    .unwrap_or(&control.spec.label)
                    .split('\n')
                    .collect::<Vec<_>>();
                let depth = lines[0].len() - lines[0].trim_start().len();
                assert!(
                    lines
                        .iter()
                        .all(|line| line.len() - line.trim_start().len() == depth),
                    "{context}: every line of {} keeps its depth: {lines:?}",
                    node.key.as_str()
                );
                let starts = prims
                    .iter()
                    .filter_map(|prim| match prim {
                        crate::widget::DrawPrim::Text { x, baseline, .. }
                            if *baseline > node.rect.y
                                && *baseline < node.rect.bottom()
                                && *x >= node.rect.x
                                && *x < node.rect.right() =>
                        {
                            Some(*x)
                        }
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                assert_eq!(
                    starts.len(),
                    lines.len(),
                    "{context}: {}",
                    node.key.as_str()
                );
                assert!(
                    starts.iter().all(|x| (*x - starts[0]).abs() < 0.01)
                        && starts[0] < node.rect.x + 16.0,
                    "{context}: {} starts every line at its lead: {starts:?}",
                    node.key.as_str()
                );
            }
            if let Some(deep) = destinations.iter().find_map(|node| match &node.content {
                UiContent::Button(control) if control.spec.label.starts_with("3. ") => {
                    control.spec.visual_label.clone()
                }
                _ => None,
            }) {
                assert!(deep.starts_with("    3. "), "{context}: {deep:?}");
                saw_deep = true;
            }
        }
    }
    assert!(saw_deep, "a third-level section was in view");
}

#[test]
fn markdown_navigation_metadata_drives_real_per_view_history() {
    let source = (0..12)
        .map(|index| format!("Paragraph {index}.\n\n"))
        .collect::<String>();
    let (mut runtime, instance, view) = markdown_runtime(&source);

    let initial = runtime.commands(instance, view).unwrap();
    assert!(!command(&initial, "markdown/back").enabled);
    assert!(!command(&initial, "markdown/forward").enabled);
    runtime
        .dispatch(instance, view, AppEvent::ScrollLines(3))
        .unwrap();
    let after_scroll = runtime.commands(instance, view).unwrap();
    assert!(
        command(&after_scroll, "markdown/back").enabled,
        "Back becomes actionable"
    );
    assert!(
        !command(&after_scroll, "markdown/forward").enabled,
        "Forward has no future entry yet"
    );

    runtime
        .dispatch(
            instance,
            view,
            AppEvent::Action(ActionInvocation {
                id: ActionId::new("markdown/back"),
                value: None,
            }),
        )
        .unwrap();
    let state = runtime.view_state(view).unwrap();
    assert_eq!(state.source_anchor, 0);
    let after_back = runtime.commands(instance, view).unwrap();
    assert!(!command(&after_back, "markdown/back").enabled);
    assert!(
        command(&after_back, "markdown/forward").enabled,
        "Forward replays the visited anchor"
    );
}

#[test]
fn thousand_line_paragraph_reaches_tail_in_preview_and_source_and_after_resize() {
    let source = (0..1_000)
        .map(|line| format!("line-{line:04} carries readable paragraph words\n"))
        .collect::<String>();
    let mut documents = crate::test_document_store();
    let document = documents.open("file:///Long.md".to_string(), source.clone());
    let snapshot = documents.snapshot(document).unwrap();
    let (mut runtime, instance, view) = markdown_runtime(&source);

    let page = |runtime: &mut ReaderFixture, height: f32| {
        runtime
            .dispatch(
                instance,
                view,
                AppEvent::MarkdownPage {
                    direction: 1,
                    viewport_width: 320.0,
                    viewport_height: height,
                },
            )
            .unwrap();
    };
    page(&mut runtime, 320.0);
    let short_page_row = runtime.view_state(view).unwrap().visual_row;
    runtime
        .dispatch(
            instance,
            view,
            AppEvent::MarkdownScroll {
                lines: -10_000,
                viewport_width: 320.0,
                viewport_height: 568.0,
            },
        )
        .unwrap();
    page(&mut runtime, 568.0);
    let tall_page_row = runtime.view_state(view).unwrap().visual_row;
    assert!(tall_page_row > short_page_row, "page size tracks resize");

    runtime
        .dispatch(
            instance,
            view,
            AppEvent::MarkdownScroll {
                lines: 10_000,
                viewport_width: 320.0,
                viewport_height: 568.0,
            },
        )
        .unwrap();
    let preview_row = runtime.view_state(view).unwrap().visual_row;
    assert!(
        preview_row > 100,
        "single paragraph has a reachable visual tail"
    );

    runtime
        .dispatch(
            instance,
            view,
            AppEvent::Action(ActionInvocation {
                id: ActionId::new("markdown/mode/source"),
                value: None,
            }),
        )
        .unwrap();
    runtime
        .dispatch(
            instance,
            view,
            AppEvent::MarkdownScroll {
                lines: 10_000,
                viewport_width: 320.0,
                viewport_height: 568.0,
            },
        )
        .unwrap();
    let viewport = LogicalRect::new(0.0, 0.0, 320.0, 568.0);
    let source_ui = runtime
        .render(instance, view, &cx(viewport, Some(&snapshot)))
        .unwrap()
        .compile(viewport)
        .unwrap();
    assert!(source_ui.paint.iter().any(|node| {
        matches!(&node.content, UiContent::MarkdownBlock(spec)
            if node.key.as_str() == "markdown/source"
                && spec.text.contains("line-0999"))
    }));
}

#[test]
fn markdown_reader_compiles_typed_blocks_and_hides_markup() {
    let source = "<!-- reader metadata -->\n# **Native apps**\n\nReadable `body` copy wraps into a calm measure.\n\n- first\n\n> quoted\n\n```rust\nfn main() {}\n```\n\n| A | B |\n|---|---|\n| 1 | 2 |\n";
    let (runtime, instance, view) = markdown_runtime(source);
    let tree = runtime
        .render(
            instance,
            view,
            &cx(LogicalRect::new(0.0, 0.0, 1_000.0, 1_400.0), None),
        )
        .unwrap();
    let compiled = tree
        .compile(LogicalRect::new(0.0, 0.0, 1_000.0, 1_400.0))
        .unwrap();
    assert!(compiled.paint.iter().any(|node| matches!(
        &node.content,
        UiContent::MarkdownBlock(MarkdownBlockSpec {
            kind: MarkdownBlockKind::Code { language: Some(language) },
            ..
        }) if language == "rust"
    )));
    assert!(compiled.paint.iter().any(|node| matches!(
        &node.content,
        UiContent::MarkdownBlock(MarkdownBlockSpec {
            kind: MarkdownBlockKind::Quote,
            ..
        })
    )));
    let prims = compiled.tray(aterm_render::Theme::default(), 13.0).prims;
    assert!(compiled.semantics.iter().all(|node| {
        !node.label.contains("reader metadata")
            && !node.label.contains("**")
            && !node.label.contains('`')
    }));
    assert!(prims.iter().all(|primitive| {
        !matches!(primitive, crate::widget::DrawPrim::Text { s, .. }
            if s.contains("reader metadata") || s.contains("**"))
    }));
    compiled.validate_parity().unwrap();
}

#[test]
fn markdown_select_all_copy_and_cancel_are_exact_source_actions() {
    let source = "<!-- reader-hidden -->\n# **Héllo**\n\nA [jump](#next).\n\n## Next\nBody 🦀\n";
    let (mut runtime, instance, view) = markdown_runtime(source);

    runtime
        .dispatch(
            instance,
            view,
            AppEvent::TextInput(TextInputEvent::SelectAll),
        )
        .unwrap();
    let state = runtime.view_state(view).unwrap();
    assert_eq!(state.selection, Some(0..source.len()));
    let selected_commands = runtime.commands(instance, view).unwrap();
    assert!(command(&selected_commands, "markdown/copy").enabled);
    assert!(!command(&selected_commands, "markdown/select-all").enabled);

    let copied = runtime
        .dispatch(
            instance,
            view,
            AppEvent::Action(ActionInvocation {
                id: ActionId::new("markdown/copy"),
                value: None,
            }),
        )
        .unwrap();
    assert!(matches!(
        copied.effects.as_slice(),
        [AppEffect::Clipboard {
            request: ClipboardRequest::CopyDocumentRange { document: _, range, sensitive: false },
            ..
        }] if range == &(0..source.len())
    ));

    runtime
        .dispatch(instance, view, AppEvent::TextInput(TextInputEvent::Cancel))
        .unwrap();
    let cleared = runtime.commands(instance, view).unwrap();
    assert!(!command(&cleared, "markdown/copy").enabled);
    assert!(command(&cleared, "markdown/select-all").enabled);
}

#[test]
fn markdown_source_mode_and_its_block_selection_are_real_view_state() {
    let source = "# **Guide**\n\nBody with ![local](assets/pic.png).\n";
    let (mut runtime, instance, view) = markdown_runtime(source);
    let document = runtime.app(instance).unwrap().document;
    let snapshot = crate::document_store::DocumentSnapshot {
        id: document,
        seq: aterm_buffer::Seq(1),
        text: std::sync::Arc::from(source),
    };
    let viewport = LogicalRect::new(0.0, 0.0, 900.0, 640.0);

    runtime
        .dispatch(
            instance,
            view,
            AppEvent::Action(ActionInvocation {
                id: ActionId::new("markdown/mode/source"),
                value: None,
            }),
        )
        .unwrap();
    assert!(matches!(
        runtime.view_state(view),
        Some(MarkdownViewState {
            mode: MarkdownViewMode::Source,
            ..
        })
    ));
    let source_ui = runtime
        .render(instance, view, &cx(viewport, Some(&snapshot)))
        .unwrap()
        .compile(viewport)
        .unwrap();
    let source_node = source_ui.semantic(&UiKey::new("markdown/source")).unwrap();
    assert!(source_node.label.contains("**Guide**"));
    assert!(
        source_node
            .action
            .as_ref()
            .is_some_and(|action| action.as_str().starts_with("markdown/select-range/"))
    );

    runtime
        .dispatch(
            instance,
            view,
            AppEvent::Action(ActionInvocation {
                id: source_node.action.clone().unwrap(),
                value: None,
            }),
        )
        .unwrap();
    assert!(matches!(
        runtime.view_state(view),
        Some(state) if state.selection == Some(0..source.len())
    ));
}

#[test]
fn markdown_image_actions_select_local_source_and_gate_remote_open() {
    let source = "![local](assets/pic.png) ![remote](https://example.com/pic.png)";
    let (mut runtime, instance, view) = markdown_runtime(source);
    let local = runtime
        .dispatch(
            instance,
            view,
            AppEvent::Action(ActionInvocation {
                id: ActionId::new("markdown/image/0"),
                value: None,
            }),
        )
        .unwrap();
    assert!(local.effects.is_empty());
    assert!(matches!(
        runtime.view_state(view),
        Some(state)
            if state.selection.as_ref().is_some_and(|range| &source[range.clone()] == "![local](assets/pic.png)")
    ));

    let remote = runtime
        .dispatch(
            instance,
            view,
            AppEvent::Action(ActionInvocation {
                id: ActionId::new("markdown/image/1"),
                value: None,
            }),
        )
        .unwrap();
    assert!(matches!(
        remote.effects.as_slice(),
        [AppEffect::OpenExternal {
            request: ExternalOpenRequest { uri, user_initiated: true },
            ..
        }] if uri == "https://example.com/pic.png"
    ));
}

#[test]
fn markdown_outline_and_links_use_typed_policy_without_local_file_access() {
    let source = "# Start\n\n[jump](#next) [web](https://example.com/docs) [file](README.md) [bad](javascript:alert)\n\n## Next\nDone\n";
    let (mut runtime, instance, view) = markdown_runtime(source);
    let initial_commands = runtime.commands(instance, view).unwrap();
    assert!(command(&initial_commands, "markdown/link/0").enabled);
    assert!(command(&initial_commands, "markdown/link/1").enabled);
    assert!(!command(&initial_commands, "markdown/link/2").enabled);
    assert!(!command(&initial_commands, "markdown/link/3").enabled);

    let outline = runtime
        .dispatch(
            instance,
            view,
            AppEvent::Action(ActionInvocation {
                id: ActionId::new("markdown/outline/1"),
                value: None,
            }),
        )
        .unwrap();
    assert!(outline.effects.is_empty());
    let next_anchor =
        crate::native_markdown::local_anchor(&crate::native_markdown::parse(source), "#next")
            .unwrap();
    assert!(matches!(
        runtime.view_state(view),
        Some(state) if state.source_anchor == next_anchor
    ));

    let local = runtime
        .dispatch(
            instance,
            view,
            AppEvent::Action(ActionInvocation {
                id: ActionId::new("markdown/link/0"),
                value: None,
            }),
        )
        .unwrap();
    assert!(local.effects.is_empty());
    assert!(matches!(
        runtime.view_state(view),
        Some(state) if state.source_anchor == next_anchor
    ));

    let external = runtime
        .dispatch(
            instance,
            view,
            AppEvent::Action(ActionInvocation {
                id: ActionId::new("markdown/link/1"),
                value: None,
            }),
        )
        .unwrap();
    assert!(matches!(
        external.effects.as_slice(),
        [AppEffect::OpenExternal {
            request: ExternalOpenRequest { uri, user_initiated: true },
            ..
        }] if uri == "https://example.com/docs"
    ));

    for denied in [2, 3] {
        let outcome = runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(ActionInvocation {
                    id: ActionId::new(format!("markdown/link/{denied}")),
                    value: None,
                }),
            )
            .unwrap();
        assert!(outcome.effects.is_empty(), "link {denied} must stay inert");
    }
}

#[test]
fn markdown_reader_is_responsive_and_all_observers_stay_in_bounds() {
    let source = (0..40)
        .map(|index| format!("## Section {index}\n\nBody [jump](#section-0).\n\n"))
        .collect::<String>();
    let (runtime, instance, view) = markdown_runtime(&source);
    for (width, height, expects_outline) in [
        (1_120.0, 720.0, true),
        (800.0, 620.0, false),
        (420.0, 360.0, false),
    ] {
        let viewport = LogicalRect::new(0.0, 0.0, width, height);
        let compiled = runtime
            .render(instance, view, &cx(viewport, None))
            .unwrap()
            .compile(viewport)
            .unwrap();
        assert_eq!(
            compiled.semantic(&UiKey::new("markdown/outline")).is_some(),
            expects_outline
        );
        assert!(compiled.semantic(&UiKey::new("markdown/header")).is_some());
        assert!(compiled.semantics.iter().all(|node| {
            node.rect.x >= 0.0
                && node.rect.y >= 0.0
                && node.rect.right() <= width
                && node.rect.bottom() <= height
        }));
        assert!(compiled.hits.iter().all(|hit| {
            hit.rect.x >= 0.0
                && hit.rect.y >= 0.0
                && hit.rect.right() <= width
                && hit.rect.bottom() <= height
        }));
        compiled.validate_parity().unwrap();
    }
}

#[test]
fn markdown_frame_commands_and_semantics_remain_bounded_for_large_outline() {
    let source = (0..5_000)
        .map(|index| format!("# Heading {index}\n\nParagraph {index}.\n\n"))
        .collect::<String>();
    let (runtime, instance, view) = markdown_runtime(&source);
    let commands = runtime.commands(instance, view).unwrap();
    assert!(commands.len() <= 47, "command materialization is capped");

    let viewport = LogicalRect::new(0.0, 0.0, 1_000.0, 700.0);
    let before = runtime
        .render(instance, view, &cx(viewport, None))
        .unwrap()
        .compile(viewport)
        .unwrap();
    assert!(
        before.semantics.len() < 96,
        "one frame stays viewport-bounded"
    );
}

#[test]
fn markdown_block_copy_is_utf8_safe_and_capped() {
    let source = format!("```text\n{}\n```", "🦀".repeat(100_000));
    let parsed = crate::native_markdown::parse(&source);
    let code = parsed
        .blocks
        .iter()
        .find(|block| {
            matches!(
                block,
                crate::native_markdown::MarkdownBlock::CodeBlock { .. }
            )
        })
        .unwrap();
    let bounded = crate::native_markdown::block_paint_text(code).into_owned();
    assert!(bounded.len() <= 128 * 1024);
    assert!(bounded.ends_with('…'));
    assert!(bounded.is_char_boundary(bounded.len()));
}

#[test]
fn markdown_header_selection_button_toggles_and_is_disabled_when_empty() {
    let (mut runtime, instance, view) = markdown_runtime("# Start\n\nBody\n");
    let viewport = LogicalRect::new(0.0, 0.0, 474.0, 468.0);
    let initial = runtime
        .render(instance, view, &cx(viewport, None))
        .unwrap()
        .compile(viewport)
        .unwrap();
    let selection = initial
        .semantic(&UiKey::new("markdown/selection-button"))
        .unwrap();
    assert_eq!(
        selection.action.as_ref().map(ActionId::as_str),
        Some("markdown/select-all")
    );
    assert!(initial.hits.iter().any(|hit| {
        hit.key.as_str() == "markdown/selection-button"
            && hit.action.as_str() == "markdown/select-all"
    }));

    runtime
        .dispatch(
            instance,
            view,
            AppEvent::Action(ActionInvocation {
                id: ActionId::new("markdown/select-all"),
                value: None,
            }),
        )
        .unwrap();
    let selected = runtime
        .render(instance, view, &cx(viewport, None))
        .unwrap()
        .compile(viewport)
        .unwrap();
    let selection = selected
        .semantic(&UiKey::new("markdown/selection-button"))
        .unwrap();
    assert_eq!(
        selection.action.as_ref().map(ActionId::as_str),
        Some("markdown/clear-selection")
    );

    let (runtime, instance, view) = markdown_runtime("\n");
    let empty = runtime
        .render(instance, view, &cx(viewport, None))
        .unwrap()
        .compile(viewport)
        .unwrap();
    assert!(empty.semantic(&UiKey::new("markdown/empty")).is_some());
    assert_eq!(
        empty
            .semantic(&UiKey::new("markdown/selection-button"))
            .and_then(|node| node.state)
            .map(|state| state.enabled),
        Some(false)
    );
    empty.validate_parity().unwrap();
}

#[test]
fn markdown_page_controls_are_semantic_exact_and_advance_read_status() {
    let source = (0..80)
        .map(|index| format!("Paragraph {index} has enough words for a reading row.\n\n"))
        .collect::<String>();
    let (mut runtime, instance, view) = markdown_runtime(&source);
    let viewport = LogicalRect::new(0.0, 0.0, 800.0, 520.0);
    let render = |runtime: &ReaderFixture| {
        runtime
            .render(instance, view, &cx(viewport, None))
            .unwrap()
            .compile(viewport)
            .unwrap()
    };

    let initial = render(&runtime);
    let previous = initial
        .semantic(&UiKey::new("markdown/previous-page-button"))
        .unwrap();
    let next = initial
        .semantic(&UiKey::new("markdown/next-page-button"))
        .unwrap();
    assert!(previous.state.is_some_and(|state| !state.enabled));
    assert!(next.state.is_some_and(|state| state.enabled));
    let next_action = next.action.clone().expect("next page action");
    assert!(next_action.as_str().starts_with("markdown/page/"));
    assert!(initial.hits.iter().any(|hit| {
        hit.key.as_str() == "markdown/next-page-button" && hit.action == next_action
    }));

    runtime
        .dispatch(
            instance,
            view,
            AppEvent::Action(ActionInvocation {
                id: next_action,
                value: None,
            }),
        )
        .unwrap();
    assert!(matches!(
        runtime.view_state(view),
        Some(state) if state.source_anchor > 0
    ));
    let advanced = render(&runtime);
    assert!(
        advanced
            .semantic(&UiKey::new("markdown/previous-page-button"))
            .and_then(|node| node.state)
            .is_some_and(|state| state.enabled)
    );
    advanced.validate_parity().unwrap();
}

/// Page keys and the visible buttons move through the same reading body,
/// not the whole tab.
#[test]
fn markdown_page_keys_reach_the_rendered_button_targets_in_every_mode() {
    let paragraph = "A paragraph with enough words to wrap differently in a narrow reader, \
        a split preview and a wide reader with an outline. "
        .repeat(5);
    let source = (0..12)
        .map(|section| {
            format!(
                "## Section {section}\n\n{paragraph}\n\n```text\nfirst line\nsecond line\n\
                 third line\n```\n\n| View | Purpose |\n| --- | --- |\n\
                 | Preview | Read formatted Markdown |\n| Source | Read the source |\n\n"
            )
        })
        .collect::<String>();
    for mode in [
        MarkdownViewMode::Preview,
        MarkdownViewMode::Source,
        MarkdownViewMode::Split,
    ] {
        for (width, height) in [
            (849.0, 460.0),
            (1_280.0, 800.0),
            (640.0, 320.0),
            (390.0, 568.0),
        ] {
            if mode == MarkdownViewMode::Split && width < 620.0 {
                continue;
            }
            let (mut runtime, instance, view) = markdown_runtime(&source);
            runtime
                .dispatch(
                    instance,
                    view,
                    AppEvent::Action(ActionInvocation {
                        id: ActionId::new(match mode {
                            MarkdownViewMode::Preview => "markdown/mode/preview",
                            MarkdownViewMode::Source => "markdown/mode/source",
                            MarkdownViewMode::Split => "markdown/mode/split",
                        }),
                        value: None,
                    }),
                )
                .unwrap();
            let viewport = LogicalRect::new(0.0, 0.0, width, height);
            for (direction, button) in [
                (1, "markdown/next-page-button"),
                (-1, "markdown/previous-page-button"),
            ] {
                let rendered = runtime
                    .render(instance, view, &cx(viewport, None))
                    .unwrap()
                    .compile(viewport)
                    .unwrap();
                let control = rendered.semantic(&UiKey::new(button)).unwrap();
                assert!(control.state.is_some_and(|state| state.enabled));
                let expected =
                    markdown_page_location(control.action.as_ref().unwrap().as_str()).unwrap();
                runtime
                    .dispatch(
                        instance,
                        view,
                        AppEvent::MarkdownPage {
                            direction,
                            viewport_width: width,
                            viewport_height: height,
                        },
                    )
                    .unwrap();
                let actual = MarkdownApp::location(runtime.view_state(view).unwrap());
                assert_eq!(
                    actual, expected,
                    "{mode:?}, {width}×{height}, direction {direction}"
                );
            }
        }
    }
}

fn editor_fixture(
    text: &str,
    title: &str,
) -> (
    EditorApp,
    EditorViewState,
    crate::document_store::DocumentSnapshot,
) {
    let mut store = crate::test_document_store();
    let document = store.open("file:///visual-editor.md".to_string(), text.to_string());
    let snapshot = store.snapshot(document).unwrap();
    let buffer = crate::native_editor::EditorBufferView::new(
        document,
        crate::document_store::DocumentViewId(1),
        snapshot.seq,
    );
    (
        EditorApp::new(document, title.to_string()),
        EditorViewState {
            buffer: Some(buffer),
            ..EditorViewState::default()
        },
        snapshot,
    )
}

fn editor_input_focus(app: &mut EditorApp, view: &mut EditorViewState, focused: bool) {
    view.common.focus_visible = true;
    let mut next_operation = 1;
    let mut service_generations = BTreeMap::new();
    let mut cx = UpdateCx {
        instance: AppInstanceId::from_stored(1),
        view: ViewId::from_stored(1),
        view_generation: 1,
        next_operation: &mut next_operation,
        service_generations: &mut service_generations,
        effects: Vec::new(),
    };
    assert_eq!(
        app.update(
            view,
            AppEvent::FocusChanged(focused.then(|| UiKey::new("editor/buffer"))),
            &mut cx,
        ),
        EventResult::Handled
    );
}

/// The assist panel `compiled` shows when nothing is listed: none, or the help line
/// on the rows it painted.
fn shown_help_palette(compiled: &crate::native_ui::CompiledUi) -> crate::native_ui::EditorPalette {
    if compiled
        .semantic(&UiKey::new("editor/config-assist"))
        .is_none()
    {
        return crate::native_ui::EditorPalette::None;
    }
    crate::native_ui::EditorPalette::Help {
        help_rows: compiled
            .paint
            .iter()
            .filter(|node| node.key.as_str().starts_with("editor/config-help/visual"))
            .count()
            .max(1),
    }
}

fn editor_spec(compiled: &crate::native_ui::CompiledUi) -> &TextViewportSpec {
    compiled
        .paint
        .iter()
        .find_map(|node| match &node.content {
            UiContent::TextViewport(spec) => Some(spec),
            _ => None,
        })
        .expect("editor viewport paint node")
}

#[test]
fn dirty_editor_uses_current_state_instead_of_stale_clean_feedback() {
    let (mut app, mut view, snapshot) = editor_fixture("draft\n", "draft.txt");
    let render = |app: &EditorApp, view: &EditorViewState| {
        let viewport = LogicalRect::new(0.0, 0.0, 1_200.0, 700.0);
        let compiled = app
            .view(view, &cx(viewport, Some(&snapshot)))
            .compile(viewport)
            .unwrap();
        let spec = editor_spec(&compiled);
        (spec.status.clone(), spec.semantic_status.clone())
    };
    app.dirty = true;
    let current = render(&app, &view);
    assert!(current.0.is_some() && current.1.is_some());
    for status in ["Saved", "No unsaved changes"] {
        view.status = Some(status.to_string());
        assert_eq!(render(&app, &view), current);
        app.dirty = false;
        assert_eq!(render(&app, &view).1.as_deref(), Some(status));
        app.dirty = true;
    }
    for status in [
        "Save failed: test write failure",
        "Independent command feedback",
    ] {
        view.status = Some(status.to_string());
        let shown = render(&app, &view);
        assert_eq!(shown.0.as_deref(), Some(status));
        assert_eq!(shown.1.as_deref(), Some(status));
    }
}

#[test]
fn manual_watcher_warning_does_not_destroy_editor_feedback_and_recovery_clears_it() {
    let source = "font_px = \"huge\"\n";
    let (mut app, mut view, _snapshot) = editor_fixture(source, "aterm.toml");
    app.config_editor = true;
    app.config_analysis = Some(crate::native_config_language::analyze(source));
    view.status = Some("Saved".to_string());
    view.config_watch_status =
        Some("aterm.toml reload rejected: the file is not valid UTF-8.".to_string());

    let warning = editor_status_message(&app, &view, 1, false).unwrap();
    assert!(warning.contains("not valid UTF-8"), "{warning}");
    assert!(
        warning.contains("Config error · Ln 1, Col 11 · aterm schema: "),
        "the diagnostic stays: {warning}"
    );
    assert_eq!(view.status.as_deref(), Some("Saved"));

    view.config_watch_status = None;
    let recovered = editor_status_message(&app, &view, 1, false).unwrap();
    assert!(!recovered.contains("not valid UTF-8"), "{recovered}");
    assert_eq!(view.status.as_deref(), Some("Saved"));
}

#[test]
fn a_recovered_draft_note_never_hides_why_save_was_refused() {
    let source = "font_px = \"huge\"\n";
    let (mut app, mut view, _snapshot) = editor_fixture(source, "aterm.toml");
    app.config_editor = true;
    app.config_analysis = Some(crate::native_config_language::analyze(source));
    app.recovery_status = Some(crate::app_documents::CARRIED_DRAFT_NOTICE.to_string());
    let before = editor_status_message(&app, &view, 1, false).unwrap();
    assert!(before.starts_with("Recovery · "), "{before}");

    view.status = Some("Save blocked · Config error · Ln 1, Col 11 · bad".to_string());
    let refused = editor_status_message(&app, &view, 1, false).unwrap();
    assert!(refused.starts_with("Save blocked"), "{refused}");

    view.status = None;
    app.checkpoint_pending = true;
    assert_eq!(
        editor_status_message(&app, &view, 1, false).as_deref(),
        Some("Saving…")
    );
}

#[test]
fn editor_view_is_responsive_bounded_and_reports_real_buffer_state() {
    let text = (1..=80)
        .map(|line| format!("line {line}\n"))
        .collect::<String>();
    let title = format!("{}-document.md", "very-long-🦀-".repeat(20));
    let (app, view, snapshot) = editor_fixture(&text, &title);
    for (width, height) in [(1_424.0, 658.0), (804.0, 582.0), (474.0, 468.0)] {
        let viewport = LogicalRect::new(0.0, 0.0, width, height);
        let compiled = app
            .view(&view, &cx(viewport, Some(&snapshot)))
            .compile(viewport)
            .unwrap();
        let spec = editor_spec(&compiled);
        let projection = spec.projection.as_ref().unwrap();
        assert!(projection.lines.len() <= 33);
        assert_eq!(projection.total_lines, 81);
        assert!(spec.label.starts_with("very-long-"));
        assert!(spec.label.ends_with("document.md"));
        assert!(spec.label.chars().count() <= 96);
        assert!(compiled.semantics.iter().all(|node| {
            node.rect.x >= 0.0
                && node.rect.y >= 0.0
                && node.rect.right() <= width
                && node.rect.bottom() <= height
        }));
        compiled.validate_parity().unwrap();
    }
}

/// The config editor's problem buttons compile at every width with visible labels matching their semantic names.
#[test]
fn config_problem_buttons_compile_at_every_width() {
    let source = "future_first_setting = 1\nfuture_second_setting = 2\n";
    let (mut app, view, snapshot) = editor_fixture(source, "aterm.toml");
    app.config_editor = true;
    app.config_analysis = Some(crate::native_config_language::analyze(source));
    app.config_analysis_revision = 1;
    for width in [1_424.0, 960.0, 804.0, 474.0, 320.0] {
        let viewport = LogicalRect::new(0.0, 0.0, width, 600.0);
        let compiled = app
            .view(&view, &cx(viewport, Some(&snapshot)))
            .compile(viewport)
            .unwrap_or_else(|error| panic!("{width}pt: {error:?}"));
        for key in [
            "editor/config-problem-previous-button",
            "editor/config-problem-next-button",
        ] {
            assert!(
                compiled.semantic(&UiKey::new(key)).is_some(),
                "{width}pt: {key}"
            );
        }
    }
}

/// Every listed config completion's label fits the room its button paints it in, at
/// every width and text size: shortened in the middle to that room, in the face and
/// size the button paints. Before, the labels were measured against the help line's
/// width with the button's own insets left out, so at 50 columns at 2x the painter cut
/// "gpu — G…nder…" a second time past its end. The negative control: some label is
/// shortened, so the room is what decides.
#[test]
fn listed_config_completions_fit_their_buttons() {
    let source = "";
    let (mut app, mut view, snapshot) = editor_fixture(source, "aterm.toml");
    app.config_editor = true;
    app.config_analysis = Some(crate::native_config_language::analyze(source));
    app.config_analysis_revision = 1;
    view.config_completion_interaction =
        Some(crate::native_config_language::ConfigCompletionContext::new(
            snapshot.id.get(),
            snapshot.seq.0,
            0,
        ));
    let mut shortened = false;
    for scale in [1.0_f32, 1.5, 2.0] {
        let _scale = RestoreAppearance::at(scale);
        for width in [1_074.0_f32, 738.0, 514.0, 374.0, 350.0, 286.5] {
            let context = format!("{width} at {scale}x");
            let viewport = LogicalRect::new(0.0, 0.0, width, 558.0);
            let compiled = app
                .view(&view, &cx(viewport, Some(&snapshot)))
                .compile(viewport)
                .unwrap();
            let labels: Vec<String> = compiled
                .paint
                .iter()
                .filter(|node| node.key.as_str().starts_with("editor/config-completion/"))
                .filter_map(|node| match &node.content {
                    UiContent::Button(control) => control.spec.visual_label.clone(),
                    _ => None,
                })
                .collect();
            assert!(!labels.is_empty(), "{context}: the list shows");
            shortened |= labels.iter().any(|label| label.contains('\u{2026}'));
            assert!(
                compiled.fit_failures().is_empty(),
                "{context}: {:?}",
                compiled.fit_failures()
            );
        }
    }
    assert!(shortened, "the negative control");
}

/// THE FILE KEEPS SEVERAL ROWS UNDER AN OPEN COMPLETION LIST (audit r20 ED1): in an 80x24
/// window at 1.5x, 1.75x and 2x text, Ctrl-Space left the file the one row being completed
/// under the toolbar, the help line, a candidate, the header and the footer, and a help whose
/// first fact wraps took the rows the file had left. The list now takes only the room the
/// file can spare: its rows close up, the toolbar's last row gives way to it while it is open
/// and the help's rows past its first give way over it. So the file shows three rows or more
/// under a list at every standard size and text scale (three is what a header, a footer and
/// a one-candidate list leave at 80x24 and 2x), the toolbar stands where the file keeps its
/// floor beside it, and the view paints the rows the metrics count.
#[test]
fn the_file_keeps_several_rows_under_an_open_completion_list() {
    use crate::native_ui::{EditorPalette, EditorToolbar};
    let helps = [
        "Prime coding agents automatically · agents_auto_prime · true / false · default true",
        "Draw the cursor as a block, a beam or an underline, and blink it while the shell \
         waits for input · cursor_shape · block / beam / underline · default block",
    ];
    let windows = [
        (584.0, 348.0, "80x24"),
        (738.0, 418.0, "102x29"),
        (900.0, 600.0, "120x36"),
        (1_200.0, 800.0, "160x48"),
    ];
    let toolbars = [
        EditorToolbar::default(),
        EditorToolbar {
            commands_hidden: false,
            problem_navigation: true,
            reload: true,
            edit_header: false,
            grid: None,
        },
        // An `edit` ticket's header row (Done and Abort) left one row under M-x at 2x.
        EditorToolbar {
            commands_hidden: false,
            problem_navigation: false,
            reload: false,
            edit_header: true,
            grid: None,
        },
    ];
    for scale in [0.85_f32, 1.0, 1.25, 1.5, 1.75, 2.0] {
        let _scale = RestoreAppearance::at(scale);
        for (width, height, name) in windows {
            let viewport = LogicalRect::new(0.0, 0.0, width, height);
            for toolbar in toolbars {
                for candidates in [1, 8] {
                    for help in helps {
                        let (palette, lines) = crate::native_ui::editor_help_palette(
                            viewport,
                            toolbar,
                            Some(candidates),
                            help,
                        );
                        let rows = crate::native_ui::editor_visible_line_capacity_with_palette(
                            viewport, palette, toolbar, 1,
                        );
                        assert!(
                            rows >= 3,
                            "{name} at {scale}x, {candidates} listed, {toolbar:?}, help \
                             {lines:?}: {rows} rows of the file"
                        );
                    }
                }
            }
        }
    }
    let tight = |viewport: LogicalRect| {
        crate::native_ui::editor_shell_metrics(
            viewport,
            EditorPalette::Rows {
                candidates: 8,
                help_rows: 1,
            },
            EditorToolbar::default(),
        )
    };
    {
        let _scale = RestoreAppearance::at(2.0);
        let small = tight(LogicalRect::new(0.0, 0.0, 584.0, 348.0));
        assert_eq!(
            small.command_rows, 0,
            "the list stands for the toolbar at 80x24 and 2x"
        );
        let roomy = tight(LogicalRect::new(0.0, 0.0, 1_200.0, 800.0));
        assert!(
            roomy.command_rows > 0,
            "and the toolbar stands where both fit"
        );
    }

    // The view: the config editor at 80x24 with keys listed under the caret, its status on
    // one row (a status that wraps takes the footer's reserved rows from the body).
    let source = std::iter::once("agents_auto_prime = false\n".to_string())
        .chain((1..18).map(|line| format!("# note {line}\n")))
        .collect::<String>();
    let (mut app, mut view, snapshot) = editor_fixture(&source, "aterm.toml");
    app.config_editor = true;
    app.config_analysis = Some(crate::native_config_language::analyze(&source));
    app.config_analysis_revision = 1;
    view.config_completion_interaction =
        Some(crate::native_config_language::ConfigCompletionContext::new(
            snapshot.id.get(),
            snapshot.seq.0,
            0,
        ));
    let viewport = LogicalRect::new(0.0, 0.0, 584.0, 348.0);
    for scale in [1.0_f32, 1.5, 2.0] {
        let _scale = RestoreAppearance::at(scale);
        let compiled = app
            .view(&view, &cx(viewport, Some(&snapshot)))
            .compile(viewport)
            .unwrap();
        assert!(
            compiled
                .semantic(&UiKey::new("editor/config-completion/0"))
                .is_some(),
            "{scale}x: the list shows"
        );
        let buffer = compiled
            .semantic(&UiKey::new("editor/buffer"))
            .expect("the file");
        let spec = editor_spec(&compiled);
        assert_eq!(
            spec.footer_rows(),
            1,
            "{scale}x: premise: {:?}",
            spec.status
        );
        let rows = crate::native_ui::whole_editor_rows(&spec.geometry(buffer.rect));
        assert!(
            rows >= 3,
            "{scale}x: {rows} rows of the file under the list"
        );
        assert_eq!(
            spec.projection
                .as_ref()
                .map(|projection| projection.lines.len()),
            Some(rows),
            "{scale}x: the rows shown are the rows projected"
        );
        assert_eq!(
            compiled
                .semantic(&UiKey::new("editor/command-bar"))
                .is_some(),
            scale < 1.5,
            "{scale}x: the toolbar gives way to the list only where the file needs it"
        );
        assert!(
            compiled.fit_failures().is_empty(),
            "{scale}x: {:?}",
            compiled.fit_failures()
        );
    }

    // An `edit` ticket's header row stands at rest and gives way last, under the list alone.
    view.edit_header = Some(EditHeader {
        for_word: Some("git".to_string()),
        waiter: EditWaiter::Waiting,
        read_only: false,
    });
    for (scale, listed, header) in [
        (1.0_f32, true, true),
        (2.0, false, true),
        (2.0, true, false),
    ] {
        let _scale = RestoreAppearance::at(scale);
        view.config_completion_interaction = listed.then(|| {
            crate::native_config_language::ConfigCompletionContext::new(
                snapshot.id.get(),
                snapshot.seq.0,
                0,
            )
        });
        let compiled = app
            .view(&view, &cx(viewport, Some(&snapshot)))
            .compile(viewport)
            .unwrap();
        let context = format!("{scale}x, listed {listed}");
        assert_eq!(
            compiled
                .semantic(&UiKey::new("editor/edit-header"))
                .is_some(),
            header,
            "{context}"
        );
        let buffer = compiled
            .semantic(&UiKey::new("editor/buffer"))
            .expect("the file");
        let rows =
            crate::native_ui::whole_editor_rows(&editor_spec(&compiled).geometry(buffer.rect));
        assert!(rows >= 3, "{context}: {rows} rows of the file");
        assert!(
            compiled.fit_failures().is_empty(),
            "{context}: {:?}",
            compiled.fit_failures()
        );
    }
}

/// A CONFIG ERROR'S STATUS NEVER TAKES THE FILE'S FLOOR: typing a new key makes aterm.toml
/// unparseable until its `=`, and the status said so in the parser's whole text ("Config
/// error · Ln 2, Col 5 · TOML syntax: TOML parse error at line 2, column 5 | 2 | curs | ^
/// expected `=` after a key · F8 jumps there"), which wrapped to the footer's three reserved
/// rows. At 80x24 and 2x that left the file two rows, and one under the completion list the
/// person opened to finish the key. The status now says what the parser found without the
/// place it repeats, and its rows past the first give way where the file would show fewer
/// than its floor, its shorter forms first. The negative control: a 3-row footer over the
/// same editor leaves the file under its floor.
#[test]
fn a_config_errors_status_never_takes_the_files_floor() {
    let source = std::iter::once("curs\n".to_string())
        .chain((1..18).map(|line| format!("# note {line}\n")))
        .collect::<String>();
    let (mut app, mut view, snapshot) = editor_fixture(&source, "aterm.toml");
    app.config_editor = true;
    let analysis = crate::native_config_language::analyze(&source);
    let message = &analysis.diagnostic_at(0).expect("the parse error").message;
    assert_eq!(message, "TOML syntax: expected `=` after a key");
    app.config_analysis = Some(analysis);
    app.config_analysis_revision = 1;
    let viewport = LogicalRect::new(0.0, 0.0, 584.0, 348.0);
    for listed in [false, true] {
        view.config_completion_interaction = listed.then(|| {
            crate::native_config_language::ConfigCompletionContext::new(
                snapshot.id.get(),
                snapshot.seq.0,
                0,
            )
        });
        for scale in [1.0_f32, 1.5, 2.0] {
            let _scale = RestoreAppearance::at(scale);
            let context = format!("{scale}x, listed {listed}");
            let compiled = app
                .view(&view, &cx(viewport, Some(&snapshot)))
                .compile(viewport)
                .unwrap();
            assert_eq!(
                compiled
                    .semantic(&UiKey::new("editor/config-completion/0"))
                    .is_some(),
                listed,
                "{context}"
            );
            let rect = compiled
                .semantic(&UiKey::new("editor/buffer"))
                .expect("the file")
                .rect;
            let spec = editor_spec(&compiled);
            let painted = spec.status.as_deref().expect("the footer");
            assert!(
                painted
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .contains("expected `=` after a key"),
                "{context}: the parser's finding shows whole: {painted:?}"
            );
            let rows = crate::native_ui::whole_editor_rows(&spec.geometry(rect));
            let floor = if listed { 3 } else { 4 };
            assert!(
                rows >= floor,
                "{context}: {rows} rows of the file under {painted:?}"
            );
            assert_eq!(
                spec.projection
                    .as_ref()
                    .map(|projection| projection.lines.len()),
                Some(rows),
                "{context}: the rows shown are the rows projected"
            );
            if scale == 2.0 {
                let under_three = crate::native_ui::whole_editor_rows(
                    &crate::native_ui::text_viewport_geometry(rect, 3),
                );
                assert!(
                    under_three < floor,
                    "{context}: negative control: three footer rows leave {under_three}"
                );
            }
            assert!(
                compiled.fit_failures().is_empty(),
                "{context}: {:?}",
                compiled.fit_failures()
            );
        }
    }
}

#[test]
fn manual_config_completions_page_every_candidate_and_keep_full_live_help() {
    let source = "";
    let (mut app, mut view, snapshot) = editor_fixture(source, "aterm.toml");
    app.config_editor = true;
    app.config_analysis = Some(crate::native_config_language::analyze(source));
    app.config_analysis_revision = 1;
    let assist = crate::native_config_language::assist(source, 0);
    view.config_completion_interaction =
        Some(crate::native_config_language::ConfigCompletionContext::new(
            snapshot.id.get(),
            snapshot.seq.0,
            0,
        ));
    let viewport = LogicalRect::new(0.0, 0.0, 320.0, 300.0);
    let capacity =
        crate::native_ui::editor_shell_metrics(viewport, assist.completions.len(), app.toolbar())
            .palette_visible_rows;
    assert!(capacity > 0 && capacity < assist.completions.len());
    let mut reachable = std::collections::BTreeSet::new();

    for selected in 0..assist.completions.len() {
        view.config_completion_selected = selected;
        let compiled = app
            .view(&view, &cx(viewport, Some(&snapshot)))
            .compile(viewport)
            .unwrap();
        let visible = compiled
            .semantics
            .iter()
            .filter_map(|node| {
                node.key
                    .as_str()
                    .strip_prefix("editor/config-completion/")?
                    .parse::<usize>()
                    .ok()
            })
            .collect::<Vec<_>>();
        assert!(visible.contains(&selected));
        assert!(visible.len() <= capacity);
        reachable.extend(visible);

        let help = compiled
            .semantic(&UiKey::new("editor/config-help"))
            .unwrap();
        assert_eq!(help.role, crate::native_ui::SemanticRole::Status);
        assert!(help.label.contains(assist.help.as_deref().unwrap()));

        for key in ["editor/config-page-previous", "editor/config-page-next"] {
            let action = compiled
                .semantic(&UiKey::new(key))
                .and_then(|node| node.action.as_ref());
            assert!(action.is_some());
            assert!(
                crate::command_registry::native_document_action(action.unwrap().as_str()).is_some()
            );
        }
        compiled.validate_parity().unwrap();
    }
    assert_eq!(
        reachable,
        (0..assist.completions.len()).collect::<std::collections::BTreeSet<_>>()
    );
}

/// The caret line is painted whole above the footer: caret reveal counts
/// only the rows shown whole, never the part row the footer cuts (or the
/// reveal-ahead row under it).
#[test]
fn the_caret_line_is_painted_whole_above_the_footer() {
    let mut source = String::new();
    for index in 1..=31 {
        source.push_str(&format!("# note {index}\n"));
    }
    for (width, height) in [(584.0, 348.0), (738.0, 418.0)] {
        for config_editor in [true, false] {
            let (mut app, mut view, snapshot) = editor_fixture(&source, "aterm.toml");
            app.config_editor = config_editor;
            app.config_analysis = Some(crate::native_config_language::analyze(&source));
            app.config_analysis_revision = snapshot.seq.0;
            let caret = source.trim_end().len();
            let buffer = view.buffer.as_mut().expect("editor buffer");
            buffer.selections = vec![crate::native_editor::Selection::caret(caret)];
            buffer.primary = 0;
            let viewport = LogicalRect::new(0.0, 0.0, width, height);
            let compiled = app
                .view(&view, &cx(viewport, Some(&snapshot)))
                .compile(viewport)
                .unwrap();
            let rect = compiled
                .semantic(&UiKey::new("editor/buffer"))
                .expect("the buffer")
                .rect;
            let geometry = editor_spec(&compiled).geometry(rect);
            let projection = editor_spec(&compiled)
                .projection
                .as_ref()
                .expect("document projection");
            let row = projection
                .lines
                .iter()
                .position(|line| !line.carets.is_empty())
                .unwrap_or_else(|| panic!("{width}x{height}: the caret line is projected"));
            let bottom = geometry.body_y + (row + 1) as f32 * geometry.line_h;
            assert!(
                bottom <= geometry.body_y + geometry.body_h + 0.01,
                "{width}x{height} config={config_editor}: the caret line (row {row}) ends at \
                 {bottom}, under the footer at {}",
                geometry.body_y + geometry.body_h
            );
        }
    }
}

/// The text scale for one test, put back when the test ends.
struct RestoreAppearance(crate::native_appearance::AppearancePreferences);

impl RestoreAppearance {
    fn at(scale: f32) -> Self {
        let held = Self(crate::native_appearance::current_preferences());
        crate::native_appearance::install_preferences(
            crate::native_appearance::AppearancePreferences {
                text_scale: scale,
                ..crate::native_appearance::AppearancePreferences::default()
            },
        );
        held
    }
}

impl Drop for RestoreAppearance {
    fn drop(&mut self) {
        crate::native_appearance::install_preferences(self.0);
    }
}

/// The config editor's help line takes the rows its text needs, as the footer
/// does: at 50 columns (350 points) the help for `agents_auto_prime` is painted
/// whole on the rows it wraps to, never "Prime coding agents automatically ·
/// agents_a…" on one; where the rows the document can spare cannot hold it whole,
/// the longest form that fits them whole (its last facts left out), and what the
/// setting does (its first fact) always whole, at 2x too, never "Prime coding
/// agents…"; each row inside the panel, the panel exactly as tall as its rows, and
/// the input side's palette (the rows it counts for the document) the view's. The
/// negative control: at 50 columns the help is wider than one row, so a one-row help
/// line (the cut this replaces) could not have painted it.
#[test]
fn the_config_help_line_wraps_whole_onto_its_own_rows() {
    let source = "agents_auto_prime = false\n";
    let (mut app, view, snapshot) = editor_fixture(source, "aterm.toml");
    app.config_editor = true;
    app.config_analysis = Some(crate::native_config_language::analyze(source));
    app.config_analysis_revision = snapshot.seq.0;
    let help = app
        .config_assist(&snapshot, 0)
        .and_then(|(_, assist)| assist.resting_help)
        .expect("the key under the caret has help");
    let mut wrapped = false;
    for scale in [1.0_f32, 1.5, 2.0] {
        let _scale = RestoreAppearance::at(scale);
        for width in [1_074.0_f32, 738.0, 514.0, 374.0, 350.0, 286.5] {
            let context = format!("{width} at {scale}x");
            let viewport = LogicalRect::new(0.0, 0.0, width, 558.0);
            let compiled = app
                .view(&view, &cx(viewport, Some(&snapshot)))
                .compile(viewport)
                .unwrap();
            let painted: Vec<String> = compiled
                .paint
                .iter()
                .filter(|node| node.key.as_str().starts_with("editor/config-help/visual"))
                .map(|node| match &node.content {
                    UiContent::Text(spec) => spec.text.clone(),
                    other => panic!("{context}: the help is text: {other:?}"),
                })
                .collect();
            let room = crate::native_ui::editor_help_width(viewport);
            let (px, face) = crate::native_ui::editor_help_type();
            let measure = |text: &str| crate::tray_raster::ui_text_width_for(face, text, px);
            let one_row = measure(&help) <= room;
            wrapped |= !one_row;
            let palette = shown_help_palette(&compiled);
            let shell = crate::native_ui::editor_shell_metrics(viewport, palette, app.toolbar());
            let needs = |form: &str| {
                if measure(form) <= room {
                    1
                } else {
                    crate::native_settings::wrap_to_box(form, room, &measure).len()
                }
            };
            if one_row {
                assert_eq!(painted.len(), 1, "{context}: {painted:?}");
                assert!(painted[0].starts_with(&help), "{context}: {painted:?}");
            } else {
                let forms = crate::native_ui::editor_help_forms(&help);
                let shown = forms.iter().position(|form| {
                    crate::native_ui::footer_shows_whole(form, &painted.join("\n"))
                });
                // Every form holds the first fact whole, the shortest it alone.
                if needs(&forms[forms.len() - 1]) <= crate::native_ui::EDITOR_HELP_MAX_ROWS {
                    assert!(
                        shown.is_some(),
                        "{context}: what the setting does is whole: {painted:?}"
                    );
                }
                match shown {
                    Some(shown) => assert!(
                        forms[..shown]
                            .iter()
                            .all(|longer| needs(longer) > shell.help_rows),
                        "{context}: a longer form fits the {} rows: {painted:?}",
                        shell.help_rows
                    ),
                    // Cut only where not even the first fact fits every row the
                    // document can spare the panel.
                    None => assert!(
                        needs(&forms[forms.len() - 1]) > shell.help_rows
                            && painted.last().is_some_and(|line| line.ends_with('…')),
                        "{context}: cut only past the panel's rows: {painted:?}"
                    ),
                }
                if needs(&help) <= crate::native_ui::EDITOR_HELP_MAX_ROWS && width <= 374.0 {
                    assert!(shell.help_rows > 1, "{context}: the help takes rows");
                }
                for line in &painted {
                    assert!(
                        measure(line) <= room + crate::native_ui::TEXT_FIT_SLACK,
                        "{context}: {line:?} fits the panel's {room}"
                    );
                }
            }
            assert_eq!(
                palette,
                crate::native_ui::editor_help_palette(viewport, app.toolbar(), None, &help).0,
                "{context}: the rows the input side counts are the rows painted"
            );
            assert_eq!(painted.len(), shell.help_rows, "{context}");
            let assist = compiled
                .semantic(&UiKey::new("editor/config-assist"))
                .expect("the help panel");
            assert!(
                (assist.rect.height - (shell.palette_padding * 2.0 + shell.palette_header_height))
                    .abs()
                    < 0.01,
                "{context}: the panel is its rows' height"
            );
            assert!(
                compiled.fit_failures().is_empty(),
                "{context}: {:?}",
                compiled.fit_failures()
            );
            if width == 350.0 {
                assert!(!one_row, "{context}: the negative control");
            }
        }
    }
    assert!(wrapped);
}

/// The editor's header keeps the file's name whole: where the name and the
/// caret's place do not both fit, the place gives way (`aterm.toml` alone, never
/// `ate…oml`); a wide editor shows both. The negative control: at 50 columns
/// (350 points) at 2x the whole name does not fit beside the place, so keeping
/// the place (the rule this replaces) cut the name.
#[test]
fn the_editor_header_keeps_the_file_name_whole_before_the_caret_place() {
    let (app, view, snapshot) = editor_fixture("agents_auto_prime = false\n", "aterm.toml");
    for scale in [1.0_f32, 1.5, 2.0] {
        let _scale = RestoreAppearance::at(scale);
        for width in [1_074.0_f32, 738.0, 514.0, 374.0, 350.0, 286.5, 248.0] {
            let context = format!("{width} at {scale}x");
            let viewport = LogicalRect::new(0.0, 0.0, width, 558.0);
            let compiled = app
                .view(&view, &cx(viewport, Some(&snapshot)))
                .compile(viewport)
                .unwrap();
            let spec = editor_spec(&compiled);
            let rect = compiled
                .semantic(&UiKey::new("editor/buffer"))
                .expect("the buffer")
                .rect;
            let cursor = spec.cursor_label.as_deref().expect("the caret's place");
            let alone = crate::native_ui::editor_header_layout(rect, "", None, false);
            let header =
                crate::native_ui::editor_header_layout(rect, &spec.label, Some(cursor), false);
            let room_beside = crate::native_ui::editor_header_layout(rect, "", Some(cursor), false);
            let name_w = crate::tray_raster::ui_text_width_for(
                crate::widget::TextFace::UiBold,
                "aterm.toml",
                crate::native_ui::native_type_px(crate::type_scale::TypeStep::Body).get(),
            );
            let fits_beside = name_w
                <= room_beside.title_right - room_beside.title_x + crate::native_ui::TEXT_FIT_SLACK;
            let fits_alone =
                name_w <= alone.title_right - alone.title_x + crate::native_ui::TEXT_FIT_SLACK;
            if fits_alone {
                assert_eq!(spec.label, "aterm.toml", "{context}: the name whole");
            } else {
                assert!(spec.label.contains('…'), "{context}: {:?}", spec.label);
            }
            assert_eq!(header.cursor_x.is_some(), fits_beside, "{context}");
            if width >= 738.0 {
                assert!(header.cursor_x.is_some(), "{context}: room for both");
            }
            if width == 350.0 && scale == 2.0 {
                assert!(
                    fits_alone && !fits_beside,
                    "{context}: the negative control"
                );
            }
        }
    }
}

/// A resting caret's help line is a panel of its own height with no row
/// under it (no "Nothing to suggest here"), so the file keeps that room;
/// how to open the list is said on the help line itself.
#[test]
fn resting_help_is_one_line_with_no_row_under_it() {
    let source = "gpu = true\n";
    let (mut app, view, snapshot) = editor_fixture(source, "aterm.toml");
    app.config_editor = true;
    app.config_analysis = Some(crate::native_config_language::analyze(source));
    app.config_analysis_revision = snapshot.seq.0;
    let viewport = LogicalRect::new(0.0, 0.0, 584.0, 348.0);
    let compiled = app
        .view(&view, &cx(viewport, Some(&snapshot)))
        .compile(viewport)
        .unwrap();
    let assist = compiled
        .semantic(&UiKey::new("editor/config-assist"))
        .expect("the help line for the key under the caret");
    let shell = crate::native_ui::editor_shell_metrics(
        viewport,
        shown_help_palette(&compiled),
        app.toolbar(),
    );
    assert_eq!(
        assist.rect.height,
        shell.palette_padding * 2.0 + shell.palette_header_height,
        "the help line and its padding, nothing more"
    );
    assert!(
        compiled
            .semantic(&UiKey::new("editor/config-completion-empty"))
            .is_none(),
        "no row under the help line"
    );
    assert_eq!(
        crate::native_ui::editor_text_viewport_rect_with_palette(
            viewport,
            shown_help_palette(&compiled),
            app.toolbar(),
        )
        .height,
        compiled
            .semantic(&UiKey::new("editor/buffer"))
            .expect("the buffer")
            .rect
            .height
    );
}

/// The Manual's landing frame at a default 80x24 window (viewport 744x420), caret at
/// Ln 1 Col 1 with nothing typed or asked for: the help line shows but no suggestion
/// list, the edited file gets most of the height, and the config's error sits on a
/// painted row.
#[test]
fn manual_landing_paints_the_config_file_not_only_its_completion_list() {
    let mut source = String::from("theme = \"Draculla\"\nbidi = \"sidewayz\"\n");
    for index in 1..=16 {
        source.push_str(&format!("# note {index}\n"));
    }
    let (mut app, view, snapshot) = editor_fixture(&source, "aterm.toml");
    app.config_editor = true;
    app.config_analysis = Some(crate::native_config_language::analyze(&source));
    app.config_analysis_revision = snapshot.seq.0;

    let viewport = LogicalRect::new(0.0, 0.0, 744.0, 420.0);
    let compiled = app
        .view(&view, &cx(viewport, Some(&snapshot)))
        .compile(viewport)
        .unwrap();

    let assist = compiled
        .semantic(&UiKey::new("editor/config-assist"))
        .expect("the help line for the key under the caret");
    assert!(
        compiled
            .semantics
            .iter()
            .all(|node| !node.key.as_str().starts_with("editor/config-completion/")),
        "an unrequested completion list on the landing frame"
    );
    let buffer = compiled
        .semantic(&UiKey::new("editor/buffer"))
        .expect("config editor buffer");
    assert!(
        buffer.rect.height > assist.rect.height,
        "the edited file gets {} px and its unrequested help {} px",
        buffer.rect.height,
        assist.rect.height
    );

    let projection = editor_spec(&compiled)
        .projection
        .as_ref()
        .expect("document projection");
    assert!(projection.total_lines >= 18, "18-line fixture config");
    assert!(
        projection.lines.len() >= 6,
        "landing frame projects {} of {} document lines",
        projection.lines.len(),
        projection.total_lines
    );

    let geometry = editor_spec(&compiled).geometry(buffer.rect);
    let painted_rows = (geometry.body_h / geometry.line_h).floor() as usize;
    assert!(
        painted_rows >= 6,
        "{painted_rows} painted document rows in a {} px buffer",
        buffer.rect.height
    );
    let erroring = projection
        .lines
        .iter()
        .position(|line| !line.diagnostics.is_empty())
        .expect("the fixture's invalid `bidi` value is diagnosed");
    assert!(
        erroring < painted_rows,
        "the config's only error (line 2) sits at painted row {erroring} of {painted_rows}"
    );
    compiled.validate_parity().unwrap();
}

#[test]
fn manual_editor_chrome_elision_preserves_complete_grapheme_clusters() {
    for (source, expected) in [
        ("A👩‍💻WWWW", "A👩‍💻…"),
        ("Ae\u{301}WWWW", "Ae\u{301}…"),
        ("A🇺🇳WWWW", "A🇺🇳…"),
    ] {
        assert_eq!(bounded_markdown_label(source, 2), expected);
    }
}

#[test]
fn editor_ime_minibuffer_projection_maps_sanitized_and_elided_carets() {
    let (_, mut view, _) = editor_fixture("", "ime.md");
    let buffer = view.buffer.as_mut().unwrap();
    buffer.minibuffer = crate::native_editor::Minibuffer::Command {
        query: "λ\n".to_string(),
        selected: 0,
    };
    let (label, caret) =
        editor_minibuffer_projection(buffer, "e\u{301}日本", Some(3), 40, f32::INFINITY);
    assert_eq!(label.as_deref(), Some("M-x λ e\u{301}日本"));
    assert_eq!(caret, Some("M-x λ e\u{301}".len()));
    let (label, caret) =
        editor_minibuffer_projection(buffer, "e\u{301}日本", None, 40, f32::INFINITY);
    assert_eq!(caret, label.as_ref().map(String::len));
    buffer.prefix_hud = Some("C-x …".to_string());
    assert_eq!(
        editor_minibuffer_projection(buffer, "候補", Some(0), 40, f32::INFINITY),
        (Some("C-x …".to_string()), None)
    );
    buffer.prefix_hud = None;
    buffer.minibuffer = crate::native_editor::Minibuffer::Inactive;
    assert_eq!(
        editor_minibuffer_projection(buffer, "候補", Some(0), 40, f32::INFINITY),
        (None, None)
    );
    let (label, caret) = editor_prompt_projection(
        "M-x ",
        "discarded👩‍💻",
        "e\u{301}日本",
        Some(3),
        9,
        f32::INFINITY,
    );
    assert_eq!(label, "M-x …👩‍💻e\u{301}日本");
    assert_eq!(caret, "M-x …👩‍💻e\u{301}".len());
    let (label, caret) =
        editor_prompt_projection("M-x ", "", "abcdef日本", Some(0), 8, f32::INFINITY);
    assert_eq!(label, "M-x …f日本");
    assert_eq!(
        caret,
        "M-x …".len(),
        "a hidden prefix caret clamps to the visible tail"
    );
}

#[test]
fn manual_editor_cursor_label_uses_shared_grapheme_cell_geometry() {
    let text = "first\n\te\u{301}中👩‍💻🇺🇳Z\n";
    let (_app, mut view, _snapshot) = editor_fixture(text, "aterm.toml");
    let buffer = view.buffer.as_mut().expect("editor buffer");
    let line_start = text.find('\n').unwrap() + 1;
    let after_tab = line_start + '\t'.len_utf8();
    let after_combining = after_tab + "e\u{301}".len();
    let after_cjk = after_combining + "中".len();
    let after_emoji = after_cjk + "👩‍💻".len();
    let after_flag = after_emoji + "🇺🇳".len();

    for (caret, expected) in [
        (after_tab, "Ln 2, Col 5"),
        (after_combining, "Ln 2, Col 6"),
        (after_cjk, "Ln 2, Col 8"),
        (after_emoji, "Ln 2, Col 10"),
        (after_flag, "Ln 2, Col 12"),
        (after_flag + 1, "Ln 2, Col 13"),
        (after_tab + 'e'.len_utf8(), "Ln 2, Col 5"),
        (after_cjk + '👩'.len_utf8(), "Ln 2, Col 8"),
    ] {
        buffer.selections = vec![crate::native_editor::Selection::caret(caret)];
        assert_eq!(editor_cursor_label(text, buffer), expected, "caret={caret}");
    }
}

/// THE CONFIG EDITOR'S FOOTER PAINTS ITS WHOLE DIAGNOSTIC wherever its
/// rows can hold it: the status wraps at whole words in the footer's real
/// slot (the editor's width inside the footer inset, the one width the
/// view fits to, the painter draws in and the audit measures), and the
/// footer takes the rows it needs (the rows the editor always leaves it,
/// and past them every row the document can spare). Where even those
/// rows cannot hold the sentence it paints the longest shorter form that
/// fits whole (the key hint goes first, then the place, then the label; the
/// message stays whole), and it cuts only when not even the message fits.
/// The body's
/// rows are counted under the footer the frame paints. The verifier's
/// cases (50 and 70 columns) paint the whole sentence. The negative
/// control: at the narrow widths the sentence is wider than one row, so a
/// one-row footer (the cut this replaces) could not have painted it.
#[test]
fn the_config_footer_paints_its_whole_diagnostic_wherever_it_fits() {
    use crate::native_appearance::{
        AppearancePreferences, current_preferences, install_preferences,
    };
    struct RestoreAppearance(AppearancePreferences);
    impl Drop for RestoreAppearance {
        fn drop(&mut self) {
            install_preferences(self.0);
        }
    }
    let _restore = RestoreAppearance(current_preferences());
    let source = "future = true\nzzz = 1\n";
    let fixture = |message: &str| {
        let (mut app, view, snapshot) = editor_fixture(source, "aterm.toml");
        app.config_editor = true;
        let mut analysis = crate::native_config_language::analyze(source);
        analysis.diagnostics = vec![crate::native_config_language::ConfigDiagnostic {
            bytes: 14..17,
            line: 2,
            column: 1,
            severity: crate::native_config_language::ConfigDiagnosticSeverity::Warning,
            message: message.to_string(),
        }];
        app.config_analysis = Some(analysis);
        app.config_analysis_revision = snapshot.seq.0;
        (app, view, snapshot)
    };
    let realistic = "zzz is unknown to this build and has no effect";
    let unknown_key = "future.👩‍💻.e\u{301}.🇺🇳.a-deliberately-long-forward-compatible-key";
    let long = format!("unknown configuration key {unknown_key}");
    let mut wrapped = false;
    for scale in [1.0_f32, 2.0] {
        install_preferences(AppearancePreferences {
            text_scale: scale,
            ..AppearancePreferences::default()
        });
        for message in [realistic, long.as_str()] {
            let (app, view, snapshot) = fixture(message);
            let forms = app
                .config_analysis
                .as_ref()
                .unwrap()
                .summary_forms_at(0)
                .unwrap();
            for width in [1_074.0_f32, 738.0, 514.0, 486.0, 374.0, 286.5, 248.0, 234.0] {
                let context = format!("{width} at {scale}x: {message}");
                let viewport = LogicalRect::new(0.0, 0.0, width, 558.0);
                let compiled = app
                    .view(&view, &cx(viewport, Some(&snapshot)))
                    .compile(viewport)
                    .unwrap();
                let spec = editor_spec(&compiled);
                let semantic = spec.semantic_status.as_deref().expect("the diagnostic");
                let painted = spec.status.as_deref().expect("the footer");
                assert_eq!(semantic, forms[0], "{context}");
                let rect = compiled
                    .semantic(&UiKey::new("editor/buffer"))
                    .expect("the buffer")
                    .rect;
                let slot = crate::native_ui::editor_footer_text_width(rect.width);
                let cap = crate::native_ui::editor_footer_row_cap(rect);
                let needs = |form: &str| {
                    crate::native_settings::wrap_to_box(
                        form,
                        slot,
                        &crate::native_ui::native_status_label_width,
                    )
                    .len()
                };
                for line in painted.lines() {
                    assert!(
                        crate::native_ui::native_status_label_width(line)
                            <= slot + crate::native_ui::TEXT_FIT_SLACK,
                        "{context}: {line:?} fits the slot {slot}"
                    );
                }
                wrapped |= needs(semantic) > 1;
                match forms
                    .iter()
                    .position(|form| crate::native_ui::footer_shows_whole(form, painted))
                {
                    Some(shown) => {
                        assert!(
                            forms[..shown].iter().all(|longer| needs(longer) > cap),
                            "{context}: a longer form fits the {cap} rows: {painted:?}"
                        );
                        assert!(painted.contains(message.split(' ').next().unwrap()));
                    }
                    None => {
                        assert!(
                            needs(&forms[forms.len() - 1]) > cap
                                && spec.footer_rows() == cap
                                && painted.ends_with('…'),
                            "{context}: cut only past every row the footer may take: {painted:?}"
                        );
                    }
                }
                if message == realistic && scale == 1.0 && width >= 374.0 {
                    assert!(
                        crate::native_ui::footer_shows_whole(semantic, painted),
                        "{context}: the whole sentence: {painted:?}"
                    );
                }
                if message == realistic && needs(message) <= cap {
                    // The message is never cut where the footer's rows hold it alone (at
                    // 32 columns at 2x, three rows hold it, not with its label).
                    assert!(
                        forms
                            .iter()
                            .any(|form| crate::native_ui::footer_shows_whole(form, painted)),
                        "{context}: the message whole: {painted:?}"
                    );
                }
                assert!(
                    compiled.fit_failures().is_empty(),
                    "{context}: {:?}",
                    compiled.fit_failures()
                );
                // The body's rows are the rows under this footer.
                let geometry = spec.geometry(rect);
                assert_eq!(spec.footer_rows(), painted.lines().count(), "{context}");
                assert!(
                    (geometry.body_h + geometry.footer_h + geometry.header_h - rect.height).abs()
                        < 0.01,
                    "{context}"
                );
                let palette = shown_help_palette(&compiled);
                let painted_rows = spec.projection.as_ref().expect("projection").lines.len();
                assert!(
                    painted_rows
                        <= crate::native_ui::editor_visible_line_capacity_with_palette(
                            viewport,
                            palette,
                            app.toolbar_in(&view),
                            spec.footer_rows(),
                        ),
                    "{context}"
                );
                compiled.validate_parity().unwrap();
            }
        }
    }
    assert!(
        wrapped,
        "the sentence is wider than one row at the narrow widths"
    );
}

/// The editor toolbar compiles as a measured whole-word plan across widths and toolbar variants.
#[test]
fn the_editor_toolbar_is_one_measured_plan_in_whole_words() {
    const WORDS: [&str; 11] = [
        "Save",
        "Undo",
        "Redo",
        "Find",
        "Go to Line",
        "Commands",
        "Prev Issue",
        "Next Issue",
        "Prev",
        "Next",
        "Reload",
    ];
    let whole_word = |label: &str| WORDS.contains(&label);
    for bad in ["‹!", "!›", "Go to…", "Run…", "F…", "…"] {
        assert!(!whole_word(bad), "{bad}");
    }
    let source = "gpu = true\nunknown_key = 1\n";
    let (mut app, view, snapshot) = editor_fixture(source, "aterm.toml");
    app.config_editor = true;
    app.config_analysis_revision = snapshot.seq.0;
    app.dirty = true;
    app.can_undo = true;
    app.can_redo = true;
    for (problems, conflict) in [(false, false), (true, false), (true, true)] {
        let analysis = crate::native_config_language::analyze(source);
        assert!(analysis.diagnostic_count() > 0, "the fixture has a problem");
        app.config_analysis = problems.then_some(analysis);
        app.disk_conflict = conflict;
        for width in [1_074.0, 738.0, 486.0, 374.0, 286.5, 234.0] {
            let viewport = LogicalRect::new(0.0, 0.0, width, 558.0);
            let context = format!("{width} problems={problems} conflict={conflict}");
            let compiled = app
                .view(&view, &cx(viewport, Some(&snapshot)))
                .compile(viewport)
                .unwrap();
            assert!(
                compiled.fit_failures().is_empty(),
                "{context}: {:?}",
                compiled.fit_failures()
            );
            let buttons: Vec<(String, String, LogicalRect)> = compiled
                .paint
                .iter()
                .filter_map(|node| match &node.content {
                    UiContent::Button(control)
                        if node.key.as_str().starts_with("editor/")
                            && node.key.as_str().ends_with("-button") =>
                    {
                        Some((
                            node.key.as_str().to_string(),
                            control.spec.visual_label.clone().unwrap_or_default(),
                            node.rect,
                        ))
                    }
                    _ => None,
                })
                .collect();
            assert!(buttons.len() >= 4, "{context}: {buttons:?}");
            for (key, label, rect) in &buttons {
                assert!(whole_word(label), "{context}: {key} paints {label:?}");
                assert!(
                    rect.width + 0.5 >= crate::native_ui::editor_toolbar_button_width(label),
                    "{context}: {key} squeezed to {}",
                    rect.width
                );
            }
            let has = |key: &str| buttons.iter().any(|(k, _, _)| k == key);
            assert_eq!(
                has("editor/config-problem-next-button"),
                problems,
                "{context}"
            );
            assert_eq!(has("editor/revert-button"), conflict, "{context}");
            assert!(
                has("editor/save-button") && has("editor/undo-button"),
                "{context}"
            );
            let rows = compiled
                .semantics
                .iter()
                .filter(|node| node.key.as_str().starts_with("editor/command-row-"))
                .count();
            let shell = crate::native_ui::editor_shell_metrics(
                viewport,
                shown_help_palette(&compiled),
                app.toolbar(),
            );
            assert_eq!(rows, shell.command_rows, "{context}");
            let label = |key: &str| {
                buttons
                    .iter()
                    .find(|(k, _, _)| k == key)
                    .map(|(_, label, _)| label.as_str())
            };
            if problems && !conflict && width == 738.0 {
                assert_eq!(
                    [
                        label("editor/goto-line-button"),
                        label("editor/commands-button"),
                        label("editor/config-problem-previous-button"),
                        label("editor/config-problem-next-button"),
                    ],
                    [
                        Some("Go to Line"),
                        Some("Commands"),
                        Some("Prev Issue"),
                        Some("Next Issue")
                    ],
                    "{context}"
                );
                assert_eq!(rows, 1, "{context}");
            }
            if problems && !conflict && width == 374.0 {
                assert_eq!(rows, 2, "{context}");
                assert_eq!(
                    [
                        label("editor/goto-line-button"),
                        label("editor/commands-button"),
                    ],
                    [Some("Go to Line"), Some("Commands")],
                    "{context}"
                );
            }
            let palette = shown_help_palette(&compiled);
            assert_eq!(
                crate::native_ui::editor_text_viewport_rect_with_palette(
                    viewport,
                    palette,
                    app.toolbar(),
                )
                .height,
                compiled
                    .semantic(&UiKey::new("editor/buffer"))
                    .expect("the buffer")
                    .rect
                    .height,
                "{context}"
            );
        }
    }
}

#[test]
fn editor_command_bar_is_actionable_and_save_shares_the_conflict_gate() {
    let (mut app, view, snapshot) = editor_fixture("first\nsecond\n", "notes.md");
    app.dirty = true;
    app.can_undo = true;
    app.can_redo = true;
    for (width, height, compact) in [(804.0, 582.0, false), (286.5, 558.0, true)] {
        let viewport = LogicalRect::new(0.0, 0.0, width, height);
        let compiled = app
            .view(&view, &cx(viewport, Some(&snapshot)))
            .compile(viewport)
            .unwrap();
        for (key, action) in [
            ("editor/save-button", "editor/save"),
            ("editor/undo-button", "editor/undo"),
            ("editor/redo-button", "editor/redo"),
            ("editor/find-button", "editor/find"),
            ("editor/goto-line-button", "editor/goto-line"),
            ("editor/commands-button", "editor/commands"),
        ] {
            let semantic = compiled.semantic(&UiKey::new(key)).unwrap();
            assert_eq!(semantic.action.as_ref().map(ActionId::as_str), Some(action));
            assert!(semantic.state.is_some_and(|state| state.enabled));
            assert!(
                compiled
                    .hits
                    .iter()
                    .any(|hit| { hit.key.as_str() == key && hit.action.as_str() == action })
            );
        }
        let painted = |key: &str| {
            compiled
                .paint
                .iter()
                .find(|node| node.key.as_str() == key)
                .and_then(|node| match &node.content {
                    UiContent::Button(control) => control.spec.visual_label.clone(),
                    _ => None,
                })
                .unwrap_or_default()
        };
        let goto = painted("editor/goto-line-button");
        let commands = painted("editor/commands-button");
        assert_eq!(
            (goto.as_str(), commands.as_str()),
            ("Go to Line", "Commands"),
            "compact={compact}"
        );
        compiled.validate_parity().unwrap();
    }

    app.disk_conflict = true;
    let mut commands = Vec::new();
    app.commands(&view, &mut commands);
    assert!(
        !command(&commands, "editor/save").enabled,
        "shortcut/controller Save must share the conflict gate"
    );
    let viewport = LogicalRect::new(0.0, 0.0, 474.0, 468.0);
    let compiled = app
        .view(&view, &cx(viewport, Some(&snapshot)))
        .compile(viewport)
        .unwrap();
    assert!(
        compiled
            .semantic(&UiKey::new("editor/save-button"))
            .and_then(|node| node.state)
            .is_some_and(|state| !state.enabled),
        "the painted/semantic Save button must be disabled during conflict"
    );
    let reload = compiled
        .semantic(&UiKey::new("editor/revert-button"))
        .expect("conflict recovery is visible without opening the command palette");
    assert_eq!(
        reload.action.as_ref().map(ActionId::as_str),
        Some("editor/revert")
    );
    assert!(reload.state.is_some_and(|state| state.enabled));
}

/// The disk notice takes the header row: a changed file offers Reload
/// (Discard's action) and Keep Mine, a deleted one Keep Mine alone, and
/// the toolbar's own Reload steps aside for it. No notice, no row (the
/// negative control).
#[test]
fn the_disk_notice_takes_the_header_row_with_its_choice() {
    let (mut app, view, snapshot) = editor_fixture("first\nsecond\n", "notes.md");
    app.dirty = true;
    app.disk_conflict = true;
    let viewport = LogicalRect::new(0.0, 0.0, 804.0, 582.0);
    let compile = |app: &EditorApp| {
        app.view(&view, &cx(viewport, Some(&snapshot)))
            .compile(viewport)
            .unwrap()
    };
    let action = |compiled: &crate::native_ui::CompiledUi, key: &str| {
        compiled
            .semantic(&UiKey::new(key))
            .and_then(|node| node.action.as_ref().map(|id| id.as_str().to_string()))
    };

    let compiled = compile(&app);
    assert!(
        compiled
            .semantic(&UiKey::new("editor/disk-notice"))
            .is_none()
    );
    assert_eq!(
        action(&compiled, "editor/revert-button").as_deref(),
        Some("editor/revert")
    );

    app.disk_notice = Some(DiskNotice::Changed);
    let compiled = compile(&app);
    assert!(
        compiled
            .semantic(&UiKey::new("editor/disk-notice"))
            .is_some()
    );
    assert_eq!(
        action(&compiled, "editor/disk-reload").as_deref(),
        Some("editor/revert")
    );
    assert_eq!(
        action(&compiled, "editor/disk-keep").as_deref(),
        Some("editor/keep-local")
    );
    assert!(
        compiled
            .semantic(&UiKey::new("editor/revert-button"))
            .is_none(),
        "the notice's Reload stands for the toolbar's"
    );
    compiled.validate_parity().unwrap();
    let mut commands = Vec::new();
    app.commands(&view, &mut commands);
    assert!(command(&commands, "editor/keep-local").enabled);

    app.disk_notice = Some(DiskNotice::Gone);
    let compiled = compile(&app);
    assert!(
        compiled
            .semantic(&UiKey::new("editor/disk-reload"))
            .is_none(),
        "a deleted file has nothing to reload"
    );
    assert_eq!(
        action(&compiled, "editor/disk-keep").as_deref(),
        Some("editor/keep-local")
    );
    assert_eq!(
        crate::native_ui::editor_text_viewport_rect_with_palette(
            viewport,
            crate::native_ui::EditorPalette::None,
            app.toolbar(),
        )
        .height,
        compiled
            .semantic(&UiKey::new("editor/buffer"))
            .expect("the buffer")
            .rect
            .height,
        "the layout gives the notice its row"
    );
}

#[test]
fn editor_history_actions_wait_for_prompts_and_resume_the_same_history() {
    use crate::native_editor::{EditorCommand, EditorEffect, EditorWorkspace};

    fn assert_history_actions(
        app: &EditorApp,
        view: &EditorViewState,
        snapshot: &crate::document_store::DocumentSnapshot,
        expected: (bool, bool),
    ) {
        let mut commands = Vec::new();
        app.commands(view, &mut commands);
        let viewport = LogicalRect::new(0.0, 0.0, 804.0, 582.0);
        let compiled = app
            .view(view, &cx(viewport, Some(snapshot)))
            .compile(viewport)
            .unwrap();
        for (id, key, enabled) in [
            ("editor/undo", "editor/undo-button", expected.0),
            ("editor/redo", "editor/redo-button", expected.1),
        ] {
            assert_eq!(command(&commands, id).enabled, enabled, "{id}");
            assert_eq!(
                compiled
                    .semantic(&UiKey::new(key))
                    .unwrap()
                    .state
                    .unwrap()
                    .enabled,
                enabled,
                "{key}"
            );
        }
        compiled.validate_parity().unwrap();
    }

    let source = "first needle\nsecond needle\n";
    let mut store = crate::test_document_store();
    let document = store.open("file:///history.txt".to_string(), source.to_string());
    let mut workspace = EditorWorkspace::new();
    let mut view = EditorViewState {
        buffer: Some(
            workspace
                .attach(
                    &mut store,
                    document,
                    crate::document_store::DocumentViewId(1),
                )
                .unwrap(),
        ),
        ..EditorViewState::default()
    };
    let mut app = EditorApp::new(document, "history.txt".to_string());
    for text in ["one ", "two "] {
        workspace
            .insert_text(&mut store, view.buffer.as_mut().unwrap(), text)
            .unwrap();
    }
    let full_edit = store.snapshot(document).unwrap();
    workspace
        .execute(
            &mut store,
            view.buffer.as_mut().unwrap(),
            EditorCommand::Undo,
        )
        .unwrap();
    let draft = store.snapshot(document).unwrap();
    app.can_undo = workspace.can_undo(document);
    app.can_redo = workspace.can_redo(document);
    assert!(app.can_undo && app.can_redo);
    assert_history_actions(&app, &view, &draft, (true, true));

    for prompt in [
        EditorCommand::GotoLine,
        EditorCommand::ExecuteCommand,
        EditorCommand::SwitchBuffer,
        EditorCommand::KillBuffer,
        EditorCommand::IncrementalSearch,
    ] {
        workspace
            .execute(&mut store, view.buffer.as_mut().unwrap(), prompt)
            .unwrap();
        workspace
            .insert_text(&mut store, view.buffer.as_mut().unwrap(), "2")
            .unwrap();
        let prompt_state = view.buffer.clone();
        assert_history_actions(&app, &view, &draft, (false, false));
        for command in [EditorCommand::Undo, EditorCommand::Redo] {
            let effects = workspace
                .execute(&mut store, view.buffer.as_mut().unwrap(), command)
                .unwrap();
            assert!(matches!(effects.as_slice(), [EditorEffect::Status(_)]));
            assert_eq!(view.buffer, prompt_state);
            let after = store.snapshot(document).unwrap();
            assert_eq!(after.seq, draft.seq);
            assert_eq!(after.text, draft.text);
            assert!(workspace.can_undo(document) && workspace.can_redo(document));
        }
        if prompt == EditorCommand::GotoLine {
            workspace
                .submit_minibuffer(&mut store, view.buffer.as_mut().unwrap())
                .unwrap();
        } else {
            workspace
                .execute(
                    &mut store,
                    view.buffer.as_mut().unwrap(),
                    EditorCommand::Abort,
                )
                .unwrap();
        }
        assert!(!view.buffer.as_ref().unwrap().minibuffer_active());
        assert_history_actions(&app, &view, &draft, (true, true));
    }

    workspace
        .execute(
            &mut store,
            view.buffer.as_mut().unwrap(),
            EditorCommand::Redo,
        )
        .unwrap();
    let redone = store.snapshot(document).unwrap();
    assert_eq!(redone.text, full_edit.text);
    assert!(redone.seq > draft.seq);
    app.can_undo = workspace.can_undo(document);
    app.can_redo = workspace.can_redo(document);
    assert_history_actions(&app, &view, &redone, (true, false));
}

#[test]
fn compact_editor_projection_and_pointer_follow_an_offscreen_caret() {
    let text = "0123456789".repeat(24);
    let (app, view, snapshot) = editor_fixture(&text, "wide-line.md");
    let viewport = LogicalRect::new(0.0, 0.0, 474.0, 468.0);
    let render = |view: &EditorViewState| {
        app.view(view, &cx(viewport, Some(&snapshot)))
            .compile(viewport)
            .unwrap()
    };
    let mut moved = view.clone();
    moved.buffer.as_mut().unwrap().selections = vec![crate::native_editor::Selection::caret(173)];
    let after = render(&moved);
    let spec = editor_spec(&after);
    let line = &spec.projection.as_ref().unwrap().lines[0];
    assert!(line.source.start > 0);
    let [(local_caret, true)] = line.carets.as_slice() else {
        panic!("the compact projection must retain exactly one primary caret");
    };
    assert!(*local_caret <= line.text.len());
    assert_eq!(
        line.source.start + *local_caret,
        173,
        "horizontal projection preserves the canonical document byte across fonts"
    );
    let rect = after.semantic(&UiKey::new("editor/buffer")).unwrap().rect;
    let geometry = spec.geometry(rect);
    assert_eq!(
        crate::native_ui::text_viewport_byte_at(
            spec,
            rect,
            geometry.text_x,
            geometry.body_y + geometry.line_h / 2.0,
        ),
        Some(line.source.start),
        "the left painted cell maps to the horizontally shifted canonical source"
    );
    after.validate_parity().unwrap();
}

#[test]
fn editor_recovery_status_wins_and_preedit_follows_input_focus() {
    let (mut app, mut view, snapshot) = editor_fixture("", "empty.md");
    let viewport = LogicalRect::new(0.0, 0.0, 474.0, 468.0);
    let render = |app: &EditorApp, view: &EditorViewState| {
        app.view(view, &cx(viewport, Some(&snapshot)))
            .compile(viewport)
            .unwrap()
    };

    app.recovery_status = Some("journal restored; review before saving".to_string());
    view.status = Some("Mark set".to_string());
    view.buffer.as_mut().unwrap().minibuffer = crate::native_editor::Minibuffer::Command {
        query: "goto".to_string(),
        selected: 0,
    };
    view.preedit = "終".to_string();
    editor_input_focus(&mut app, &mut view, true);
    let recovered = render(&app, &view);
    let spec = editor_spec(&recovered);
    assert!(
        spec.status
            .as_deref()
            .is_some_and(|status| status.starts_with("Recovery · ")),
        "the recovery note outranks the view's own status"
    );
    assert!(
        spec.minibuffer
            .as_deref()
            .is_some_and(|m| m.ends_with('終'))
    );
    let caret = recovered
        .ime_caret_rect()
        .expect("typed minibuffer has a visible caret");
    let viewport_node = recovered
        .paint
        .iter()
        .find(|node| matches!(node.content, UiContent::TextViewport(_)))
        .unwrap();
    let geometry = spec.geometry(viewport_node.rect);
    assert!(caret.y >= geometry.body_y + geometry.body_h);
    assert!(
        spec.preedit.is_empty(),
        "IME preedit belongs only to minibuffer"
    );
    recovered.validate_parity().unwrap();

    editor_input_focus(&mut app, &mut view, false);
    let without_control_focus = render(&app, &view);
    assert!(!editor_spec(&without_control_focus).focused);
    assert_eq!(
        without_control_focus.ime_caret_rect(),
        Some(caret),
        "clearing control focus does not change the editor's typed prompt target"
    );
}

#[test]
fn editor_ime_minibuffer_tail_and_marked_caret_fit_real_mono_metrics() {
    use crate::native_appearance::{
        AppearancePreferences, current_preferences, install_preferences,
    };
    struct RestoreAppearance(AppearancePreferences);
    impl Drop for RestoreAppearance {
        fn drop(&mut self) {
            install_preferences(self.0);
        }
    }
    let _restore = RestoreAppearance(current_preferences());
    let mut historical_overflow = false;
    for scale in [0.85, 1.0, 1.35, 2.0] {
        install_preferences(AppearancePreferences {
            text_scale: scale,
            ..AppearancePreferences::default()
        });
        for width in [280.0, 360.0, 474.0, 760.0] {
            let (mut app, mut view, snapshot) = editor_fixture("", "ime-fit.md");
            editor_input_focus(&mut app, &mut view, true);
            let query = format!("{}TAIL", "λ".repeat(200));
            view.buffer.as_mut().unwrap().minibuffer = crate::native_editor::Minibuffer::Command {
                query: query.clone(),
                selected: 0,
            };
            view.preedit = "e\u{301}Z".to_string();
            view.preedit_caret = Some("e\u{301}".len());
            let viewport = LogicalRect::new(0.0, 0.0, width, 640.0);
            let compiled = app
                .view(&view, &cx(viewport, Some(&snapshot)))
                .compile(viewport)
                .unwrap();
            let spec = editor_spec(&compiled);
            let node = compiled
                .paint
                .iter()
                .find(|node| matches!(node.content, UiContent::TextViewport(_)))
                .unwrap();
            let label = spec.minibuffer.as_deref().unwrap();
            assert!(
                label.starts_with("M-x …") && label.ends_with("TAILe\u{301}Z"),
                "scale={scale}, width={width}, label={label:?}"
            );
            assert_eq!(
                spec.minibuffer_caret,
                Some(label.len() - 1),
                "the platform middle caret stays before Z"
            );
            let caret = compiled
                .ime_caret_rect()
                .expect("focused marked caret fits");
            // The footer's one text slot ([`crate::native_ui::editor_footer_text_width`]).
            let slot = crate::native_ui::editor_footer_text_width(node.rect.width);
            let inset = (node.rect.width - slot) / 2.0;
            assert!(
                caret.x >= node.rect.x + inset && caret.right() <= node.rect.x + inset + slot,
                "scale={scale}, width={width}, caret={caret:?}"
            );
            let px = crate::native_ui::native_type_px(crate::type_scale::TypeStep::Caption).get();
            let measured =
                crate::tray_raster::measure_text(label, px, crate::widget::TextWeight::Regular);
            assert!(
                measured + 1.0 <= slot,
                "text and end caret retain the right gutter: scale={scale}, width={width}"
            );
            let historical_chars = ((slot / 7.0).floor() as usize).clamp(16, 256);
            let (historical, _) = editor_prompt_projection(
                "M-x ",
                &query,
                &view.preedit,
                view.preedit_caret,
                historical_chars,
                f32::INFINITY,
            );
            historical_overflow |= crate::tray_raster::measure_text(
                &historical,
                px,
                crate::widget::TextWeight::Regular,
            ) + 1.0
                > slot;
            view.preedit_caret = None;
            let at_end = app
                .view(&view, &cx(viewport, Some(&snapshot)))
                .compile(viewport)
                .unwrap();
            assert!(at_end.ime_caret_rect().expect("end caret fits").x > caret.x);
        }
    }
    assert!(
        historical_overflow,
        "the previous fixed 7px budget must actually clip a focused caret in this sweep"
    );
}

#[test]
fn middle_label_is_utf8_safe_and_retains_filename_identity() {
    let label = bounded_middle_label("prefix-🦀-a-very-long-document.md", 18);
    assert!(label.starts_with("prefix-🦀"));
    assert!(label.ends_with("ument.md"));
    assert_eq!(label.graphemes().count(), 18);
    assert_eq!(bounded_middle_label("abc", 1), "…");
    assert_eq!(bounded_middle_label("abc", 0), "");
    assert_eq!(bounded_middle_label("A👩‍💻BCDE", 4), "A👩‍💻…E");
    assert_eq!(bounded_middle_label("ABCDEé", 3), "A…é");
    assert_eq!(bounded_middle_label("ABCDE🇺🇳", 3), "A…🇺🇳");
}

#[test]
fn middle_label_fits_proportional_pixel_budget() {
    use crate::widget::TextFace;

    let _ = crate::native_appearance::install_preferences(
        crate::native_appearance::AppearancePreferences::default(),
    );
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let px = 20.0 * crate::native_appearance::text_scale();
    let max_width = 160.0;
    let title = "aterm-native-final-sample-with-a-distinguishing-suffix.md";
    let label = bounded_middle_label_to_width(title, max_width, px);
    assert!(crate::tray_raster::ui_text_width_for(TextFace::UiBold, &label, px) <= max_width);
    assert!(label.starts_with("aterm"));
    assert!(label.ends_with(".md"));
    assert_eq!(
        bounded_middle_label_to_width("short.md", 500.0, px),
        "short.md"
    );
    assert_eq!(bounded_middle_label_to_width(title, 0.0, px), "");
}

#[test]
fn markdown_header_keeps_identity_without_redundant_compact_fragment() {
    use crate::widget::TextFace;

    let _ = crate::native_appearance::install_preferences(
        crate::native_appearance::AppearancePreferences::default(),
    );
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let title = "aterm-native-final-sample-with-a-distinguishing-suffix.md";
    let mut documents = crate::test_document_store();
    let document = documents.open(
        "file:///long-title.md".to_string(),
        "# Reader\n".to_string(),
    );
    let app = MarkdownApp::new(document, title.to_string(), "# Reader\n");
    let view = MarkdownViewState::default();
    let px = 20.0 * crate::native_appearance::text_scale();

    {
        let viewport = LogicalRect::new(0.0, 0.0, 928.0, 620.0);
        let compiled = app
            .view(&view, &cx(viewport, None))
            .compile(viewport)
            .unwrap();
        let visual = compiled.semantic(&UiKey::new("markdown/title")).unwrap();
        assert!(
            crate::tray_raster::ui_text_width_for(TextFace::UiBold, &visual.label, px)
                <= visual.rect.width - 4.0 + 0.01,
            "{}px title paints inside its exact flexible rect",
            viewport.width,
        );
        assert!(
            visual.label.starts_with("at"),
            "{}px title lost its identity-bearing prefix: {}",
            viewport.width,
            visual.label,
        );
        assert!(visual.label.ends_with(".md"));
        assert!(
            compiled
                .semantic(&UiKey::new("markdown/header"))
                .unwrap()
                .label
                .contains(title),
            "visual elision never truncates the semantic document identity",
        );
        compiled.validate_parity().unwrap();
    }

    for viewport in [
        LogicalRect::new(0.0, 0.0, 474.0, 468.0),
        LogicalRect::new(0.0, 0.0, 286.5, 558.0),
    ] {
        let compiled = app
            .view(&view, &cx(viewport, None))
            .compile(viewport)
            .unwrap();
        assert!(
            compiled.semantic(&UiKey::new("markdown/title")).is_none(),
            "compact chrome must not paint an opaque filename fragment"
        );
        assert!(
            compiled
                .semantic(&UiKey::new("markdown/header"))
                .unwrap()
                .label
                .contains(title),
            "the tab/header semantics retain complete document identity"
        );
        assert!(compiled.paint_audit_lines()[0].contains("overflow=0"));
        compiled.validate_parity().unwrap();
    }
}
