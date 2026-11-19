// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::native_app::{ViewCx, ViewMotionCx};
use crate::native_ui::CompiledUi;
use crate::stats_deck::{DeckGeom, DeckInput, DeckStill, FULL_PT, deck_paint_kept, prims_fp};
use crate::stats_plan::DeckForm;

fn cx(width: f32, scale: f32) -> ViewCx<'static> {
    ViewCx {
        viewport: LogicalRect::new(0.0, 0.0, width, 900.0),
        animation_phase_ms: 0,
        motion: ViewMotionCx::default(),
        terminal_font_px: 13.0 * scale,
        terminal_scale: scale,
        semantic_font: None,
        document: None,
    }
}

fn page(snap: &Arc<Snapshot>) -> ResourcesApp {
    let mut app = ResourcesApp::default();
    assert!(
        app.publish(Arc::clone(snap), None),
        "a first reading is news"
    );
    app
}

fn roles() -> crate::settings::Roles {
    crate::settings::Roles::from_theme(aterm_render::Theme::default())
}

/// The deck as written by hand for a node at `rect`: the Full form across `rect`'s width in
/// device px, no network band, the still edge and an empty Tab scope, brought to points and to
/// the node's place.
fn expected(snap: &Snapshot, rect: LogicalRect, scale: f32, form: DeckForm) -> Vec<DrawPrim> {
    expected_at(snap, rect, scale, 1.0, form)
}

/// [`expected`] magnified by the text scale `text`: laid at `text` times the window's scale and
/// font across the node's device px.
fn expected_at(
    snap: &Snapshot,
    rect: LogicalRect,
    scale: f32,
    text: f32,
    form: DeckForm,
) -> Vec<DrawPrim> {
    expected_scoped(snap, rect, scale, text, form, &[], "")
}

/// The deck's input for `snap` on the page's ground in `form`, with `panes` (under `label`) as
/// its Tab lane: what a pinned lens on that tab takes.
fn deck_input<'a>(snap: &'a Snapshot, form: DeckForm, panes: &[u64], label: &str) -> DeckInput<'a> {
    let roles = roles();
    DeckInput {
        network: None,
        network_place: None,
        network_target: crate::network_deck::Target::Overview,
        disk_span: crate::stats_lens::DiskSpan::Day,
        cpu_policy_pending: false,
        lab: None,
        fleet: None,
        snap: Some(snap),
        prev_snap: None,
        tab_panes: panes.to_vec(),
        tab_label: label.to_owned(),
        tab_count: 0,
        inks: crate::stats_deck::cached_inks(roles.surface, roles.text_primary, roles.accent),
        dwc: false,
        marks: Vec::new(),
        plate: None,
        form,
        contrast: false,
        locale: crate::stats_deck::host_locale(Some(snap)),
        live: crate::stats_live::DeckLive::STILL,
    }
}

/// [`expected_at`] with `panes` (under `label`) as the deck's Tab lane.
fn expected_scoped(
    snap: &Snapshot,
    rect: LogicalRect,
    scale: f32,
    text: f32,
    form: DeckForm,
    panes: &[u64],
    label: &str,
) -> Vec<DrawPrim> {
    let geom = DeckGeom {
        w: (rect.width * scale).round(),
        h: (FULL_PT * f64::from(scale * text)).round() as f32,
        pad: 0.0,
        scale: scale * text,
        font_px: 13.0 * scale * text,
    };
    let input = deck_input(snap, form, panes, label);
    let (mut prims, _, _) = deck_paint_kept(&geom, &input, &mut DeckStill::default());
    crate::widget::scale_prims(&mut prims, 1.0 / scale);
    crate::widget::translate_prims(&mut prims, rect.x, rect.y);
    prims
}

/// The page's head is the deck's own Full paint at the page's width, at a wide and a narrow
/// page and at 1× and 2×: the compiled head node paints exactly what the deck's painter paints
/// for that width, scaled to points and placed at the node.
/// NEGATIVE CONTROL: the same expectation drawn Folded differs from the head, so the equality
/// is no accident of an empty paint.
#[test]
fn the_resources_pages_head_is_the_decks_full_paint_at_the_pages_width() {
    let snap = Arc::new(crate::stats_deck::tests::snapshot());
    let model = page(&snap);
    let mut fingerprints = Vec::new();
    for width in [1200.0, 420.0] {
        for scale in [1.0, 2.0] {
            let view = ResourcesViewState::default();
            let cx = cx(width, scale);
            let compiled = model.view(&view, &cx).compile(cx.viewport).unwrap();
            let index = compiled
                .paint
                .iter()
                .position(|node| node.key.as_str() == HEAD_KEY)
                .expect("the head is painted");
            let node = &compiled.paint[index];
            assert!(matches!(node.content, UiContent::Resources(_)));
            let painted =
                compiled.paint_motion_node(index, roles(), aterm_render::Theme::default(), 13.0, 0);
            assert!(
                !painted.is_empty(),
                "the head paints at {width} pt, {scale}x"
            );
            let full = expected(&snap, node.rect, scale, DeckForm::Full);
            assert_eq!(
                prims_fp(&painted),
                prims_fp(&full),
                "the head is the deck's Full paint at {width} pt, {scale}x"
            );
            assert!(
                (node.rect.height - (FULL_PT * f64::from(scale)).round() as f32 / scale).abs()
                    < 1e-3,
                "the head stands the Full table's height"
            );
            let folded = expected(&snap, node.rect, scale, DeckForm::Folded);
            assert_ne!(prims_fp(&painted), prims_fp(&folded), "NEGATIVE CONTROL");
            fingerprints.push(prims_fp(&painted));
        }
    }
    assert_ne!(
        fingerprints[0], fingerprints[2],
        "a narrow page lays the deck out anew"
    );
}

/// A reading the page already holds is no news; another one is.
#[test]
fn the_page_takes_a_reading_once() {
    let snap = Arc::new(crate::stats_deck::tests::snapshot());
    let mut model = page(&snap);
    assert!(!model.publish(Arc::clone(&snap), None));
    let mut later = (*snap).clone();
    later.seq += 1;
    assert!(model.publish(Arc::new(later), None));
    assert_eq!(model.reading().map(|s| s.seq), Some(snap.seq + 1));
}

/// Puts the appearance it held back when dropped.
struct Appearance(crate::native_appearance::AppearancePreferences);

impl Drop for Appearance {
    fn drop(&mut self) {
        crate::native_appearance::install_preferences(self.0);
    }
}

/// Under a raised text scale the head is the deck magnified, as the page's other text is: the
/// Full paint laid at the scale times the window's scale and font across the node's device px,
/// standing the Full table's height times the scale.
/// NEGATIVE CONTROL: the deck as the window draws it, unmagnified, at the same width differs,
/// and at text scale 1 the head is that deck.
#[test]
fn the_resources_pages_head_grows_with_the_pages_text_scale() {
    let _restore = Appearance(crate::native_appearance::current_preferences());
    let snap = Arc::new(crate::stats_deck::tests::snapshot());
    let model = page(&snap);
    let scale = 2.0;
    let head_at = |text: f32| {
        crate::native_appearance::install_preferences(
            crate::native_appearance::AppearancePreferences {
                text_scale: text,
                ..crate::native_appearance::current_preferences()
            },
        );
        let cx = cx(1200.0, scale);
        let compiled = model
            .view(&ResourcesViewState::default(), &cx)
            .compile(cx.viewport)
            .unwrap();
        let index = compiled
            .paint
            .iter()
            .position(|node| node.key.as_str() == HEAD_KEY)
            .expect("the head is painted");
        let rect = compiled.paint[index].rect;
        let painted =
            compiled.paint_motion_node(index, roles(), aterm_render::Theme::default(), 13.0, 0);
        (rect, painted)
    };
    let (plain_rect, plain) = head_at(1.0);
    let text = 1.6;
    let (rect, painted) = head_at(text);
    assert_eq!(
        prims_fp(&painted),
        prims_fp(&expected_at(&snap, rect, scale, text, DeckForm::Full)),
        "the head is the deck magnified by the text scale"
    );
    assert!(
        (rect.height - (FULL_PT * f64::from(scale * text)).round() as f32 / scale).abs() < 1e-3,
        "the head stands the Full table's height times the text scale: {}",
        rect.height
    );
    assert!(rect.height > plain_rect.height * 1.5, "the head grows");
    assert_ne!(
        prims_fp(&painted),
        prims_fp(&expected(&snap, rect, scale, DeckForm::Full)),
        "NEGATIVE CONTROL: the unmagnified deck"
    );
    assert_eq!(
        prims_fp(&plain),
        prims_fp(&expected(&snap, plain_rect, scale, DeckForm::Full)),
        "NEGATIVE CONTROL: at text scale 1 the head is the window's deck"
    );
}

/// Every lens document remains in the semantic tree while offscreen; the two instruments
/// deliberately excluded from the page have no nodes or actions.
#[test]
fn the_resources_page_keeps_every_documents_words_and_only_its_documents() {
    let snap = Arc::new(crate::stats_deck::tests::snapshot());
    let model = page(&snap);
    for width in [420.0, 1200.0] {
        let view = ResourcesViewState::default();
        let cx = cx(width, 2.0);
        let compiled = model
            .view(&view, &cx)
            .compile(cx.viewport)
            .expect("the page compiles");
        let mut offscreen = 0;
        for instrument in Instrument::page_order() {
            let (_, slug) = instrument.page().expect("a listed document");
            let key = format!("resources/doc/{slug}");
            let node = compiled
                .semantic(&UiKey::new(key.as_str()))
                .expect("every document is semantic");
            assert_eq!(node.label, instrument.word());
            let words = crate::native_ui::semantic_value_words(&node.value);
            assert!(!words.is_empty(), "{slug} keeps its document words");
            if node.offscreen {
                offscreen += 1;
            }
            let copies = compiled
                .semantic(&UiKey::new(format!("{key}/copy")))
                .is_some();
            assert_eq!(copies, instrument.resource().is_some());
        }
        assert!(
            offscreen > 0,
            "the test reads documents beyond its viewport"
        );
        for slug in ["tokens", "network-state"] {
            assert!(
                compiled.semantics.iter().all(|node| !node
                    .key
                    .as_str()
                    .starts_with(&format!("resources/doc/{slug}"))),
                "excluded {slug}"
            );
        }
    }
}

/// Scrolling reuses the measured heights and words. Width and reading changes measure anew.
#[test]
fn resources_document_measurements_survive_a_scroll_but_follow_width_and_reading() {
    let snap = Arc::new(crate::stats_deck::tests::snapshot());
    let mut model = page(&snap);
    let view = ResourcesViewState::default();
    let wide = cx(1200.0, 2.0);
    let _ = model.view(&view, &wide);
    let words = Arc::clone(&view.docs.borrow().docs[0].said);
    view.scroll_by(120.0);
    let _ = model.view(&view, &wide);
    assert!(Arc::ptr_eq(&words, &view.docs.borrow().docs[0].said));
    let narrow = cx(420.0, 2.0);
    let _ = model.view(&view, &narrow);
    assert!(
        !Arc::ptr_eq(&words, &view.docs.borrow().docs[0].said),
        "a new width remeasures"
    );
    let narrow_words = Arc::clone(&view.docs.borrow().docs[0].said);
    let mut later = (*snap).clone();
    later.seq += 1;
    model.publish(Arc::new(later), None);
    let _ = model.view(&view, &narrow);
    assert!(
        !Arc::ptr_eq(&narrow_words, &view.docs.borrow().docs[0].said),
        "a new reading remeasures"
    );
}

#[test]
fn resources_visibility_reports_only_rising_edges() {
    let mut model = ResourcesApp::default();
    assert!(!model.seen(false));
    assert!(model.seen(true));
    assert!(!model.seen(true));
    assert!(!model.seen(false));
    assert!(model.seen(true));
}

/// A pending copy has one typed reply, marks just its button busy and disables further copies.
/// Once its reply arrives the status replaces the busy state, and a duplicate reply is inert.
#[test]
fn resources_copy_waits_for_its_own_reply_and_exposes_its_busy_state() {
    use crate::native_app::{ActionInvocation, AppEffect, AppViewState, NativeApp, NativeRuntime};
    let snap = Arc::new(crate::stats_deck::tests::snapshot());
    let mut runtime = NativeRuntime::new();
    let instance = runtime
        .insert_instance(NativeApp::SystemResources(Box::new(page(&snap))))
        .unwrap();
    let view = crate::tab_model::ViewId::from_stored(711);
    runtime
        .attach_view(
            view,
            instance,
            AppViewState::SystemResources(Box::default()),
        )
        .unwrap();
    let action = |id: &str| {
        AppEvent::Action(ActionInvocation {
            id: ActionId::new(id),
            value: None,
        })
    };
    let copy = runtime
        .dispatch(instance, view, action("resources/doc/cpu/copy-report"))
        .unwrap();
    let operation = copy
        .effects
        .iter()
        .find_map(|effect| match effect {
            AppEffect::Clipboard {
                request:
                    ClipboardRequest::CopyText {
                        text,
                        sensitive: false,
                    },
                reply,
            } => {
                assert_eq!(text, &crate::stats_lens::cpu_diagnostic_report(&snap));
                Some(reply.operation)
            }
            _ => None,
        })
        .expect("a typed native clipboard effect");
    let cx = cx(1200.0, 2.0);
    let compiled = runtime
        .render(instance, view, &cx)
        .unwrap()
        .compile(cx.viewport)
        .unwrap();
    let report = compiled
        .semantic(&UiKey::new("resources/doc/cpu/copy-report"))
        .unwrap();
    assert!(
        report
            .state
            .is_some_and(|state| state.busy && !state.enabled)
    );
    let copy = compiled
        .semantic(&UiKey::new("resources/doc/cpu/copy"))
        .unwrap();
    assert!(
        copy.state
            .is_some_and(|state| !state.busy && !state.enabled)
    );
    assert!(
        runtime
            .dispatch(instance, view, action("resources/doc/memory/copy"))
            .unwrap()
            .effects
            .is_empty()
    );
    let reply = || AppEvent::ClipboardFinished {
        operation,
        outcome: ClipboardOutcome::Copied,
    };
    assert_eq!(
        runtime.dispatch(instance, view, reply()).unwrap().result,
        EventResult::Handled
    );
    assert_eq!(
        runtime.dispatch(instance, view, reply()).unwrap().result,
        EventResult::Bubble
    );
    let compiled = runtime
        .render(instance, view, &cx)
        .unwrap()
        .compile(cx.viewport)
        .unwrap();
    let report = compiled
        .semantic(&UiKey::new("resources/doc/cpu/copy-report"))
        .unwrap();
    assert!(
        report
            .state
            .is_some_and(|state| !state.busy && state.enabled)
    );
    assert_eq!(
        compiled
            .semantic(&UiKey::new("resources/doc/cpu/said"))
            .unwrap()
            .label,
        "CPU report copied"
    );
}

/// The keys of the page's documents, in column order: in the compiled paint (every block laid
/// on screen in a tall viewport) and in the semantics.
fn document_order(snap: Snapshot, width: f32) -> [Vec<String>; 2] {
    use crate::resources_spotlight::tests as spot;
    let mut model = ResourcesApp::default();
    assert!(model.publish(Arc::new(snap), Some((Arc::new(spot::missing_trust()), 1))));
    let cx = ViewCx {
        viewport: LogicalRect::new(0.0, 0.0, width, 100_000.0),
        ..cx(width, 2.0)
    };
    let compiled = model
        .view(&ResourcesViewState::default(), &cx)
        .compile(cx.viewport)
        .expect("the page compiles");
    let documents = |keys: Vec<&str>| -> Vec<String> {
        keys.into_iter()
            .filter(|key| key.starts_with("resources/doc/") && key.matches('/').count() == 2)
            .map(str::to_owned)
            .collect()
    };
    [
        documents(compiled.paint.iter().map(|n| n.key.as_str()).collect()),
        documents(compiled.semantics.iter().map(|n| n.key.as_str()).collect()),
    ]
}

/// Spotlight's document follows the Processes document and comes before How to read, in the
/// page's paint and semantics, at a wide and a narrow page.
/// NEGATIVE CONTROL: a reading of a Mac that does not split by coalition, where no Spotlight
/// figure is read, holds no Spotlight document, and How to read follows Processes.
#[test]
fn the_resources_page_shows_the_spotlight_document_after_processes() {
    use crate::resources_spotlight::tests as spot;
    let at = |keys: &[String], slug: &str| {
        keys.iter()
            .position(|key| *key == format!("resources/doc/{slug}"))
    };
    for width in [1200.0, 420.0] {
        let shown = spot::snapshot(spot::volumes(aterm_vitals::spotlight::Indexing::Enabled));
        for keys in document_order(shown.clone(), width) {
            let processes = at(&keys, "processes").expect("Processes");
            assert_eq!(at(&keys, "spotlight"), Some(processes + 1), "{keys:?}");
            assert_eq!(at(&keys, "how-to-read"), Some(processes + 2), "{keys:?}");
        }
        let mut unsupported = shown;
        unsupported.coal_lane = aterm_vitals::explain::CoalLane::Unsupported;
        for keys in document_order(unsupported, width) {
            let processes = at(&keys, "processes").expect("Processes");
            assert_eq!(at(&keys, "spotlight"), None, "NEGATIVE CONTROL: {keys:?}");
            assert_eq!(at(&keys, "how-to-read"), Some(processes + 1), "{keys:?}");
        }
    }
}

/// `inspect <view> text` reads every row of Spotlight's document, while it is scrolled off
/// screen: the head's state and wanted scope, Now's rows, each process, each volume, the
/// index's words and every What is indexed row.
/// NEGATIVE CONTROL: a reading without the system volume no longer says its row, so the words
/// follow the reading.
#[test]
fn inspect_of_the_resources_page_reads_every_spotlight_row() {
    use crate::resources_spotlight::tests as spot;
    use aterm_vitals::spotlight::Indexing;
    let scope = spot::missing_trust();
    let text = |snap: Snapshot| {
        let mut model = ResourcesApp::default();
        assert!(model.publish(Arc::new(snap), Some((Arc::new(scope.clone()), 1))));
        let cx = cx(1200.0, 2.0);
        let compiled = model
            .view(&ResourcesViewState::default(), &cx)
            .compile(cx.viewport)
            .expect("the page compiles");
        let node = compiled
            .semantic(&UiKey::new("resources/doc/spotlight"))
            .expect("Spotlight's document is semantic");
        assert!(
            node.offscreen,
            "PRECONDITION: read while scrolled off screen"
        );
        crate::app_control::semantic_text_lines(&compiled).join("\n")
    };
    let says = |text: &str, words: &str| {
        let escaped = format!("{words:?}");
        text.contains(escaped.trim_matches('"'))
    };
    let both = spot::volumes(Indexing::Enabled);
    let read = text(spot::snapshot(both));
    let rows = crate::resources_spotlight::what_is_indexed(Some(&scope), Some(&Indexing::Enabled));
    let mut expected = vec![
        "Idle \u{b7} wanted: Applications, Documents, Desktop and synced drives".to_owned(),
        "CPU: 0.03 cores".to_owned(),
        "Disk reads: ".to_owned(),
        "Memory: ".to_owned(),
        "Processes: 11 processes \u{b7} mds_stores holds".to_owned(),
        "mds_stores: ".to_owned(),
        "mds: ".to_owned(),
        "mdworker_shared \u{d7}9: ".to_owned(),
        "of CPU since it began".to_owned(),
        "14:35".to_owned(),
        "/: indexing on".to_owned(),
        "/System/Volumes/Data: indexing on".to_owned(),
        "Not read: only root can read the index's folder".to_owned(),
    ];
    expected.extend(rows.iter().map(crate::resources_spotlight::ScopeRow::said));
    for words in &expected {
        assert!(says(&read, words), "{words:?} in\n{read}");
    }
    let data_only = spot::volumes(Indexing::Enabled)
        .into_iter()
        .filter(|v| v.path != "/")
        .collect();
    let read = text(spot::snapshot(data_only));
    assert!(says(&read, "/System/Volumes/Data: indexing on"));
    assert!(
        !says(&read, "/: indexing on"),
        "NEGATIVE CONTROL: the dropped volume's words are gone\n{read}"
    );
}

/// [`crate::stats_deck::tests::snapshot`] with three more sessions after its `Tab 1` (key 7,
/// claude): `Tab 2` split into a quiet vim (21), one of whose members the OS refused, and a busy
/// cargo (22), and `Tab 3` (23), whose memory, disk and GPU figures were not read. The host's
/// order is not the order of load. GPU clients overlap: each scope's summed client share
/// (cargo's 5%, aterm's 20%, the Mac's 30%) is over what the GPU row draws for it (2%, 3%, the
/// device's 12%).
fn four_sessions() -> Snapshot {
    let mut snap = crate::stats_deck::tests::snapshot();
    let lead = snap.tabs[0].clone();
    let session = |key: u64, tab: u64, label: &str, program: &str, cores: f32| {
        let mut view = lead.clone();
        view.key = key;
        view.tab = tab;
        view.label = label.to_owned();
        view.program = program.to_owned();
        view.scope.cores.level = cores;
        view.scope.procs = 2;
        view.scope.footprint = Some(64 << 20);
        view.scope.footprint_unread = 0;
        view.scope.read_bps = Some(2_000_000.0);
        view.scope.write_bps = Some(500_000.0);
        view.scope.gpu = Some(0.05);
        view
    };
    let mut unread = session(23, 23, "Tab 3", "", 1.0);
    unread.scope.footprint = None;
    unread.scope.read_bps = None;
    unread.scope.write_bps = None;
    unread.scope.gpu = None;
    let mut refused = session(21, 21, "Tab 2", "vim", 0.1);
    refused.scope.refused = 1;
    snap.tabs
        .extend([refused, session(22, 21, "Tab 2", "cargo", 3.5), unread]);
    let gpu = &mut snap.resources[aterm_vitals::model::Resource::Gpu.index()];
    gpu.tabs.push((
        22,
        aterm_vitals::model::ScopeFig {
            now: Some(2.0),
            ..aterm_vitals::model::ScopeFig::default()
        },
    ));
    gpu.aterm.now = Some(3.0);
    gpu.now = Some(12.0);
    if let Some(aterm) = snap.aterm.as_mut() {
        aterm.gpu = Some(0.2);
    }
    snap.global.gpu = Some(0.3);
    snap
}

/// Three readings of [`crate::stats_deck::tests::sample_at`] with `Tab 2` split into a vim (21)
/// and a cargo (22), each under its own shell: the reading holds each pane's own figures, and a
/// scope of the tab sums both.
fn split_tab() -> Snapshot {
    use aterm_vitals::model::{ProcSample, TabRoot};
    let at = |t: u64, busy: &[u64], cpu: &[u64], vim_ms: u64, cargo_ms: u64| {
        let mut sample = crate::stats_deck::tests::sample_at(t, busy, cpu);
        for (key, shell_pid, program) in [(21, 300, "vim"), (22, 400, "cargo")] {
            sample.tabs.push(TabRoot {
                key,
                tab: 21,
                shell_pid,
                label: "Tab 2".into(),
                program: program.into(),
                tab_id: None,
                agent_leader: None,
            });
        }
        let process = |pid: u32, ppid: u32, name: &str, ms: u64, mib: u64| ProcSample {
            start: Some(1),
            pid,
            ppid,
            cpu_ns: Some(ms * 1_000_000),
            footprint: Some(mib << 20),
            name: name.into(),
            disk_written: None,
            uid: None,
            ..ProcSample::default()
        };
        sample.procs.get_or_insert_with(Vec::new).extend([
            process(300, 100, "zsh", 0, 5),
            process(301, 300, "vim", vim_ms, 900),
            process(400, 100, "zsh", 0, 5),
            process(401, 400, "cargo", cargo_ms, 700),
        ]);
        sample
    };
    let mut v = aterm_vitals::Vitals::new();
    let _ = v.absorb(&at(10, &[0; 10], &[0, 0], 0, 0));
    let _ = v.absorb(&at(
        11,
        &[20, 30, 10, 0, 5, 5, 5, 5, 5, 5],
        &[100, 400],
        600,
        900,
    ));
    v.absorb(&at(
        12,
        &[120, 130, 60, 10, 10, 10, 10, 10, 10, 10],
        &[200, 1200],
        1800,
        2500,
    ))
}

/// The Sessions table holds one row for each session of the reading, in the host's order and
/// never re-sorted by load, then aterm's and this Mac's; each row is a button that chooses its
/// scope over its cost (cores-worth, memory, processes, disk read and write, GPU share), and a
/// figure not read says `unread`, never 0. One `inspect <view> text` reads every row, those
/// scrolled off screen included.
/// The GPU share is the GPU row's (what the head and the GPU document say), never the summed
/// client share, and disk rates with refused members are lower bounds.
/// NEGATIVE CONTROLS: a session left out, the rows sorted by load, an unread figure drawn as 0,
/// or the summed client GPU share each break a line here (the session count, the order, the
/// `unread` words, the GPU figures over the row's).
#[test]
fn the_session_table_lists_every_session_of_the_snapshot_in_the_hosts_order_with_its_cost() {
    let snap = Arc::new(four_sessions());
    let order: Vec<u64> = snap.tabs.iter().map(|view| view.key).collect();
    let mut by_load = snap.tabs.clone();
    by_load.sort_by(|a, b| b.scope.cores.level.total_cmp(&a.scope.cores.level));
    assert_ne!(
        by_load.iter().map(|view| view.key).collect::<Vec<_>>(),
        order,
        "PRECONDITION: the host's order is not the order of load"
    );
    let model = page(&snap);
    let cx = cx(1200.0, 2.0);
    let compiled = model
        .view(&ResourcesViewState::default(), &cx)
        .compile(cx.viewport)
        .expect("the page compiles");
    let rows: Vec<&crate::native_ui::SemanticNode> = compiled
        .semantics
        .iter()
        .filter(|node| {
            node.key.as_str().starts_with("resources/sessions/")
                && node.key.as_str().ends_with("/choose")
        })
        .collect();
    let mut expected: Vec<String> = order.iter().map(u64::to_string).collect();
    expected.extend(["aterm".to_owned(), "mac".to_owned()]);
    assert_eq!(
        rows.iter()
            .map(|node| node.key.as_str().split('/').nth(2).unwrap_or_default())
            .collect::<Vec<_>>(),
        expected,
        "one row a session in the host's order, then aterm and this Mac"
    );
    assert!(
        rows.iter().any(|node| node.offscreen),
        "PRECONDITION: the table is read while scrolled off screen"
    );
    let lines = crate::app_control::semantic_text_lines(&compiled).join("\n");
    let cost = |id: &str| {
        compiled
            .semantic(&UiKey::new(format!("resources/sessions/{id}/cost")))
            .map(|node| node.label.clone())
            .expect("each row says its cost")
    };
    let gpu = &snap.resources[aterm_vitals::model::Resource::Gpu.index()];
    for (row, view) in rows.iter().zip(snap.tabs.iter()) {
        assert_eq!(
            row.label,
            session_name(&snap, view),
            "a row is named for its tab and program"
        );
        assert_eq!(
            row.action.as_ref().map(ActionId::as_str),
            Some(format!("resources/scope/session/{}", view.key).as_str()),
            "pressing a row chooses its session"
        );
        let said = cost(&view.key.to_string());
        let row_gpu = snap
            .resource(aterm_vitals::model::Resource::Gpu, &[view.key])
            .tab
            .now;
        assert_eq!(said, cost_words(Some(&view.scope), row_gpu));
        assert!(lines.contains(&said), "one inspect reads {said:?}");
    }
    assert_eq!(
        rows.iter()
            .filter(|row| row.label.starts_with("Tab 2"))
            .map(|row| row.label.as_str())
            .collect::<Vec<_>>(),
        [
            "Tab 2 \u{b7} pane 1 \u{b7} vim",
            "Tab 2 \u{b7} pane 2 \u{b7} cargo"
        ],
        "each pane of a split tab is named by its place, so no two rows read alike"
    );
    assert_eq!(
        rows[0].label, "Tab 1 \u{b7} claude",
        "a lone pane is its tab"
    );
    assert_eq!(
        cost("22"),
        "3.5 cores \u{b7} memory 64 MiB \u{b7} 2 processes \u{b7} disk read 2.0 MB/s \u{b7} \
         disk write 0.5 MB/s \u{b7} GPU 2%",
        "a read row says every figure, its GPU share as the GPU row draws it, not the summed \
         client share (5%)"
    );
    assert!(
        cost("21").contains("disk read at least 2.0 MB/s \u{b7} disk write at least 0.5 MB/s"),
        "disk rates with refused members are lower bounds: {}",
        cost("21")
    );
    let unread = cost("23");
    for figure in ["memory", "disk read", "disk write", "GPU"] {
        assert!(
            unread.contains(&format!("{figure} unread")),
            "an unread {figure} says so, never 0: {unread}"
        );
    }
    assert!(
        !unread.contains("0 B") && !unread.contains("0%"),
        "{unread}"
    );
    assert_eq!(
        cost("aterm"),
        cost_words(snap.aterm.as_ref(), gpu.aterm.now)
    );
    assert!(
        cost("aterm").ends_with("GPU 3%"),
        "aterm's GPU is the row's, not its summed client share (20%): {}",
        cost("aterm")
    );
    assert_eq!(cost("mac"), cost_words(Some(&snap.global), gpu.now));
    assert!(
        cost("mac").ends_with("GPU 12%"),
        "this Mac's GPU is the device's busy share, not its clients' sum (30%): {}",
        cost("mac")
    );
}

/// A runtime holding the page at `snap` and one view of it (711), scoped to this Mac.
fn runtime_at(
    snap: &Arc<Snapshot>,
) -> (
    crate::native_app::NativeRuntime,
    crate::native_app::AppInstanceId,
    crate::tab_model::ViewId,
) {
    use crate::native_app::{AppViewState, NativeApp, NativeRuntime};
    let mut runtime = NativeRuntime::new();
    let instance = runtime
        .insert_instance(NativeApp::SystemResources(Box::new(page(snap))))
        .unwrap();
    let view = crate::tab_model::ViewId::from_stored(711);
    runtime
        .attach_view(
            view,
            instance,
            AppViewState::SystemResources(Box::default()),
        )
        .unwrap();
    (runtime, instance, view)
}

fn act(id: &str) -> AppEvent {
    AppEvent::Action(crate::native_app::ActionInvocation {
        id: ActionId::new(id),
        value: None,
    })
}

/// Pressing a session's row (`act` on its action) scopes the whole page to that session's tab,
/// a split tab's other pane included: the head's Tab lane is the tab's, as a pinned lens on that
/// tab draws and says it, every document's words are those laid at the Tab bar over the tab's
/// sessions, and the scope row (one segmented control) and the row show it chosen. Choosing
/// writes nothing and asks the host for nothing.
/// NEGATIVE CONTROLS: the scope left at This Mac. The page at This Mac draws another head, says
/// other head words and other words of a document, so each equality above holds the scope; and
/// the chosen pane alone says other head words than its whole tab, so the equality holds the
/// tab's other pane.
#[test]
fn choosing_a_session_row_scopes_the_head_and_every_document_to_that_session() {
    let snap = Arc::new(split_tab());
    let session = snap.tab(22).expect("cargo's pane").clone();
    let panes = [22, 21];
    let (mut runtime, instance, view) = runtime_at(&snap);
    let cx = cx(1200.0, 2.0);
    let compile = |runtime: &mut crate::native_app::NativeRuntime| {
        runtime
            .render(instance, view, &cx)
            .unwrap()
            .compile(cx.viewport)
            .unwrap()
    };
    let mac = compile(&mut runtime);
    let chose = runtime
        .dispatch(
            instance,
            view,
            act(&format!("resources/scope/session/{}", session.key)),
        )
        .unwrap();
    assert_eq!(chose.result, EventResult::Handled);
    assert!(
        chose.effects.iter().all(|effect| matches!(
            effect,
            crate::native_app::AppEffect::RepaintSelf(_)
                | crate::native_app::AppEffect::InvalidateOwnPresentation
        )),
        "choosing writes nothing and asks the host for nothing but a repaint"
    );
    let scoped = compile(&mut runtime);

    let head = |compiled: &CompiledUi| {
        let index = compiled
            .paint
            .iter()
            .position(|node| node.key.as_str() == HEAD_KEY)
            .expect("the head is painted");
        let rect = compiled.paint[index].rect;
        let painted =
            compiled.paint_motion_node(index, roles(), aterm_render::Theme::default(), 13.0, 0);
        let words = crate::native_ui::semantic_value_words(
            &compiled.semantic(&UiKey::new(HEAD_KEY)).unwrap().value,
        );
        (rect, prims_fp(&painted), words)
    };
    let (rect, painted, words) = head(&scoped);
    assert_eq!(
        painted,
        prims_fp(&expected_scoped(
            &snap,
            rect,
            2.0,
            1.0,
            DeckForm::Full,
            &panes,
            &session.label
        )),
        "the head's Tab lane is the chosen session's"
    );
    let locale = crate::stats_deck::host_locale(Some(&snap));
    let sentences = |panes: &[u64]| {
        crate::stats_deck::DECK_RESOURCES
            .iter()
            .map(|&res| aterm_vitals::words::row_sentence(res, &snap, panes, &locale))
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert_eq!(
        words,
        crate::native_ui::semantic_value_words(&crate::native_ui::SemanticValue::Text(sentences(
            &panes
        )))
    );
    let (_, mac_painted, mac_words) = head(&mac);
    assert_ne!(mac_painted, painted, "NEGATIVE CONTROL: This Mac's head");
    assert_ne!(mac_words, words, "NEGATIVE CONTROL: This Mac's head words");
    assert_ne!(
        crate::native_ui::semantic_value_words(&crate::native_ui::SemanticValue::Text(sentences(
            &[22]
        ))),
        words,
        "NEGATIVE CONTROL: the chosen pane alone, without its tab's other pane"
    );

    let document = |compiled: &CompiledUi, slug: &str| {
        compiled
            .semantic(&UiKey::new(format!("resources/doc/{slug}")))
            .map(|node| node.value.clone())
            .expect("every document is semantic")
    };
    let laid = |face: DocFace, bar: Bar, panes: &[u64], label: &str| {
        let what = match face {
            DocFace::Lens(instrument) => crate::stats_lens::PageDoc::Lens(instrument),
            DocFace::Spotlight => crate::stats_lens::PageDoc::Spotlight(None),
        };
        document_words(crate::stats_lens::lay_page(
            1152.0,
            &deck_input(&snap, DeckForm::Full, panes, label),
            what,
            bar,
            2.0,
            26.0,
            &crate::deck_face::deck_metrics(),
            false,
        ))
    };
    let mut moved = 0;
    for face in DocFace::documents(&snap) {
        let slug = face.word();
        let said = document(&scoped, slug);
        assert_eq!(
            said,
            crate::native_ui::SemanticValue::Text(laid(face, Bar::Tab, &panes, &session.label)),
            "{slug} says what its document says of the chosen session"
        );
        if document(&mac, slug) != said {
            moved += 1;
        }
        assert_eq!(
            document(&mac, slug),
            crate::native_ui::SemanticValue::Text(laid(face, Bar::Global, &[], "")),
            "{slug} at This Mac"
        );
    }
    assert!(
        moved > 0,
        "NEGATIVE CONTROL: a document's words at This Mac differ from the session's"
    );

    let selected = |compiled: &CompiledUi, key: &str| {
        compiled
            .semantic(&UiKey::new(key))
            .and_then(|node| node.state)
            .is_some_and(|state| state.selected)
    };
    assert!(selected(&scoped, "resources/scope/session"));
    assert!(!selected(&scoped, "resources/scope/mac"));
    assert!(selected(
        &scoped,
        &format!("resources/sessions/{}/choose", session.key)
    ));
    assert!(selected(&mac, "resources/scope/mac"));
    assert!(
        mac.semantic(&UiKey::new("resources/scope/session"))
            .is_none()
    );
    assert!(
        scoped
            .segment_groups
            .iter()
            .any(|group| group.key.as_str() == "resources/scope" && group.segments.len() == 3),
        "the scope row is one segmented control"
    );
}

/// A press that changes the scope lays every document anew at its own height, which moves the
/// Sessions table: the pressed row stays where it stood on screen, under a still pointer, both
/// ways: `act` on This Mac's row at the column's end (in sight, as `act` reveals its control
/// before it presses), and a pointer press back on a session's row (which focuses it).
/// NEGATIVE CONTROL: the two scopes' columns differ in height (PRECONDITION), so a scroll left
/// alone moves the row (the row's place in the column moves with them).
#[test]
fn a_pressed_sessions_row_stays_where_it_stood_when_its_scope_changes_the_documents() {
    use crate::native_app::{AppViewState, NativeRuntime};
    let snap = Arc::new(split_tab());
    let (mut runtime, instance, view) = runtime_at(&snap);
    let cx = cx(1200.0, 2.0);
    let compile = |runtime: &mut NativeRuntime| {
        runtime
            .render(instance, view, &cx)
            .unwrap()
            .compile(cx.viewport)
            .unwrap()
    };
    let state = |runtime: &NativeRuntime| match runtime.view_state(view) {
        Some(AppViewState::SystemResources(state)) => (state.scroll.get(), state.limit.get()),
        _ => panic!("the page's view"),
    };
    let rect = |compiled: &CompiledUi, key: &str| {
        let node = compiled
            .semantic(&UiKey::new(key))
            .unwrap_or_else(|| panic!("{key}"));
        assert!(!node.offscreen, "{key} is in sight");
        node.rect
    };
    let press = |runtime: &mut NativeRuntime, action: &str| {
        let pressed = runtime.dispatch(instance, view, act(action)).unwrap();
        assert_eq!(pressed.result, EventResult::Handled);
    };
    let _ = compile(&mut runtime);
    press(&mut runtime, "resources/scope/session/22");
    let _ = compile(&mut runtime);
    assert_eq!(
        runtime
            .dispatch(instance, view, AppEvent::ScrollLines(100_000))
            .unwrap()
            .result,
        EventResult::Handled
    );

    // In sight only: `act` on This Mac's row at the end of the column.
    let at_tab = compile(&mut runtime);
    let (scroll, tab_limit) = state(&runtime);
    assert!(
        (scroll - tab_limit).abs() < 0.5,
        "PRECONDITION: at the column's end"
    );
    assert!(
        at_tab
            .semantic(&UiKey::new("resources/scope/mac"))
            .is_some_and(|node| node.offscreen),
        "PRECONDITION: only the table's row chooses This Mac in sight"
    );
    let before = rect(&at_tab, "resources/sessions/mac/choose");
    press(&mut runtime, "resources/scope/mac");
    let at_mac = compile(&mut runtime);
    let (_, mac_limit) = state(&runtime);
    assert!(
        (mac_limit - tab_limit).abs() > 1.0,
        "PRECONDITION: the scope changes the documents' heights ({tab_limit} -> {mac_limit})"
    );
    let after = rect(&at_mac, "resources/sessions/mac/choose");
    assert!(
        (after.y - before.y).abs() < 0.5,
        "the pressed row stays where it stood: {before:?} -> {after:?}"
    );

    // Back to the session, the documents' heights changing the other way: a pointer press on
    // cargo's row focuses it before its action.
    let before = rect(&at_mac, "resources/sessions/22/choose");
    runtime
        .view_state_mut(view)
        .expect("the page's view")
        .common_mut()
        .last_focus = Some(UiKey::new("resources/sessions/22/choose"));
    press(&mut runtime, "resources/scope/session/22");
    let again = compile(&mut runtime);
    let after = rect(&again, "resources/sessions/22/choose");
    assert!(
        (after.y - before.y).abs() < 0.5,
        "the focused row stays where it stood: {before:?} -> {after:?}"
    );
}

/// A chosen session holds the page's scope through each reading that still holds it (and
/// through one that read no sessions: a machine-only one, or one from before two sweeps, as a
/// page opened over a tab just after launch reads first); the first reading without it returns
/// the scope to this Mac.
/// NEGATIVE CONTROL: the readings that still hold the session, or read none, keep its scope, so
/// the return follows the session's end and not any new reading.
#[test]
fn a_chosen_session_that_ends_returns_the_scope_to_this_mac() {
    let snap = Arc::new(crate::stats_deck::tests::snapshot());
    let key = snap.tabs[0].key;
    let (mut runtime, instance, view) = runtime_at(&snap);
    let cx = cx(1200.0, 2.0);
    let mut seq = snap.seq;
    let mut publish = |runtime: &mut crate::native_app::NativeRuntime,
                       edit: &dyn Fn(&mut Snapshot)| {
        let mut next = (*snap).clone();
        seq += 1;
        next.seq = seq;
        edit(&mut next);
        assert!(runtime.publish_resources(instance, Arc::new(next), None));
    };
    let chosen = |runtime: &mut crate::native_app::NativeRuntime| {
        let compiled = runtime
            .render(instance, view, &cx)
            .unwrap()
            .compile(cx.viewport)
            .unwrap();
        let on = |key: &str| {
            compiled
                .semantic(&UiKey::new(key))
                .and_then(|node| node.state)
                .is_some_and(|state| state.selected)
        };
        match (on("resources/scope/mac"), on("resources/scope/session")) {
            (true, false) => "mac",
            (false, true) => "session",
            other => panic!("one scope is chosen: {other:?}"),
        }
    };
    runtime
        .dispatch(
            instance,
            view,
            act(&format!("resources/scope/session/{key}")),
        )
        .unwrap();
    assert_eq!(chosen(&mut runtime), "session");
    publish(&mut runtime, &|_| {});
    assert_eq!(
        chosen(&mut runtime),
        "session",
        "NEGATIVE CONTROL: a reading that holds it keeps it"
    );
    publish(&mut runtime, &|next| {
        next.machine_only = true;
        next.tabs.clear();
    });
    assert_eq!(
        chosen(&mut runtime),
        "session",
        "NEGATIVE CONTROL: a reading of no sessions says nothing of it"
    );
    publish(&mut runtime, &|next| {
        next.aterm = None;
        next.tabs.clear();
    });
    assert_eq!(
        chosen(&mut runtime),
        "session",
        "NEGATIVE CONTROL: a reading from before two sweeps (no aterm, no sessions, not \
         machine-only) says nothing of it"
    );
    publish(&mut runtime, &|next| next.tabs.clear());
    assert_eq!(
        chosen(&mut runtime),
        "mac",
        "an ended session returns the page to this Mac"
    );
}

/// [`crate::stats_deck::tests::snapshot`] with three weeks of free space behind it, one reading
/// an hour (100 GB used three weeks ago, 0.15 GB more each hour), so the Disk document's
/// used-space chart offers both its spans.
fn three_weeks() -> Snapshot {
    const HOUR_MS: i64 = 3_600_000;
    let mut snap = crate::stats_deck::tests::snapshot();
    let total = snap.volume.as_ref().map_or(0, |volume| volume.total);
    let now = snap.at_unix_ms.div_euclid(HOUR_MS);
    let first = now + 1 - aterm_vitals::history::WEEK_BUCKETS as i64;
    for hour in first..=now {
        let used = 100_000_000_000 + 150_000_000 * u64::try_from(hour - first).unwrap_or(0);
        snap.free_weeks.note(
            aterm_vitals::ring::Taken::at(hour * HOUR_MS + 1, 1_000),
            total.saturating_sub(used),
        );
    }
    snap
}

/// The Disk document of `snap` at This Mac with `span` drawn, laid as the page lays it at 1200
/// points wide and 2x.
fn disk_document(snap: &Snapshot, span: DiskSpan, draw: bool) -> crate::stats_lens::PageLaid {
    disk_document_at(snap, span, 1152.0, draw)
}

/// [`disk_document`] laid `width_pt` wide.
fn disk_document_at(
    snap: &Snapshot,
    span: DiskSpan,
    width_pt: f64,
    draw: bool,
) -> crate::stats_lens::PageLaid {
    crate::stats_lens::lay_page(
        width_pt,
        &DeckInput {
            disk_span: span,
            ..deck_input(snap, DeckForm::Full, &[], "")
        },
        crate::stats_lens::PageDoc::Lens(Instrument::Disk),
        Bar::Global,
        2.0,
        26.0,
        &crate::deck_face::deck_metrics(),
        draw,
    )
}

/// What the compiled page shows of the Disk document: its words, its prims' fingerprint and
/// the prims, and the span button's value.
fn disk_shown(compiled: &CompiledUi) -> (SemanticValue, u64, Vec<DrawPrim>, SemanticValue) {
    let key = UiKey::new("resources/doc/disk");
    let index = compiled
        .paint
        .iter()
        .position(|node| node.key == key)
        .expect("the Disk document is painted");
    let painted =
        compiled.paint_motion_node(index, roles(), aterm_render::Theme::default(), 13.0, 0);
    let words = compiled
        .semantic(&key)
        .expect("Disk is semantic")
        .value
        .clone();
    let value = compiled
        .semantic(&UiKey::new(SPAN_KEY))
        .expect("the span button")
        .value
        .clone();
    (words, prims_fp(&painted), painted, value)
}

/// The Disk document's prims laid by hand at `rect`, `span` drawn: the page's paint brought to
/// points and to the node.
fn disk_expected(snap: &Snapshot, span: DiskSpan, rect: LogicalRect) -> u64 {
    let mut prims = disk_document(snap, span, true).prims;
    crate::widget::scale_prims(&mut prims, 0.5);
    crate::widget::translate_prims(&mut prims, rect.x, rect.y);
    prims_fp(&prims)
}

/// A press on the Disk document's span labels (`24 h \u{b7} 3 weeks`) switches its used-space
/// chart: the page lays one span button over the labels, so a pointer at the painted `3 weeks`
/// hits it, and its press draws and says the three-week chart, as a card with that span lays
/// it, and lists the button at `3 weeks`; `act` on it returns the day. Switching writes nothing
/// and asks the host for nothing but a repaint.
/// NEGATIVE CONTROL: a page whose Ctx keeps DiskSpan::Day: the day's document differs from the
/// three weeks' in its words and its prims, so each equality holds the span the view chose.
#[test]
fn a_press_on_the_pages_span_chips_switches_the_disk_chart_between_a_day_and_three_weeks() {
    let snap = Arc::new(three_weeks());
    let (mut runtime, instance, view) = runtime_at(&snap);
    let cx = cx(1200.0, 2.0);
    let compile = |runtime: &mut crate::native_app::NativeRuntime| {
        runtime
            .render(instance, view, &cx)
            .unwrap()
            .compile(cx.viewport)
            .unwrap()
    };
    let words =
        |span: DiskSpan| SemanticValue::Text(document_words(disk_document(&snap, span, false)));
    let day_words = words(DiskSpan::Day);
    let weeks_words = words(DiskSpan::Weeks);
    assert_ne!(day_words, weeks_words, "NEGATIVE CONTROL: the day's words");
    let SemanticValue::Text(said) = &weeks_words else {
        unreachable!()
    };
    assert!(said.contains("GB used, last 3 weeks"), "{said}");

    let _ = compile(&mut runtime);
    runtime
        .dispatch(
            instance,
            view,
            AppEvent::RevealControl(UiKey::new("resources/doc/disk")),
        )
        .unwrap();
    let at_day = compile(&mut runtime);
    let rect = at_day
        .semantic(&UiKey::new("resources/doc/disk"))
        .expect("Disk")
        .rect;
    assert!(
        (f64::from(rect.height) - disk_document(&snap, DiskSpan::Day, false).height_pt).abs() < 0.5,
        "PRECONDITION: the whole document in sight: {rect:?}"
    );
    let (said, painted, prims, value) = disk_shown(&at_day);
    assert_eq!(said, day_words, "the view opens on the day");
    assert_eq!(painted, disk_expected(&snap, DiskSpan::Day, rect));
    assert_eq!(value, SemanticValue::Text("24 h".into()));

    // The pointer on the painted `3 weeks` presses the span button.
    let (x, baseline, px) = prims
        .iter()
        .find_map(|prim| match prim {
            DrawPrim::Text {
                x, baseline, s, px, ..
            } if s == "3 weeks" => Some((*x, *baseline, *px)),
            _ => None,
        })
        .expect("the Disk document draws its span labels");
    let hit = at_day
        .hit_test(x + px, baseline - px * 0.35)
        .expect("a control lies over the labels");
    assert_eq!(hit.key.as_str(), SPAN_KEY);
    // The ring a focused button strokes 2 pt deep just inside its rect clears the labels' ink and
    // the chosen label's rule.
    let button = at_day
        .semantic(&UiKey::new(SPAN_KEY))
        .expect("the span button")
        .rect;
    let (day_x, day_base) = prims
        .iter()
        .find_map(|prim| match prim {
            DrawPrim::Text { x, baseline, s, .. } if s == "24 h" => Some((*x, *baseline)),
            _ => None,
        })
        .expect("the day's label");
    let rule_bottom = prims
        .iter()
        .find_map(|prim| match prim {
            DrawPrim::Panel { x, y, h, .. }
                if *x == day_x && *y > day_base && *y < day_base + 4.0 =>
            {
                Some(*y + *h)
            }
            _ => None,
        })
        .expect("the chosen label's rule");
    assert!(
        button.x + 3.0 <= day_x && rule_bottom + 0.5 <= button.y + button.height - 3.0,
        "the ring clears the first label and the rule: {button:?}, label at {day_x}, rule to {rule_bottom}"
    );
    assert!(
        at_day
            .fit_failures()
            .iter()
            .all(|failure| !failure.contains(SPAN_KEY)),
        "the button paints no words to overflow: {:?}",
        at_day.fit_failures()
    );
    let pressed = runtime
        .dispatch(instance, view, act(hit.action.as_str()))
        .unwrap();
    assert_eq!(pressed.result, EventResult::Handled);
    assert!(
        pressed.effects.iter().all(|effect| matches!(
            effect,
            crate::native_app::AppEffect::RepaintSelf(_)
                | crate::native_app::AppEffect::InvalidateOwnPresentation
        )),
        "switching writes nothing and asks the host for nothing but a repaint"
    );
    let at_weeks = compile(&mut runtime);
    let (said, weeks_painted, _, value) = disk_shown(&at_weeks);
    let rect = at_weeks
        .semantic(&UiKey::new("resources/doc/disk"))
        .expect("Disk")
        .rect;
    assert_eq!(said, weeks_words, "the press draws three weeks");
    assert_eq!(weeks_painted, disk_expected(&snap, DiskSpan::Weeks, rect));
    assert_ne!(
        weeks_painted,
        disk_expected(&snap, DiskSpan::Day, rect),
        "NEGATIVE CONTROL: the day's prims"
    );
    assert_eq!(value, SemanticValue::Text("3 weeks".into()));

    // `act` returns the day.
    let acted = runtime.dispatch(instance, view, act(SPAN_KEY)).unwrap();
    assert_eq!(acted.result, EventResult::Handled);
    let again = compile(&mut runtime);
    let rect = again
        .semantic(&UiKey::new("resources/doc/disk"))
        .expect("Disk")
        .rect;
    let (said, painted, _, value) = disk_shown(&again);
    assert_eq!(said, day_words, "act returns the day");
    assert_eq!(painted, disk_expected(&snap, DiskSpan::Day, rect));
    assert_eq!(value, SemanticValue::Text("24 h".into()));
}

/// A page width, pt, too narrow for the span labels beside the Disk chart's heading and its growth
/// words, which the heading gives up for them.
const NARROW_PX: f32 = 340.0;

/// An ordinary page width, pt, of two columns, where a reading's growth words beside the Disk
/// chart's heading leave no room for the span labels unless the heading gives them up.
const TWO_COLUMN_PX: f32 = 714.0;

/// The span is the view's: of two views of the page in two windows, switching one leaves the
/// other on the day, and the switched one keeps three weeks through a new reading. The labels are
/// the page's one way to switch, so on either span the heading gives up its growth words for
/// them: a narrow page lays the button on the day and on three weeks, and at an ordinary width
/// the button a view laid stays where it was when a reading brings growth words that would have
/// crowded the labels out. Where the three-week series holds nothing older than a day the chart
/// offers one span: the labels are not drawn, no button lies, and `act` on its action is not the
/// page's.
/// NEGATIVE CONTROL: the same page with three weeks behind it lays the button in both views
/// (PRECONDITION), so its absence follows the empty series; and without the labels the heading
/// draws growth words the labels give up (PRECONDITION), so the card's rule for the day, which
/// keeps those words and drops the labels, lays no button there.
#[test]
fn the_pages_disk_span_is_kept_by_the_view_and_no_button_is_laid_where_no_span_is_offered() {
    use crate::native_app::AppViewState;
    let snap = Arc::new(three_weeks());
    let (mut runtime, instance, first) = runtime_at(&snap);
    let second = crate::tab_model::ViewId::from_stored(712);
    runtime
        .attach_view(
            second,
            instance,
            AppViewState::SystemResources(Box::default()),
        )
        .unwrap();
    let narrow = cx(NARROW_PX, 2.0);
    let cx = cx(1200.0, 2.0);
    let value = |runtime: &mut crate::native_app::NativeRuntime, instance, view| {
        runtime
            .render(instance, view, &cx)
            .unwrap()
            .compile(cx.viewport)
            .unwrap()
            .semantic(&UiKey::new(SPAN_KEY))
            .map(|node| node.value.clone())
    };
    let day = Some(SemanticValue::Text("24 h".into()));
    let weeks = Some(SemanticValue::Text("3 weeks".into()));
    assert_eq!(
        value(&mut runtime, instance, first),
        day,
        "PRECONDITION: a button"
    );
    assert_eq!(
        value(&mut runtime, instance, second),
        day,
        "PRECONDITION: a button"
    );
    let switched = runtime.dispatch(instance, first, act(SPAN_KEY)).unwrap();
    assert_eq!(switched.result, EventResult::Handled);
    assert_eq!(value(&mut runtime, instance, first), weeks);
    assert_eq!(
        value(&mut runtime, instance, second),
        day,
        "the other window's view"
    );
    let mut later = (*snap).clone();
    later.seq += 1;
    assert!(runtime.publish_resources(instance, Arc::new(later), None));
    assert_eq!(
        value(&mut runtime, instance, first),
        weeks,
        "kept through a reading"
    );
    assert_eq!(value(&mut runtime, instance, second), day);

    // On a page too narrow for the labels beside the heading and its growth words both views lay
    // the button: the labels shed those words, so the day's view can choose three weeks and the
    // three weeks' can return.
    let narrow_value = |runtime: &mut crate::native_app::NativeRuntime, instance, view| {
        runtime
            .render(instance, view, &narrow)
            .unwrap()
            .compile(narrow.viewport)
            .unwrap()
            .semantic(&UiKey::new(SPAN_KEY))
            .map(|node| node.value.clone())
    };
    assert_eq!(
        narrow_value(&mut runtime, instance, second),
        day,
        "the narrow page's way to three weeks"
    );
    assert_eq!(
        narrow_value(&mut runtime, instance, first),
        weeks,
        "the narrow page's way back"
    );
    let back = runtime.dispatch(instance, first, act(SPAN_KEY)).unwrap();
    assert_eq!(back.result, EventResult::Handled);
    assert_eq!(narrow_value(&mut runtime, instance, first), day);
    assert_eq!(value(&mut runtime, instance, first), day);

    // At an ordinary two-column width a reading's growth words would crowd the day's labels out;
    // the heading gives them up instead, so the button the view laid before the reading stays.
    let ordinary = self::cx(TWO_COLUMN_PX, 2.0);
    let button = |runtime: &mut crate::native_app::NativeRuntime, instance, view| {
        runtime
            .render(instance, view, &ordinary)
            .unwrap()
            .compile(ordinary.viewport)
            .unwrap()
            .semantic(&UiKey::new(SPAN_KEY))
            .map(|node| (node.rect, node.value.clone()))
    };
    let before = button(&mut runtime, instance, second);
    assert_eq!(
        before.as_ref().map(|(_, value)| value.clone()),
        day,
        "the day's button at an ordinary width"
    );
    let rate10 = Some(750e6 / 600.0);
    let mut growing = (*snap).clone();
    growing.seq += 2;
    growing.facts.disk_rates.rate10 = rate10;
    // The page lays a document `TWO_COLUMN_PX` less its margins wide, as at 1200 pt.
    let width_pt = f64::from(TWO_COLUMN_PX) - (f64::from(cx.viewport.width) - 1152.0);
    let mut lone = crate::stats_deck::tests::snapshot();
    lone.facts.disk_rates.rate10 = rate10;
    // The growth words and the span label on the chart heading's line (the other column's
    // heading may share it).
    let beside_name = |laid: &crate::stats_lens::PageLaid| -> Vec<String> {
        let base = laid
            .prims
            .iter()
            .find_map(|prim| match prim {
                DrawPrim::Text { baseline, s, .. } if s == "GB used, last 24 hours" => {
                    Some(*baseline)
                }
                _ => None,
            })
            .expect("the chart's heading");
        laid.prims
            .iter()
            .filter_map(|prim| match prim {
                DrawPrim::Text { baseline, s, .. }
                    if *baseline == base && (s.contains("10 min") || s == "3 weeks") =>
                {
                    Some(s.clone())
                }
                _ => None,
            })
            .collect()
    };
    let without = beside_name(&disk_document_at(&lone, DiskSpan::Day, width_pt, true));
    let with = beside_name(&disk_document_at(&growing, DiskSpan::Day, width_pt, true));
    assert!(
        !without.is_empty() && !without.iter().any(|s| with.contains(s)),
        "PRECONDITION: the labels cost the heading its growth words: {without:?} -> {with:?}"
    );
    assert!(with.iter().any(|s| s == "3 weeks"), "{with:?}");
    assert!(runtime.publish_resources(instance, Arc::new(growing), None));
    assert_eq!(
        button(&mut runtime, instance, second),
        before,
        "the button stays through growth words"
    );

    // No reading older than the last day: one span, no labels, no button.
    let empty = Arc::new(crate::stats_deck::tests::snapshot());
    let laid = disk_document(&empty, DiskSpan::Day, true);
    assert_eq!(laid.span_chips, None);
    assert!(
        !laid
            .prims
            .iter()
            .any(|prim| matches!(prim, DrawPrim::Text { s, .. } if s == "3 weeks")),
        "the labels are not drawn"
    );
    let (mut runtime, instance, view) = runtime_at(&empty);
    let compiled = runtime
        .render(instance, view, &cx)
        .unwrap()
        .compile(cx.viewport)
        .unwrap();
    assert!(
        compiled
            .semantic(&UiKey::new("resources/doc/disk"))
            .is_some(),
        "PRECONDITION: the Disk document is on the page"
    );
    assert!(compiled.semantic(&UiKey::new(SPAN_KEY)).is_none());
    let acted = runtime.dispatch(instance, view, act(SPAN_KEY)).unwrap();
    assert_eq!(acted.result, EventResult::Bubble, "no button, no switch");
    assert_eq!(value(&mut runtime, instance, view), None);
}

/// The page's view of the macOS configuration section, given the host's `page` and the reading
/// `snap` with the scope `scope`: `inspect text` and `inspect controls` as a driver reads them.
fn macos_section(
    page: crate::resources_macos::MacosPage,
    snap: Snapshot,
    scope: crate::resources_spotlight::ScopeReading,
) -> (Vec<String>, Vec<String>) {
    const WID: crate::WindowId = crate::WindowId(0);
    let mut app = crate::App::headless_for_test();
    app.open_resources_tab().expect("the page opens");
    let (instance, _) = app.active_native_view(WID).expect("the page is in front");
    app.native_runtime
        .publish_resources(instance, Arc::new(snap), Some((Arc::new(scope), 1)));
    app.native_runtime.resources_macos(instance, page);
    let compiled = app.compiled_native_ui(WID).expect("the page compiles");
    (
        crate::app_control::semantic_text_lines(&compiled),
        compiled.controls_lines(),
    )
}

/// THE MACOS CONFIGURATION SECTION SHOWS NOW AGAINST WANTED FOR EVERY ROW: on a fixture ALab Mac
/// that differs from Wanted in one item a row (system sleep 10 minutes, checking for updates
/// off, fseventsd past its bar, the build cache backed up and its output found indexed, a folder
/// the scope keeps out missing from setup's receipt), each of the six rows says its Now, its
/// Wanted and that it differs, and every row but the build folders' Spotlight one (no mark keeps
/// a folder out of Spotlight without root) offers its Apply….
/// CONTROLS: the same Mac at Wanted offers no Apply and differs nowhere; with the ALab bit off,
/// Power and Software Update differ in nothing and offer nothing; a build folder under
/// ~/Documents reads "not read here" and offers no Apply.
#[test]
fn the_macos_configuration_section_shows_now_against_wanted_for_every_row() {
    use crate::resources_macos::tests::{
        PAST, UNDER, at_wanted, differing, reading, scope, snapshot,
    };
    use crate::resources_macos::{MacosPage, SLUGS, Tm};
    let page = |reading: crate::resources_macos::MacosReading, alab: bool| MacosPage {
        reading: Some((Arc::new(reading), 1)),
        alab,
        pending: None,
        said: None,
    };
    let row_line = |text: &[String], slug: &str| -> String {
        let key = format!("{MACOS_KEY}/{slug}");
        text.iter()
            .find(|line| line.contains(&format!("key={key:?}")))
            .unwrap_or_else(|| panic!("no row {slug}: {text:#?}"))
            .clone()
    };
    let applies = |controls: &[String]| -> Vec<String> {
        SLUGS
            .iter()
            .filter(|slug| {
                let action = format!("{MACOS_APPLY_PREFIX}{slug}");
                controls.iter().any(|line| line.contains(&action))
            })
            .map(|slug| (*slug).to_owned())
            .collect()
    };

    let (text, controls) = macos_section(page(differing(), true), snapshot(PAST), scope(true));
    assert!(
        text.iter().any(|line| line.contains("macOS configuration")),
        "{text:#?}"
    );
    for (slug, now, wanted) in [
        (
            "spotlight-scope",
            "now everything else on the data volume indexed",
            "kept out",
        ),
        ("build-spotlight", "now indexed", "wanted kept out"),
        (
            "build-time-machine",
            "now 1 of 1 in",
            "wanted out of Time Machine",
        ),
        (
            "file-events",
            "now holds 20.0 GiB",
            "wanted under the 8.0 GiB bar",
        ),
        ("power", "system sleep 10 minutes", "system sleep never"),
        (
            "software-update",
            "check for updates off",
            "check for updates on",
        ),
    ] {
        let line = row_line(&text, slug);
        assert!(
            line.contains(now) && line.contains(wanted) && line.contains("differs from wanted"),
            "{slug}: {line}"
        );
    }
    assert_eq!(
        applies(&controls),
        [
            "spotlight-scope",
            "build-time-machine",
            "file-events",
            "power",
            "software-update"
        ],
        "{controls:#?}"
    );

    let (text, controls) = macos_section(page(at_wanted(), true), snapshot(UNDER), scope(false));
    assert!(applies(&controls).is_empty(), "CONTROL: {controls:#?}");
    for slug in SLUGS {
        let line = row_line(&text, slug);
        assert!(!line.contains("differs"), "CONTROL: {slug}: {line}");
    }

    let (text, controls) = macos_section(page(differing(), false), snapshot(PAST), scope(true));
    for slug in ["power", "software-update"] {
        let line = row_line(&text, slug);
        assert!(
            !line.contains("differs") && line.contains("not an ALab Mac"),
            "CONTROL: {slug}: {line}"
        );
        assert!(
            !applies(&controls).contains(&slug.to_owned()),
            "CONTROL: {slug}"
        );
    }

    let documents = std::path::PathBuf::from("/Users//t/Documents/aterm/target");
    let guarded = reading(
        crate::resources_macos::tests::AT_BASELINE,
        true,
        Tm::Out,
        vec![(documents, Tm::Guarded("~/Documents".into()))],
    );
    let (text, controls) = macos_section(page(guarded, true), snapshot(UNDER), scope(false));
    let line = row_line(&text, "build-time-machine");
    assert!(
        line.contains("not read here: macOS guards ~/Documents") && !line.contains("differs"),
        "CONTROL: {line}"
    );
    assert!(applies(&controls).is_empty(), "CONTROL: {controls:#?}");
}
