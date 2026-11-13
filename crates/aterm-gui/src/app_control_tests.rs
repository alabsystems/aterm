// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::native_app::AppEvent;

/// `act` reaches a control a Settings list holds off screen: it scrolls
/// the row into view and presses it. A key no list holds stays refused.
#[test]
fn act_scrolls_a_listed_row_into_view_before_pressing_it() {
    let mut app = App::headless_for_test();
    let wid = *app.windows.keys().next().expect("a window");
    let (cell_width, cell_height) = app.win_cell_size(wid);
    let pad = app.win_pad(wid);
    {
        let window = app.windows.get_mut(&wid).unwrap();
        let scale = window.scale;
        let px = |points: f64| (points * scale) as usize;
        window.cols =
            u16::try_from(px(1_120.0).saturating_sub(pad * 2) / cell_width.max(1)).unwrap();
        window.rows = u16::try_from(px(700.0).saturating_sub(pad) / cell_height.max(1)).unwrap();
    }
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Themes));
    let (_, view) = app.active_native_view(wid).expect("a native view");
    let key = "settings/showcase/pack";
    let drawn = |app: &App| {
        app.compile_view(view)
            .unwrap()
            .compiled
            .semantic(&UiKey::new(key))
            .is_some_and(|node| !node.offscreen)
    };
    assert!(
        !drawn(&app),
        "the disclosure starts below the list's screen"
    );
    let refused = app.act_app(ActRequest {
        view,
        ui_key: "settings/showcase/card/no-such-theme".to_string(),
        action: "settings/showcase/card/no-such-theme".to_string(),
        value: None,
    });
    assert_eq!(refused, Err("unknown or stale UI key".to_string()));
    app.act_app(ActRequest {
        view,
        ui_key: key.to_string(),
        action: key.to_string(),
        value: None,
    })
    .expect("the off-screen disclosure is pressed");
    assert!(drawn(&app), "the pressed row is on screen");
    let Some(crate::native_app::AppViewState::Settings(state)) =
        app.native_runtime.view_state(view)
    else {
        panic!("the Settings view stays live");
    };
    assert!(state.showcase.pack_open);
}

/// A native view's inspected viewport is where the view is drawn: in a split, below its
/// pane's subtab title row.
#[test]
fn a_split_native_views_viewport_is_below_its_subtab_title_row() {
    use crate::native_app::AppViewState;
    use crate::native_settings::SettingsRoute;
    let mut app = App::headless_for_test();
    let wid = *app.windows.keys().next().expect("a window");
    assert!(app.open_settings_tab(SettingsRoute::Home));
    let (instance, _) = app.active_native_view(wid).expect("a native view");
    app.split_active_with_native(
        wid,
        crate::tab_model::SplitAxis::Horizontal,
        instance,
        AppViewState::Settings(Box::new(crate::native_settings::SettingsViewState::new(
            &app.config,
        ))),
    )
    .expect("split");
    let plan = app.active_visible_leaf_plan(wid).expect("a plan");
    assert_eq!(plan.leaves.len(), 2, "two views side by side");
    let (cw, ch) = app.win_cell_size(wid);
    let (cw, ch) = (cw as f32, ch as f32);
    for leaf in &plan.leaves {
        let header = leaf.header.expect("a split pane reserves its title row");
        let viewport = app.view_rect(wid, leaf.view).expect("a viewport");
        assert_eq!(
            viewport,
            LogicalRect::new(
                leaf.rect.origin.x * cw,
                leaf.rect.origin.y * ch,
                leaf.rect.size.width * cw,
                leaf.rect.size.height * ch,
            ),
            "the viewport is the plan's content rect"
        );
        assert!(
            viewport.origin.y >= (header.origin.y + header.size.height) * ch,
            "the viewport starts below the title row: {viewport:?} / {header:?}"
        );
    }
}

#[test]
fn wire_requests_become_stable_owned_targets() {
    assert_eq!(parse_open("app about").unwrap(), OpenRequest::About);
    assert_eq!(
        parse_open("app system-state").unwrap(),
        OpenRequest::SystemState
    );
    assert_eq!(
        parse_inspect("app/v1 view 42 controls").unwrap(),
        InspectRequest::View {
            view: ViewId::from_stored(42),
            projection: InspectionProjection::Controls,
        }
    );
    let action = parse_act("app/v1 view 42 settings/font_px settings/set/font_px 16").unwrap();
    assert_eq!(action.view, ViewId::from_stored(42));
    assert_eq!(action.value.as_deref(), Some("16"));
    assert!(matches!(
        parse_open("app settings /about").unwrap(),
        OpenRequest::Settings(crate::native_settings::SettingsRoute::About)
    ));
}

#[test]
fn editor_inspection_exposes_semantic_and_painted_statuses_and_diagnostics() {
    use crate::native_editor::{
        EditorDiagnosticSpan, EditorViewportLine, EditorViewportProjection,
    };
    use crate::native_ui::{
        Layout, Length, LogicalRect as UiLogicalRect, TextViewportSpec, UiContent, UiNode, UiTree,
    };

    let semantic_status =
        "Unknown config key `mystery`; open completion help for registered settings";
    let compiled = UiTree::new(
        UiNode::new(
            "editor/buffer",
            UiContent::TextViewport(TextViewportSpec {
                label: "aterm.toml".to_string(),
                document_key: "document:config@4".to_string(),
                selectable: true,
                projection: Some(EditorViewportProjection {
                    first_line: 8,
                    total_lines: 12,
                    lines: vec![EditorViewportLine {
                        number: 8,
                        source: 100..114,
                        column_start: 0,
                        text: "mystery = true".to_string(),
                        selections: Vec::new(),
                        carets: vec![(14, true)],
                        syntax: Vec::new(),
                        diagnostics: vec![
                            EditorDiagnosticSpan {
                                bytes: 0..7,
                                error: false,
                            },
                            EditorDiagnosticSpan {
                                bytes: 10..14,
                                error: true,
                            },
                        ],
                    }],
                }),
                grid: None,
                grid_reading: None,
                concealed: false,
                preedit: String::new(),
                preedit_caret: None,
                status: Some("Unknown config key…".to_string()),
                semantic_status: Some(semantic_status.to_string()),
                minibuffer: None,
                minibuffer_caret: None,
                cursor_label: Some("Ln 9, Col 15".to_string()),
                dirty: true,
                saving: false,
                focused: true,
                focus_visible: true,
                action: Some(ActionId::new("editor/focus-buffer")),
            }),
        )
        .layout(Layout::default().width(Length::Fill).height(Length::Fill)),
    )
    .compile(UiLogicalRect::new(0.0, 0.0, 260.0, 180.0))
    .unwrap();

    let lines = editor_viewport_inspection_lines(&compiled);
    assert!(lines.iter().any(|line| {
        line.starts_with("editor-status ") && line.contains(&format!("value={semantic_status:?}"))
    }));
    assert!(lines.iter().any(|line| {
        line.starts_with("editor-painted-status ")
            && line.contains("value=\"Unknown config key…\"")
            && line.contains("truncated=true")
    }));
    assert!(lines.iter().any(|line| {
        line.starts_with("editor-row ")
            && line.contains("diagnostics=[0..7@100..107:warning,10..14@110..114:error]")
    }));
}

/// The status paints in the proportional Ui face, so a character budget calibrated for
/// the monospace minibuffer under-counts the strip and a tail elide drops the remedy.
#[test]
fn manual_diagnostic_footer_keeps_a_remedy_that_fits_the_default_window() {
    let dir = std::env::temp_dir().join(format!(
        "aterm-control-manual-footer-fit-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("aterm.toml");
    crate::test_scratch::write_config(&path, "bidi = \"sideways\"\n");

    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();

    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    if let Some(window) = app.windows.get_mut(&wid) {
        window.cols = 80;
        window.rows = 24;
    }
    app.ensure_and_open_config_editor_path_in_window(wid, &path)
        .unwrap();
    let (_, view) = app.active_native_view(wid).unwrap();
    let lines = app
        .inspect_app(InspectRequest::View {
            view,
            projection: InspectionProjection::Audit,
        })
        .unwrap();

    let status = lines
        .iter()
        .find(|line| line.starts_with("editor-status "))
        .expect("editor-status");
    assert!(status.contains("bidi"), "{status}");

    let painted = lines
        .iter()
        .find(|line| line.starts_with("editor-painted-status "))
        .expect("editor-painted-status");
    assert!(
        painted.contains("truncated=false"),
        "the remedy fits the default footer and must be painted whole: {painted}"
    );

    let audit = lines
        .iter()
        .find(|line| line.starts_with("paint-editor "))
        .expect("paint-editor audit");
    let measured = |key: &str| -> f32 {
        audit
            .split_whitespace()
            .find_map(|field| field.strip_prefix(key))
            .unwrap_or_else(|| panic!("{key} in {audit}"))
            .parse()
            .unwrap()
    };
    assert!(
        measured("footer-required=") <= measured("footer-available="),
        "the whole diagnostic fits the slot it is painted into: {audit}"
    );

    let _ = std::fs::remove_dir_all(dir);
}

/// `inspect app/v1 tabs` prints `index=` in the one-based numbering `tab <N>` and
/// `tab close <N>` take, and a number past the end is refused.
#[test]
fn tabs_inspection_prints_the_index_the_tab_verb_takes() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.push_stub_tab(wid, crate::stub_session(app.next_session_id));
    app.push_stub_tab(wid, crate::stub_session(app.next_session_id));
    let count = app.windows[&wid].tab_set.len();
    assert_eq!(count, 3, "fixture: three tabs");
    let lines = app.inspect_app(InspectRequest::Tabs).unwrap();
    let numbers: Vec<usize> = lines
        .iter()
        .filter(|line| line.starts_with("  tab id="))
        .map(|line| {
            line.split_whitespace()
                .find_map(|field| field.strip_prefix("index="))
                .unwrap_or_else(|| panic!("index= in {line:?}"))
                .parse()
                .unwrap()
        })
        .collect();
    assert_eq!(numbers, [1, 2, 3], "one-based, in tab order: {lines:?}");
    for number in &numbers {
        let action = crate::control::control_input::parse_tab(&number.to_string()).unwrap();
        assert_eq!(
            app.apply_tab_cmd_in(wid, action),
            Ok((number - 1, count)),
            "`tab {number}` selects the tab inspect printed as index={number}"
        );
    }
    let action = crate::control::control_input::parse_tab(&(count + 1).to_string()).unwrap();
    assert!(app.apply_tab_cmd_in(wid, action).is_err());
}

#[test]
fn tabs_inspection_uses_the_effective_label_with_stable_fallback() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);

    let tab_id = {
        let window = app.windows.get_mut(&wid).expect("headless window");
        window.tab_chrome_titles_by_tab.clear();
        let tab = window.tab_set.tab_at_mut(0).expect("terminal tab");
        tab.presentation.title.clear();
        tab.id
    };
    let empty_fallback = app.inspect_app(InspectRequest::Tabs).unwrap();
    let empty_line = empty_fallback
        .iter()
        .find(|line| line.starts_with(&format!("  tab id={} ", tab_id.get())))
        .expect("terminal tab inspection");
    assert!(
        empty_line.contains("kind=terminal") && empty_line.contains("title=\"aterm\""),
        "empty title did not mirror chrome fallback: {empty_line:?}"
    );
    app.windows
        .get_mut(&wid)
        .unwrap()
        .tab_set
        .tab_at_mut(0)
        .unwrap()
        .presentation
        .title = "stable-terminal".to_string();
    let fallback = app.inspect_app(InspectRequest::Tabs).unwrap();
    let fallback_line = fallback
        .iter()
        .find(|line| line.starts_with(&format!("  tab id={} ", tab_id.get())))
        .expect("terminal tab inspection");
    assert!(fallback_line.contains("title=\"stable-terminal\""));

    {
        let session = app.pool.get(0).expect("session 0");
        let mut meta = session
            .ctx
            .meta
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(
            meta.set("title", Some("title-smoke".to_string())),
            Some(true)
        );
        assert_eq!(
            meta.set("description", Some("Reviewing release checks".to_string())),
            Some(true)
        );
    }
    app.refresh_window_tabs(wid);

    let inspected = app.inspect_app(InspectRequest::Tabs).unwrap();
    let inspected_line = inspected
        .iter()
        .find(|line| line.starts_with(&format!("  tab id={} ", tab_id.get())))
        .expect("terminal tab inspection");
    assert!(
        inspected_line.contains("title=\"title-smoke\""),
        "inspection diverged from refreshed title chrome: {inspected_line:?}"
    );
}

#[test]
fn tabs_inspection_keeps_distinct_labels_for_tabs_sharing_one_session() {
    let mut app = App::headless_for_test();
    let source = WindowId(0);
    let original_id = app.windows[&source].tab_set.active_id().unwrap();

    let shared_window = app
        .open_active_session_in_new_window_logical()
        .expect("share the active session into another window");
    let shared_id = app.windows[&shared_window].tab_set.active_id().unwrap();
    let _ = app.migrate_active_tab_to_next_window();
    assert_eq!(app.windows.len(), 1);
    assert_eq!(app.pool.views(0), Some(2));

    {
        let window = app.windows.get_mut(&source).unwrap();
        window.tab_title_cache.clear();
        window.tab_chrome_titles_by_tab.clear();
        for index in 0..window.tab_set.len() {
            let tab = window.tab_set.tab_at_mut(index).unwrap();
            tab.presentation.title = if tab.id == original_id {
                "source-fallback".to_string()
            } else if tab.id == shared_id {
                "shared-fallback".to_string()
            } else {
                panic!("unexpected tab {}", tab.id.get());
            };
        }
    }

    let term = app.pool.get(0).unwrap().term.clone();
    let parser_guard = term.lock().unwrap();
    app.refresh_window_tabs(source);
    let inspected = app.inspect_app(InspectRequest::Tabs).unwrap();
    let original_line = inspected
        .iter()
        .find(|line| line.starts_with(&format!("  tab id={} ", original_id.get())))
        .expect("original tab inspection");
    let shared_line = inspected
        .iter()
        .find(|line| line.starts_with(&format!("  tab id={} ", shared_id.get())))
        .expect("shared tab inspection");
    assert!(original_line.contains("title=\"source-fallback\""));
    assert!(shared_line.contains("title=\"shared-fallback\""));
    drop(parser_guard);

    let shared_index = app.windows[&source]
        .tab_set
        .tabs()
        .iter()
        .position(|tab| tab.id == shared_id)
        .unwrap();
    assert!(!app.close_tab_at(source, shared_index));
    let window = &app.windows[&source];
    assert!(window.tab_chrome_titles_by_tab.contains_key(&original_id));
    assert!(!window.tab_chrome_titles_by_tab.contains_key(&shared_id));
}

#[test]
fn document_open_uses_host_grant_and_native_tab_path() {
    let mut app = App::headless_for_test();
    if let Some(window) = app.windows.get_mut(&WindowId(0)) {
        // The stacked Split needs room for both document panes below their chrome.
        window.rows = 24;
    }
    let dir = std::env::temp_dir().join(format!("aterm-control-document-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("README.md");
    std::fs::write(&path, "# Native app\n").unwrap();
    let request = format!(
        "app markdown {}",
        crate::native_document_host::path_to_file_uri(&path).expect("a file uri")
    );
    let opened = app.open_app(parse_open(&request).unwrap()).unwrap();
    assert!(opened.starts_with("app editor file://"));
    assert!(
        app.inspect_tabs_body()
            .iter()
            .any(|line| line.contains("app=editor"))
    );
    let identity = app.active_native_view(WindowId(0)).unwrap();
    let (instance, view) = identity;
    let document = app.native_runtime.document_id(instance).unwrap();
    let original = app.document_store.snapshot(document).unwrap();
    let audit = app
        .inspect_app(InspectRequest::View {
            view,
            projection: InspectionProjection::Audit,
        })
        .unwrap();
    assert!(audit[1].contains("compiled-fingerprint="));
    assert!(audit.iter().any(|line| {
        line.starts_with("markdown-view ")
            && line.contains("mode=read")
            && line.contains("selected-mode=read")
            && line.contains("status=\"Saved")
    }));
    assert!(audit.iter().any(|line| {
        line.starts_with("paint-markdown ")
            && line.contains("block-kind=heading-1")
            && line.contains("source=0..")
    }));
    assert!(
        audit.iter().any(|line| {
            line.starts_with("paint-node key=\"editor/markdown/read\"")
                && line.contains("selected:true")
                && line.contains("action=editor/markdown/read")
        }),
        "{audit:#?}"
    );
    for mode in ["edit", "split", "read"] {
        let key = format!("editor/markdown/{mode}");
        app.act_app(ActRequest {
            view,
            ui_key: key.clone(),
            action: key,
            value: None,
        })
        .unwrap();
        let text = app
            .inspect_app(InspectRequest::View {
                view,
                projection: InspectionProjection::Text,
            })
            .unwrap();
        assert!(text.iter().any(|line| {
            line.starts_with(&format!("markdown-view mode={mode} selected-mode={mode} "))
                && line.contains("status=\"Saved")
        }));
        assert_eq!(
            text.iter().any(|line| line.starts_with("editor-row ")),
            mode != "read",
            "{mode} source projection: {text:#?}"
        );
        assert_eq!(
            text.iter().any(|line| line.starts_with("markdown-block ")),
            mode != "edit",
            "{mode} reader projection: {text:#?}"
        );
        assert_eq!(app.active_native_view(WindowId(0)), Some(identity));
        assert_eq!(app.native_runtime.document_id(instance), Some(document));
        let after = app.document_store.snapshot(document).unwrap();
        assert_eq!(after.seq, original.seq);
        assert_eq!(after.text, original.text);
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn advertised_editor_buffer_action_focuses_only_its_live_view() {
    let dir = std::env::temp_dir().join(format!(
        "aterm-control-editor-buffer-focus-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("target.txt");
    let other_path = dir.join("other.txt");
    std::fs::write(&path, "aéz\n").unwrap();
    std::fs::write(&other_path, "other window\n").unwrap();
    let mut app = App::headless_for_test();
    let target_window = WindowId(0);
    app.open_document_tab_in_window(
        target_window,
        crate::native_app::AppKind::Editor,
        &crate::native_document_host::path_to_file_uri(&path).unwrap(),
    )
    .unwrap();
    let (instance, view) = app.active_native_view(target_window).unwrap();
    let document = app.native_runtime.document_id(instance).unwrap();
    app.dispatch_native_view_event(
        target_window,
        view,
        AppEvent::EditorSetSelection { anchor: 1, head: 3 },
    )
    .unwrap();
    app.reconcile_active_editor_viewport(target_window).unwrap();
    let target_tab = app.windows[&target_window]
        .tab_set
        .active()
        .unwrap()
        .clone();

    let session = app.next_session_id;
    let front_window = app.insert_logical_window(crate::stub_session(session), 24, 80);
    app.open_document_tab_in_window(
        front_window,
        crate::native_app::AppKind::Editor,
        &crate::native_document_host::path_to_file_uri(&other_path).unwrap(),
    )
    .unwrap();
    let (other_instance, other_view) = app.active_native_view(front_window).unwrap();
    let other_document = app.native_runtime.document_id(other_instance).unwrap();
    app.reconcile_active_editor_viewport(front_window).unwrap();
    let front_tab = app.windows[&front_window].tab_set.active().unwrap().clone();
    let editor_state = |app: &App, view| {
        let Some(crate::native_app::AppViewState::Editor(state)) =
            app.native_runtime.view_state(view)
        else {
            panic!("editor view remains attached")
        };
        state.as_ref().clone()
    };
    let document_state = |app: &App, document| {
        let snapshot = app.document_store.snapshot(document).unwrap();
        (
            snapshot.seq,
            snapshot.text,
            app.document_store.dirty(document),
            app.document_store.checkpoint_seq(document),
        )
    };
    let before = editor_state(&app, view);
    let other_before = editor_state(&app, other_view);
    let document_before = document_state(&app, document);
    let other_document_before = document_state(&app, other_document);
    let initial_lifetime = before.common.input_lifetime();
    let other_lifetime = other_before.common.input_lifetime();
    assert_eq!(app.frontmost_window, Some(front_window));
    assert!(before.common.last_focus.is_none());

    let key = UiKey::new("editor/buffer");
    let inspected = app.compile_view(view).unwrap();
    let node = inspected.compiled.semantic(&key).unwrap();
    assert_eq!(node.role, SemanticRole::TextViewport);
    assert_eq!(
        node.action.as_ref().unwrap().as_str(),
        "editor/focus-buffer"
    );
    assert!(node.state.is_none_or(|state| state.enabled));
    assert!(!node.offscreen);
    assert!(inspected.compiled.focus_order.contains(&key));
    let request = parse_act(&format!(
        "app/v1 view {} editor/buffer editor/focus-buffer",
        view.get()
    ))
    .unwrap();
    let mut wrong_action = request.clone();
    wrong_action.action = "editor/find".to_string();
    assert!(app.act_app(wrong_action).is_err());
    let mut supplied_value = request.clone();
    supplied_value.value = Some("must not replace this document".to_string());
    assert!(app.act_app(supplied_value).is_err());
    assert_eq!(editor_state(&app, view), before);
    assert!(std::sync::Arc::ptr_eq(
        &initial_lifetime,
        &editor_state(&app, view).common.input_lifetime()
    ));

    let mut failures = Vec::new();
    if let Err(error) = app.act_app(request.clone()) {
        failures.push(format!("advertised buffer action refused: {error}"));
    }
    let focused = editor_state(&app, view);
    if focused.common.last_focus.as_ref() != Some(&key) || !focused.common.focus_visible {
        failures.push("buffer action did not install visible field focus".to_string());
    }
    let focused_lifetime = focused.common.input_lifetime();
    if std::sync::Arc::ptr_eq(&initial_lifetime, &focused_lifetime) {
        failures.push("buffer focus did not retire the previous input lifetime".to_string());
    }
    let painted = app.compile_view(view).unwrap();
    let viewport = painted
        .compiled
        .paint
        .iter()
        .find_map(|paint| {
            if paint.key != key {
                return None;
            }
            let crate::native_ui::UiContent::TextViewport(spec) = &paint.content else {
                return None;
            };
            Some(spec)
        })
        .unwrap();
    if !viewport.focused {
        failures.push("buffer action did not reach the focused viewport paint".to_string());
    }
    if let Err(error) = app.act_app(request.clone()) {
        failures.push(format!("repeated buffer action refused: {error}"));
    }
    assert!(
        std::sync::Arc::ptr_eq(
            &focused_lifetime,
            &editor_state(&app, view).common.input_lifetime()
        ),
        "reaffirming the same field preserves its input lifetime"
    );
    assert_eq!(editor_state(&app, view).buffer, before.buffer);
    assert_eq!(
        app.windows[&target_window].tab_set.active(),
        Some(&target_tab)
    );
    assert_eq!(
        app.windows[&front_window].tab_set.active(),
        Some(&front_tab)
    );
    assert_eq!(app.frontmost_window, Some(front_window));
    assert_eq!(editor_state(&app, other_view), other_before);
    assert!(std::sync::Arc::ptr_eq(
        &other_lifetime,
        &editor_state(&app, other_view).common.input_lifetime()
    ));

    assert!(
        app.open_settings_tab_in_window(
            target_window,
            crate::native_settings::SettingsRoute::About
        )
    );
    let (_, settings_view) = app.active_native_view(target_window).unwrap();
    let settings_before = app
        .native_runtime
        .view_state(settings_view)
        .unwrap()
        .common()
        .clone();
    let inactive_before = editor_state(&app, view);
    assert!(
        app.act_app(request.clone()).is_err(),
        "an inactive view cannot borrow the current native target"
    );
    assert_eq!(editor_state(&app, view), inactive_before);
    assert_eq!(
        app.native_runtime
            .view_state(settings_view)
            .unwrap()
            .common(),
        &settings_before
    );
    let index = app.windows[&target_window]
        .tab_set
        .tabs()
        .iter()
        .position(|tab| tab.id == target_tab.id)
        .unwrap();
    assert!(!app.close_tab_at(target_window, index));
    assert!(app.native_runtime.view_state(view).is_none());
    assert!(
        app.act_app(request).is_err(),
        "a retired view cannot redirect its action to a replacement"
    );
    assert_eq!(
        app.active_native_view(target_window).unwrap().1,
        settings_view
    );
    assert_eq!(
        app.native_runtime
            .view_state(settings_view)
            .unwrap()
            .common(),
        &settings_before
    );
    assert_eq!(app.frontmost_window, Some(front_window));
    assert_eq!(
        app.windows[&front_window].tab_set.active(),
        Some(&front_tab)
    );
    assert_eq!(editor_state(&app, other_view), other_before);
    assert!(std::sync::Arc::ptr_eq(
        &other_lifetime,
        &editor_state(&app, other_view).common.input_lifetime()
    ));
    assert_eq!(document_state(&app, document), document_before);
    assert_eq!(document_state(&app, other_document), other_document_before);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "aéz\n");
    assert_eq!(
        std::fs::read_to_string(&other_path).unwrap(),
        "other window\n"
    );
    assert!(app.structural_invariants_ok());
    let _ = std::fs::remove_dir_all(dir);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn settings_inspection_and_exact_action_are_end_to_end() {
    let mut app = App::headless_for_test();
    if let Some(window) = app.windows.get_mut(&WindowId(0)) {
        window.cols = 140;
        window.rows = 100;
    }
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::About));
    let tabs = app.inspect_app(InspectRequest::Tabs).unwrap();
    assert!(tabs[0].starts_with("app-inspection version=app/v1"));
    assert!(tabs.iter().any(|line| line.contains("app=settings")));

    let (_, view) = app.active_native_view(WindowId(0)).unwrap();
    let controls = app
        .inspect_app(InspectRequest::View {
            view,
            projection: InspectionProjection::Controls,
        })
        .unwrap();
    assert!(controls.iter().any(|line| line.contains("about/open-site")));
    let audit = app
        .inspect_app(InspectRequest::View {
            view,
            projection: InspectionProjection::Audit,
        })
        .unwrap();
    assert!(audit[0].contains("projection=audit"));
    assert!(audit.iter().any(|line| line.starts_with("paint-audit ")));
    assert!(
        audit
            .iter()
            .any(|line| line.contains("key=\"about/wordmark\""))
    );

    assert!(
        app.act_app(ActRequest {
            view,
            ui_key: "settings/nav/appearance".to_string(),
            action: "settings/route/appearance".to_string(),
            value: None,
        })
        .is_ok()
    );
    assert!(
        app.act_app(ActRequest {
            view,
            ui_key: "settings/nav/appearance".to_string(),
            action: "updates/install-now".to_string(),
            value: None,
        })
        .unwrap_err()
        .contains("does not match")
    );
}

/// `act` refuses every Security panel button before the reducer runs, while the
/// same action dispatched as a press still works, so the refusal is the fence.
#[test]
fn the_security_panels_owner_gestures_are_refused_on_the_control_surface() {
    use crate::native_settings::{
        MACOS_ACCESS_ASK_AGAIN, MACOS_ACCESS_ASKPASS_OFF, MACOS_ACCESS_ASKPASS_ON,
        MACOS_ACCESS_DEBUGGER_OFF, MACOS_ACCESS_DEBUGGER_ON, MACOS_ACCESS_GESTURE_PREFIX,
        MACOS_ACCESS_OPEN_DEVTOOLS, MACOS_ACCESS_TRASH, MacosAccess, SettingsRoute,
    };
    use aterm_containment::consent::{
        DebuggerChange, DebuggerMember, DebuggerMode, DebuggerProbe, DebuggerProbeLabel,
    };
    for action in [
        MACOS_ACCESS_ASK_AGAIN,
        MACOS_ACCESS_TRASH,
        MACOS_ACCESS_DEBUGGER_ON,
        MACOS_ACCESS_DEBUGGER_OFF,
        MACOS_ACCESS_OPEN_DEVTOOLS,
        MACOS_ACCESS_ASKPASS_OFF,
        MACOS_ACCESS_ASKPASS_ON,
    ] {
        assert!(action.starts_with(MACOS_ACCESS_GESTURE_PREFIX), "{action}");
    }
    use aterm_containment::consent::{Claimant, Claimants, DrClass, Enumeration, Folder};
    let copy = |path: &str, dr_text: &str, running: bool| Claimant {
        path: std::path::PathBuf::from(path),
        dr: aterm_containment::consent::classify_dr(dr_text),
        dr_text: dr_text.to_string(),
        signing: if running { "developer-id" } else { "adhoc" },
        team: None,
        running,
        links: Vec::new(),
    };
    let access = MacosAccess {
        enabled: true,
        dr: DrClass::Identity,
        bundle_id: Some(crate::native_settings::tests::fixture_subject(
            "com.example.fixture",
        )),
        tccutil: aterm_containment::TccutilPresence::Executable,
        warmup_rows: vec![(Folder::Documents, crate::consent_warmup::WarmupRow::Denied)],
        claimants: Some(Claimants {
            found: vec![
                copy(
                    "/A/x.app",
                    "designated => identifier \"x\" and anchor apple generic and \
                     certificate leaf[subject.OU] = \"T\"",
                    true,
                ),
                copy("/A/x.app.rollback", "designated => cdhash H\"aa\"", false),
            ],
            enumeration: Enumeration::Complete,
        }),
        trash_tool: true,
        debugger: crate::debugger_access::DebuggerFacts {
            enabled: true,
            probe: DebuggerProbe {
                mode: DebuggerMode::Disabled,
                member: DebuggerMember::Admin,
                label: DebuggerProbeLabel::StatusOk,
            },
            self_hardened: aterm_containment::consent::SelfHardening::Hardened,
            busy: false,
            ..crate::debugger_access::DebuggerFacts::default()
        },
        ..MacosAccess::default()
    };
    let mut app = App::headless_for_test();
    if let Some(window) = app.windows.get_mut(&WindowId(0)) {
        window.cols = 140;
        window.rows = 160;
    }
    assert!(app.open_settings_tab(SettingsRoute::Security));
    fn state(app: &mut App, view: ViewId) -> &mut crate::native_settings::SettingsViewState {
        match app.native_runtime.view_state_mut(view) {
            Some(crate::native_app::AppViewState::Settings(state)) => state,
            _ => panic!("the Settings view is live"),
        }
    }
    let wid = WindowId(0);
    let (_, view) = app.active_native_view(wid).unwrap();
    state(&mut app, view).replace_macos_access(access);

    let controls = app
        .inspect_app(InspectRequest::View {
            view,
            projection: InspectionProjection::Controls,
        })
        .unwrap();
    for action in [
        MACOS_ACCESS_ASK_AGAIN,
        MACOS_ACCESS_TRASH,
        MACOS_ACCESS_DEBUGGER_ON,
        MACOS_ACCESS_OPEN_DEVTOOLS,
    ] {
        assert!(
            controls
                .iter()
                .any(|line| line.contains(&format!("action={action}"))),
            "the button is on the page, so the refusal is not vacuous: {controls:#?}"
        );
        let refused = app
            .act_app(ActRequest {
                view,
                ui_key: action.to_string(),
                action: action.to_string(),
                value: None,
            })
            .unwrap_err();
        assert!(refused.contains("does not press"), "{refused}");
    }
    let panel = state(&mut app, view);
    assert!(
        panel
            .macos_access_for_test()
            .is_some_and(|access| access.reset.is_none()),
        "the refused Ask Again cleared nothing"
    );
    assert_eq!(panel.take_claimant_retire_request(), None);
    assert_eq!(
        panel.take_debugger_change_request(),
        None,
        "the refused Allow recorded no change"
    );
    assert!(
        !panel.take_devtools_hand_over_request(),
        "the refused Open Developer Tools asked the host for nothing"
    );

    state(&mut app, view)
        .arm_consent_gestures(crate::native_settings::ConsentGestures::confirming());
    for action in [
        MACOS_ACCESS_ASK_AGAIN,
        MACOS_ACCESS_TRASH,
        MACOS_ACCESS_DEBUGGER_ON,
        MACOS_ACCESS_OPEN_DEVTOOLS,
    ] {
        app.dispatch_native_view_event(
            wid,
            view,
            AppEvent::Action(ActionInvocation {
                id: ActionId::new(action),
                value: None,
            }),
        )
        .unwrap();
    }
    let panel = state(&mut app, view);
    assert!(
        panel
            .macos_access_for_test()
            .is_some_and(|access| access.reset.is_some())
    );
    assert_eq!(
        panel.take_claimant_retire_request(),
        Some(vec![std::path::PathBuf::from("/A/x.app.rollback")])
    );
    assert_eq!(
        panel
            .take_debugger_change_request()
            .map(|(change, _)| change),
        Some(DebuggerChange::On)
    );
    assert!(
        panel.take_devtools_hand_over_request(),
        "the pressed Open Developer Tools asks the host for the hand-over"
    );
}

/// `act` refuses an action of Settings ▸ Harness's Owner rules card by its prefix alone,
/// naming the window, before the page is asked whether it shows the button.
/// NEGATIVE CONTROL: a harness action off the person-only list, on the same page, is pressed.
#[test]
fn act_refuses_the_owner_rules_presses_and_presses_another_harness_action() {
    use crate::native_settings::{OWNER_RULES_PREFIX, SettingsRoute};
    let mut app = App::headless_for_test();
    if let Some(window) = app.windows.get_mut(&WindowId(0)) {
        window.cols = 140;
        window.rows = 160;
    }
    assert!(app.open_settings_tab(SettingsRoute::Harness));
    let (_, view) = app.active_native_view(WindowId(0)).unwrap();
    for what in ["confirm/0123456789ab", "withdraw/x"] {
        let action = format!("{OWNER_RULES_PREFIX}{what}");
        let refused = app
            .act_app(ActRequest {
                view,
                ui_key: action.clone(),
                action: action.clone(),
                value: None,
            })
            .unwrap_err();
        assert_eq!(
            Some(refused.clone()),
            super::person_only_refusal(&action),
            "the person-only list refused it, not the tree: {refused}"
        );
        assert!(
            refused.contains("Owner rules") && refused.contains("in the window"),
            "{refused}"
        );
    }

    let toggle = "harness/config/toggle";
    assert!(!toggle.starts_with(OWNER_RULES_PREFIX));
    let controls = app
        .inspect_app(InspectRequest::View {
            view,
            projection: InspectionProjection::Controls,
        })
        .unwrap();
    assert!(
        controls
            .iter()
            .any(|line| line.contains(&format!("action={toggle}"))),
        "the control is on the page: {controls:#?}"
    );
    assert!(
        app.act_app(ActRequest {
            view,
            ui_key: toggle.to_string(),
            action: toggle.to_string(),
            value: None,
        })
        .is_ok_and(|acted| acted.contains(toggle)),
        "a harness action off the list is pressed"
    );
}

/// A PERSON-ONLY BUTTON TAKES A PERSON'S OWN PRESS: Settings ▸ Security's Full Disk Access
/// button pressed through the window's accessibility tree (a tab's program reaches it with
/// aterm's own Accessibility grant) is refused by the list `act` refuses it by, and so is a
/// control client's key or click on it, and a key or click another process posted into the
/// window (`InputOrigin::Relayed`), the page left as it was. The refusal stands where every
/// input meets the page's reducer, so the action dispatched as a palette command dispatches
/// it, under any of those marks, is refused too.
/// NEGATIVE CONTROL: the person's own press of it is taken. With the relayed mark left out of
/// `App::gesture_unvouched`, the relayed press met no refusal.
#[cfg(a11y_tree)]
#[test]
fn a_person_only_button_is_pressed_by_no_accessibility_or_control_client() {
    use crate::native_app::{ActionInvocation, AppViewState};
    use crate::native_settings::{MACOS_ACCESS_OPEN_FDA, SettingsRoute};
    #[derive(Clone, Copy, Debug)]
    enum By {
        Assistive,
        Controller,
        Relayed,
        Person,
    }
    let wid = WindowId(0);
    for by in [By::Assistive, By::Controller, By::Relayed, By::Person] {
        let mut app = App::headless_for_test();
        if let Some(window) = app.windows.get_mut(&wid) {
            window.cols = 160;
            window.rows = 60;
        }
        assert!(app.open_settings_tab(SettingsRoute::Security));
        app.control_security_posture(&crate::security_posture::Asked::Stage(Some(
            "denied".to_string(),
        )))
        .unwrap();
        let (_, view) = app.active_native_view(wid).unwrap();
        let feedback = |app: &App| match app.native_runtime.view_state(view) {
            Some(AppViewState::Settings(state)) => state.feedback.clone(),
            _ => panic!("the Settings view is live"),
        };
        let before = feedback(&app);
        let id = ActionId::new(MACOS_ACCESS_OPEN_FDA);
        let command = |app: &mut App| {
            app.dispatch_native_view_event(
                wid,
                view,
                crate::native_app::AppEvent::Action(ActionInvocation {
                    id: id.clone(),
                    value: None,
                }),
            )
        };
        let press = |app: &mut App| {
            app.dispatch_native_control_action(
                wid,
                view,
                UiKey::new(MACOS_ACCESS_OPEN_FDA),
                ActionInvocation {
                    id: id.clone(),
                    value: None,
                },
                false,
            )
        };
        match by {
            By::Assistive => {
                let request = app.accessibility_request_for_test(
                    wid,
                    view,
                    MACOS_ACCESS_OPEN_FDA,
                    aterm_a11y::model::Action::Click,
                    None,
                );
                app.on_accessibility_action(wid, request);
                let refusal = app.with_assistive_source(|app| app.person_only_press_refusal(&id));
                assert!(refusal.is_some(), "the list names the button");
                assert_eq!(app.with_assistive_source(press).err(), refusal);
                assert_eq!(app.with_assistive_source(command).err(), refusal);
            }
            By::Controller => {
                let refusal =
                    app.with_controller_mark(true, |app| app.person_only_press_refusal(&id));
                assert!(refusal.is_some(), "the list names the button");
                assert_eq!(app.with_controller_mark(true, press).err(), refusal);
                assert_eq!(app.with_controller_mark(true, command).err(), refusal);
            }
            By::Relayed => {
                use aterm_window::event::InputOrigin::Relayed;
                let refusal =
                    app.with_origin_mark(Relayed, |app| app.person_only_press_refusal(&id));
                assert!(refusal.is_some(), "the list names the button");
                assert_eq!(app.with_origin_mark(Relayed, press).err(), refusal);
                assert_eq!(app.with_origin_mark(Relayed, command).err(), refusal);
            }
            By::Person => {
                assert_eq!(app.person_only_press_refusal(&id), None);
                assert_eq!(press(&mut app), Ok(EventResult::Handled));
            }
        }
        let after = feedback(&app);
        if matches!(by, By::Person) {
            assert!(after.is_some() && after != before, "the press was taken");
        } else {
            assert_eq!(after, before, "{by:?}: the page is as it was");
        }
    }
}

#[test]
fn askpass_settings_controller_keys_need_an_owner_answer() {
    use crate::native_settings::{
        MACOS_ACCESS_ASKPASS_OFF, MACOS_ACCESS_ASKPASS_ON, MacosAccess, SettingsRoute,
    };
    for (was_off, action) in [
        (false, MACOS_ACCESS_ASKPASS_OFF),
        (true, MACOS_ACCESS_ASKPASS_ON),
    ] {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let window = app.windows.get_mut(&wid).unwrap();
        window.cols = 180;
        window.rows = 200;
        assert!(app.open_settings_tab(SettingsRoute::Security));
        let (_, view) = app.active_native_view(wid).unwrap();
        let Some(crate::native_app::AppViewState::Settings(state)) =
            app.native_runtime.view_state_mut(view)
        else {
            panic!("Settings is active");
        };
        state.replace_macos_access(MacosAccess {
            enabled: true,
            askpass_relay_off: was_off,
            ..MacosAccess::default()
        });
        state.common.last_focus = Some(UiKey::new(action));
        let compiled = app.compiled_native_ui(wid).unwrap();
        assert!(
            compiled.semantic(&UiKey::new(action)).is_some(),
            "the requested direction is a real drawn control"
        );
        let refused = app
            .act_app(ActRequest {
                view,
                ui_key: action.to_string(),
                action: action.to_string(),
                value: None,
            })
            .unwrap_err();
        assert!(refused.contains("does not press"), "{refused}");
        assert_eq!(
            app.input(
                wid,
                crate::input::InputEvent::Key {
                    key: aterm_types::keyboard::Key::Named(aterm_types::keyboard::NamedKey::Enter,),
                    mods: aterm_types::keyboard::Modifiers::default(),
                    base_layout: None,
                    layout_text: None,
                    event_type: aterm_types::keyboard::KeyEventType::Press,
                },
                crate::input::Source::Controller,
            ),
            crate::input::InputOutcome::Ok
        );
        let Some(crate::native_app::AppViewState::Settings(state)) =
            app.native_runtime.view_state_mut(view)
        else {
            panic!("Settings remains active");
        };
        assert_eq!(
            state.take_askpass_off_request(),
            None,
            "a controller keyboard activation cannot supply an owner's answer"
        );
    }
}

#[test]
fn inspection_reuses_staged_glass_after_responsive_resize_and_route_change() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let (cell_width, cell_height) = app.win_cell_size(wid);
    let pad = app.win_pad(wid);
    let wide_columns = (2_100usize.saturating_sub(pad * 2) / cell_width.max(1)).max(1);
    let medium_columns = (2_000usize.saturating_sub(pad * 2) / cell_width.max(1)).max(1);
    let rows = (1_200usize.saturating_sub(pad * 2) / cell_height.max(1)).max(1);
    if let Some(window) = app.windows.get_mut(&wid) {
        window.scale = 2.0;
        window.cols = u16::try_from(wide_columns).unwrap();
        window.rows = u16::try_from(rows).unwrap();
    }
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Appearance));
    let (_, view) = app.active_native_view(wid).unwrap();

    assert!(app.prepare_native_input_scratch(wid));
    let wide = app.cached_native_ui(wid).unwrap().compiled.clone();
    assert!(
        wide.bounds.width >= 1_040.0,
        "wide bounds: {:?}",
        wide.bounds
    );
    let wide_rail = wide
        .semantic(&UiKey::new("settings/navigation"))
        .unwrap()
        .rect
        .width;
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::About));
    if let Some(window) = app.windows.get_mut(&wid) {
        window.cols = u16::try_from(medium_columns).unwrap();
    }
    assert!(app.prepare_native_input_scratch(wid));
    let retained_compilation = app.cached_native_ui(wid).unwrap().compiled.clone();
    let inspected = app.compile_view(view).unwrap();
    assert_eq!(inspected.source, InspectionCompileSource::Staged);
    assert_eq!(retained_compilation, inspected.compiled);
    assert!(
        (760.0..1_040.0).contains(&inspected.compiled.bounds.width),
        "medium-width bounds: {:?}",
        inspected.compiled.bounds
    );
    assert!(
        inspected
            .compiled
            .semantic(&UiKey::new("settings/compact-toolbar"))
            .is_none(),
        "a 1,000×600 logical window has room for the labeled macOS-style rail"
    );
    assert_ne!(
        inspected
            .compiled
            .semantic(&UiKey::new("settings/navigation"))
            .expect("medium navigation rail")
            .rect
            .width,
        wide_rail,
        "the staged artifact must reflect the wide-to-medium breakpoint"
    );

    let tree = app
        .inspect_app(InspectRequest::View {
            view,
            projection: InspectionProjection::Tree,
        })
        .unwrap();
    assert!(tree[1].contains("source=staged"));
    assert_eq!(
        &tree[2..],
        semantic_tree_lines(&retained_compilation),
        "control geometry and semantics are the staged app-render compilation"
    );

    assert!(
        app.windows
            .get_mut(&wid)
            .unwrap()
            .tab_set
            .switch_to_index(0)
    );
    app.sync_window(wid);
    let inactive = app.compile_view(view).unwrap();
    assert_eq!(inactive.source, InspectionCompileSource::InactiveFallback);
    let inactive_wire = app
        .inspect_app(InspectRequest::View {
            view,
            projection: InspectionProjection::Controls,
        })
        .unwrap();
    assert!(inactive_wire[1].contains("source=inactive-fallback"));
}

#[test]
fn inspection_uses_exact_retained_native_sibling_when_terminal_has_focus() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    let (instance, native_view) = app.active_native_view(wid).unwrap();
    let (_, terminal_view) =
        app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Horizontal);
    assert_eq!(
        app.windows[&wid].tab_set.active().unwrap().focus,
        terminal_view,
        "the terminal sibling owns focus"
    );
    app.prepare_heterogeneous_input_scratch(wid)
        .expect("mixed frame is retained");
    let retained = app.windows[&wid].leaf_render_cache[&native_view]
        .native
        .as_ref()
        .unwrap()
        .compiled
        .clone();

    let inspected = app.compile_view(native_view).unwrap();
    assert_eq!(inspected.source, InspectionCompileSource::Retained);
    assert_eq!(inspected.compiled, retained);
    let wire = app
        .inspect_app(InspectRequest::View {
            view: native_view,
            projection: InspectionProjection::Audit,
        })
        .unwrap();
    assert!(wire[1].contains("source=retained"), "{wire:#?}");
    assert!(wire[1].contains(&format!(
        "compiled-fingerprint={:016x}",
        retained.fingerprint()
    )));

    let outcome = app
        .native_runtime
        .dispatch(
            instance,
            native_view,
            AppEvent::Action(ActionInvocation {
                id: ActionId::new("settings/route/appearance"),
                value: None,
            }),
        )
        .unwrap();
    assert_eq!(outcome.result, EventResult::Handled);
    app.invalidate_native_view_cache(wid, native_view, crate::native_app::DamageRegion::All);

    let stale_on_glass = app.compile_view(native_view).unwrap();
    assert_eq!(stale_on_glass.source, InspectionCompileSource::Retained);
    assert!(!stale_on_glass.model_current);
    assert_eq!(stale_on_glass.compiled, retained);
    let stale_wire = app
        .inspect_app(InspectRequest::View {
            view: native_view,
            projection: InspectionProjection::Audit,
        })
        .unwrap();
    assert!(stale_wire[1].contains("source=retained"));
    assert!(stale_wire[1].contains("model-current=false"));
}

#[test]
fn an_invalidated_retained_sibling_inspects_as_shown_not_current() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    let (_, native_view) = app.active_native_view(wid).unwrap();
    app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Horizontal);
    app.prepare_heterogeneous_input_scratch(wid)
        .expect("mixed frame is retained");
    let raster = app
        .windows
        .get_mut(&wid)
        .and_then(|window| window.leaf_render_cache.get_mut(&native_view))
        .and_then(|cache| cache.native.as_mut())
        .unwrap();
    raster.presented = true;
    let retained = raster.compiled.clone();
    let stamp = raster.stamp;
    assert!(app.compile_view(native_view).unwrap().model_current);

    // Hover, pressed and focus marks owe pixels, not a different tree.
    app.invalidate_native_pointer_style(wid, native_view);
    let feedback = app.compile_view(native_view).unwrap();
    assert_eq!(feedback.source, InspectionCompileSource::Retained);
    assert!(
        feedback.model_current,
        "pointer feedback is no model change"
    );
    assert!(
        app.retained_native_pointer_artifact(wid, native_view)
            .is_some()
    );
    assert!(
        app.retained_native_leaf_artifact(wid, native_view, true)
            .is_none()
    );

    // The invalidation leaves the stamp as it was and glass still shows the raster, but what
    // the view draws may have moved where the stamp does not look: the feedback still owed
    // on it cannot carry the pointer through.
    app.invalidate_native_ui_cache(wid);
    assert!(
        app.retained_native_pointer_artifact(wid, native_view)
            .is_none()
    );
    let invalidated = app.compile_view(native_view).unwrap();
    let cache = &app.windows[&wid].leaf_render_cache[&native_view];
    assert_eq!(cache.native.as_ref().unwrap().stamp, stamp);
    assert_eq!(invalidated.source, InspectionCompileSource::Retained);
    assert_eq!(invalidated.compiled, retained);
    assert!(!invalidated.model_current);
    let wire = app
        .inspect_app(InspectRequest::View {
            view: native_view,
            projection: InspectionProjection::Audit,
        })
        .unwrap();
    assert!(wire[1].contains("source=retained"), "{wire:#?}");
    assert!(wire[1].contains("model-current=false"), "{wire:#?}");

    app.prepare_heterogeneous_input_scratch(wid)
        .expect("mixed frame is retained again");
    assert!(app.compile_view(native_view).unwrap().model_current);
}

#[test]
fn settings_preview_and_inactive_inspection_use_logical_window_font_size() {
    let mut app = App::headless_for_test();
    app.windows.get_mut(&WindowId(0)).unwrap().metrics.font_px = 12.0;
    let next_session = app.next_session_id;
    let second = app.insert_logical_window(crate::stub_session(next_session), 50, 140);
    {
        let window = app.windows.get_mut(&second).unwrap();
        window.metrics.font_px = 36.0;
        window.scale = 2.0;
    }
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::TextFonts));
    let (_, view) = app.active_native_view(second).unwrap();

    let specimen_size = |compiled: &crate::native_ui::CompiledUi| {
        compiled
            .paint
            .iter()
            .find_map(|paint| match &paint.content {
                crate::native_ui::UiContent::SettingsPreview(spec) => Some(spec.font_px),
                _ => None,
            })
            .expect("Typography specimen")
    };
    for scale in [1.0, 1.25, 2.0, 3.0] {
        let window = app.windows.get_mut(&second).unwrap();
        window.scale = scale;
        window.metrics.font_px = 18.0 * scale as f32;
        let compiled = app.compiled_native_ui(second).unwrap();
        let logical = specimen_size(&compiled);
        assert_eq!(logical, 18.0, "display scale {scale}");
        assert_eq!(logical * scale as f32, app.win_font_px(second));
    }
    app.windows.get_mut(&second).unwrap().scale = 2.0;
    app.windows.get_mut(&second).unwrap().metrics.font_px = 36.0;

    assert!(
        app.windows
            .get_mut(&second)
            .unwrap()
            .tab_set
            .switch_to_index(0)
    );
    app.sync_window(second);
    assert_eq!(app.font_px, crate::FONT_PX);

    let inspected = app.compile_view(view).unwrap();
    assert_eq!(inspected.source, InspectionCompileSource::InactiveFallback);
    assert_eq!(inspected.window, second);
    assert_eq!(inspected.scale, 2.0);
    assert_eq!(specimen_size(&inspected.compiled), 18.0);
    let preview = inspected
        .compiled
        .semantics
        .iter()
        .find(|node| node.label == "Typography preview")
        .expect("Typography renderer preview semantics");
    let SemanticValue::Text(value) = &preview.value else {
        panic!("Typography preview has a text semantic value");
    };
    assert!(
        value.contains("at 18 pixels"),
        "inactive inspection must report the owning window's font size: {value}"
    );

    let wire = app
        .inspect_app(InspectRequest::View {
            view,
            projection: InspectionProjection::Text,
        })
        .unwrap();
    assert!(wire[1].contains("source=inactive-fallback"));
    assert!(wire.iter().any(|line| line.contains("at 18 pixels")));
}

#[test]
fn editor_inspection_and_command_controls_share_the_live_reducer() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    if let Some(window) = app.windows.get_mut(&wid) {
        window.cols = 92;
        window.rows = 34;
    }
    let dir = std::env::temp_dir().join(format!(
        "aterm-control-editor-inspection-{}-{:?}",
        std::process::id(),
        std::thread::current().id(),
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("notes.txt");
    let original = "alpha needle\nbeta line\n";
    std::fs::write(&path, original).unwrap();
    let uri = crate::native_document_host::path_to_file_uri(&path).expect("a file uri");
    app.open_app(parse_open(&format!("app editor {uri}")).unwrap())
        .unwrap();
    let (instance, view) = app.active_native_view(wid).unwrap();
    let document = app.native_runtime.document_id(instance).unwrap();

    let inspect = |app: &App, projection| {
        app.inspect_app(InspectRequest::View { view, projection })
            .unwrap()
    };
    let lines = inspect(&app, InspectionProjection::Text);
    assert!(lines.iter().any(|line| {
        line.contains("editor-row")
            && line.contains(r#"text="alpha needle""#)
            && line.contains("carets=[0@0:primary]")
            && line.contains("selections=[]")
    }));
    let Some(crate::native_app::AppViewState::Editor(state)) =
        app.native_runtime.view_state_mut(view)
    else {
        panic!("editor state remains live");
    };
    state.buffer.as_mut().unwrap().selections =
        vec![crate::native_editor::Selection { anchor: 0, head: 5 }];
    let selected_tree = inspect(&app, InspectionProjection::Tree);
    assert!(selected_tree.iter().any(|line| {
        line.contains("editor-row")
            && line.contains("carets=[5@5:primary]")
            && line.contains("selections=[0..5@0..5:primary]")
    }));
    let Some(crate::native_app::AppViewState::Editor(state)) =
        app.native_runtime.view_state_mut(view)
    else {
        panic!("editor state remains live");
    };
    state.buffer.as_mut().unwrap().selections = vec![crate::native_editor::Selection::caret(0)];

    let before_find_revision = inspect(&app, InspectionProjection::Text)[0].clone();
    app.act_app(ActRequest {
        view,
        ui_key: "editor/find-button".to_string(),
        action: "editor/find".to_string(),
        value: None,
    })
    .unwrap();
    let find_inspection = inspect(&app, InspectionProjection::Text);
    assert_ne!(find_inspection[0], before_find_revision);
    assert!(find_inspection.iter().any(|line| {
        line.contains("editor-minibuffer")
            && line.contains("active=true")
            && line.contains("I-search:")
    }));
    let Some(crate::native_app::AppViewState::Editor(state)) = app.native_runtime.view_state(view)
    else {
        panic!("editor state remains live");
    };
    assert!(matches!(
        state.buffer.as_ref().unwrap().minibuffer,
        crate::native_editor::Minibuffer::Search { ref query, .. } if query.is_empty()
    ));

    let before_paste_revision = find_inspection[0].clone();
    assert!(app.native_input_event(
        wid,
        &crate::input::InputEvent::Paste("needle".to_string(), crate::input::PasteFraming::AtDrain),
    ));
    let Some(crate::native_app::AppViewState::Editor(state)) = app.native_runtime.view_state(view)
    else {
        panic!("editor state remains live");
    };
    assert!(matches!(
        state.buffer.as_ref().unwrap().minibuffer,
        crate::native_editor::Minibuffer::Search { ref query, .. } if query == "needle"
    ));
    assert_eq!(
        app.document_store.snapshot(document).unwrap().text.as_ref(),
        original
    );
    let search_text = inspect(&app, InspectionProjection::Text);
    assert_ne!(search_text[0], before_paste_revision);
    assert!(search_text.iter().any(|line| {
        line.contains("editor-minibuffer")
            && line.contains("active=true")
            && line.contains("I-search: needle")
    }));

    app.dispatch_native_view_event(
        wid,
        view,
        AppEvent::TextInput(crate::native_app::TextInputEvent::Cancel),
    )
    .unwrap();
    for (key, action, expected) in [
        ("editor/goto-line-button", "editor/goto-line", "Goto line:"),
        ("editor/commands-button", "editor/commands", "M-x"),
    ] {
        app.act_app(ActRequest {
            view,
            ui_key: key.to_string(),
            action: action.to_string(),
            value: None,
        })
        .unwrap();
        assert!(
            inspect(&app, InspectionProjection::Text)
                .iter()
                .any(|line| line.contains("editor-minibuffer") && line.contains(expected))
        );
        app.dispatch_native_view_event(
            wid,
            view,
            AppEvent::TextInput(crate::native_app::TextInputEvent::Cancel),
        )
        .unwrap();
    }

    app.dispatch_native_view_event(
        wid,
        view,
        AppEvent::TextInput(crate::native_app::TextInputEvent::Commit("!".to_string())),
    )
    .unwrap();
    assert_eq!(
        app.document_store.snapshot(document).unwrap().text.as_ref(),
        format!("!{original}")
    );
    for (key, action, expected_text) in [
        ("editor/undo-button", "editor/undo", original.to_string()),
        ("editor/redo-button", "editor/redo", format!("!{original}")),
    ] {
        app.act_app(ActRequest {
            view,
            ui_key: key.to_string(),
            action: action.to_string(),
            value: None,
        })
        .unwrap();
        assert_eq!(
            app.document_store.snapshot(document).unwrap().text.as_ref(),
            expected_text
        );
    }
    app.act_app(ActRequest {
        view,
        ui_key: "editor/save-button".to_string(),
        action: "editor/save".to_string(),
        value: None,
    })
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        format!("!{original}")
    );
    assert!(
        inspect(&app, InspectionProjection::Text)
            .iter()
            .any(|line| { line.contains("editor-status") && line.contains(r#"value="Saved""#) })
    );

    let _ = std::fs::remove_dir_all(dir);
}

/// The control surface may not raise the file-provider dialog no grant silences.
#[cfg(unix)]
#[test]
fn the_control_surface_refuses_only_cloud_storage_opens() {
    let scratch = aterm_tempfile::tempdir().expect("a scratch home");
    let home = std::fs::canonicalize(scratch.path()).expect("the scratch home");
    let uri =
        |p: &std::path::Path| crate::native_document_host::path_to_file_uri(p).expect("a file uri");
    let refuse = |uri: &str| crate::App::refuse_file_provider_open_in(uri, Some(home.clone()));

    for domain in ["Library/Mobile Documents", "Library/CloudStorage"] {
        let path = home.join(domain).join("provider").join("notes.md");
        assert!(refuse(&uri(&path)).is_err(), "{}", path.display());
    }

    for ordinary in [
        "Documents/notes.md",
        "Desktop/notes.md",
        "src/aterm/README.md",
    ] {
        let path = home.join(ordinary);
        assert!(refuse(&uri(&path)).is_ok(), "{}", path.display());
    }

    for opaque in ["https://example.invalid/x", "not-a-uri", ""] {
        assert!(refuse(opaque).is_ok(), "{opaque}");
    }
    let cloud = uri(&home.join("Library/CloudStorage/provider/notes.md"));
    assert!(crate::App::refuse_file_provider_open_in(&cloud, None).is_ok());
}
