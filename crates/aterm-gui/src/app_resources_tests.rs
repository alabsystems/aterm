// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::sync::Arc;

use crate::app_control::{OpenRequest, parse_open, semantic_text_lines};
use crate::native_app::AppKind;
use crate::native_resources::HEAD_KEY;
use crate::stats_paint::HitKind;
use crate::{App, WindowId};

fn tabs(app: &App, wid: WindowId) -> usize {
    app.windows[&wid].tab_set.len()
}

fn front(app: &App, wid: WindowId) -> (crate::tab_model::TabId, crate::tab_model::ViewId) {
    let tab = app.windows[&wid].tab_set.active().expect("a tab in front");
    (tab.id, tab.focus)
}

fn index_of(app: &App, wid: WindowId, tab: crate::tab_model::TabId) -> usize {
    app.windows[&wid]
        .tab_set
        .tabs()
        .iter()
        .position(|t| t.id == tab)
        .expect("the tab")
}

fn open(app: &mut App) {
    let request = parse_open("app system-resources").expect("the request parses");
    assert_eq!(request, OpenRequest::SystemResources { section: None });
    assert_eq!(app.open_app(request).as_deref(), Ok("app system-resources"));
}

/// `open app system-resources` opens the page as a tab of the front window, focused, and listed
/// by `inspect tabs` as kind `system-resources`; a second open, and the menu's, focus that same
/// view and add no tab. With the other window in front, an open puts a view of the one page
/// there and leaves the first window alone.
/// NEGATIVE CONTROL: once the page's tab is closed an open makes a new tab, so the focusing
/// opens above are not an open that never adds one.
#[test]
fn the_resources_page_opens_once_in_the_front_window_and_a_second_open_focuses_it() {
    let crate::stats_band::tests::TwoWindows {
        mut app,
        first,
        second,
        attached: _attached,
    } = crate::stats_band::tests::two_windows();
    let (first_tabs, second_tabs) = (tabs(&app, first), tabs(&app, second));
    let (terminal_tab, _) = front(&app, first);

    open(&mut app);
    assert_eq!(
        tabs(&app, first),
        first_tabs + 1,
        "the first open adds a tab"
    );
    let (page_tab, page_view) = front(&app, first);
    let (instance, view) = app.active_native_view(first).expect("the page is in front");
    assert_eq!(view, page_view, "the page's view is the focused leaf");
    assert_eq!(
        app.native_runtime.app(instance).map(|app| app.kind()),
        Some(AppKind::SystemResources)
    );
    let listed = app.inspect_tabs_body().join("\n");
    assert!(listed.contains("app=system-resources"), "{listed}");

    let back = index_of(&app, first, terminal_tab);
    app.switch_tab_in(first, back);
    assert_eq!(front(&app, first).0, terminal_tab, "PRECONDITION");
    open(&mut app);
    assert_eq!(
        front(&app, first),
        (page_tab, page_view),
        "a second open focuses it"
    );
    app.switch_tab_in(first, back);
    app.open_resources_from_menu();
    assert_eq!(app.pending_action_refusal, None);
    assert_eq!(
        front(&app, first),
        (page_tab, page_view),
        "so does the menu's"
    );
    assert_eq!(tabs(&app, first), first_tabs + 1, "and neither adds a tab");

    app.frontmost_window = Some(second);
    open(&mut app);
    assert_eq!(tabs(&app, second), second_tabs + 1);
    let (other_instance, other_view) = app.active_native_view(second).expect("in front there");
    assert_eq!(other_instance, instance, "one page a process");
    assert_ne!(other_view, page_view, "a view a window");
    assert_eq!(
        app.native_runtime
            .instance_by_kind(AppKind::SystemResources),
        Some(instance)
    );
    assert_eq!(
        tabs(&app, first),
        first_tabs + 1,
        "the first window is untouched"
    );
    assert_eq!(front(&app, first), (page_tab, page_view));

    app.frontmost_window = Some(first);
    let at = index_of(&app, first, page_tab);
    app.close_tab_at(first, at);
    assert_eq!(
        tabs(&app, first),
        first_tabs,
        "PRECONDITION: the page's tab closed"
    );
    open(&mut app);
    assert_eq!(tabs(&app, first), first_tabs + 1, "NEGATIVE CONTROL");
    assert_ne!(
        front(&app, first).1,
        page_view,
        "NEGATIVE CONTROL: a new view"
    );
    assert!(app.structural_invariants_ok());
}

/// Opening the page, going back to a terminal tab, focusing the page again and closing it book
/// no `resizes` row in any session of the window: the page's tab takes the same rows as a
/// terminal tab's.
/// NEGATIVE CONTROL: a chrome change under the same reader, the deck switched to its Full size
/// in the config, books one `site=deck` row in each session, so the reader reads them.
#[test]
fn opening_the_resources_page_resizes_no_session() {
    use crate::stats_deck::tests::{new_sites, sites_of};
    use crate::stats_plan::DeckSizePref;
    let _attached = crate::stats_deck::tests::attach_stats();
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let sessions = crate::stats_band::tests::three_panes_in(&mut app, wid);
    let size = aterm_window::dpi::PhysicalSize::new(1600, 1400);
    app.windows.get_mut(&wid).expect("the window").win_px = Some(size);
    app.on_resize(wid, size);
    app.switch_tab_in(wid, 0);
    let before = sites_of(&app, &sessions);

    open(&mut app);
    let (page_tab, _) = front(&app, wid);
    app.switch_tab_in(wid, 0);
    open(&mut app);
    let at = index_of(&app, wid, page_tab);
    app.close_tab_at(wid, at);
    assert!(
        app.windows[&wid]
            .tab_set
            .tabs()
            .iter()
            .all(|tab| tab.id != page_tab),
        "PRECONDITION: the page's tab closed"
    );
    let after = sites_of(&app, &sessions);
    assert!(
        new_sites(&before, &after).iter().all(Vec::is_empty),
        "the page re-grids no session: {:?}",
        new_sites(&before, &after)
    );

    // A headless window's head does not follow its deck's form, so the control moves it to the
    // Full deck's as `refresh_all_window_metrics` moves an attached window's during the reload.
    let (_, cell) = app.win_cell_size(wid);
    {
        let mut ws = app.windows.get_mut(&wid).expect("the window");
        ws.deck_form = crate::stats_plan::deck_form_for(DeckSizePref::Full);
        let head = crate::stats_deck::chrome_head_px(
            0.0,
            ws.scale,
            crate::stats_deck::StatsBars::Deck,
            ws.deck_form,
            cell,
        );
        ws.metrics.head = head;
        app.backend.set_head(head);
    }
    let scratch = aterm_tempfile::tempdir().expect("scratch dir");
    crate::app_colorscheme::install_resolved_colors::apply_config_text(
        &mut app,
        scratch.path(),
        &format!(
            "[sys]\nstats_deck_size = \"{}\"\n",
            DeckSizePref::Full.word()
        ),
    );
    assert_eq!(app.deck_size_pref(), DeckSizePref::Full, "PRECONDITION");
    let booked = new_sites(&after, &sites_of(&app, &sessions));
    assert!(
        booked.iter().all(|sites| sites == &["site=deck"]),
        "NEGATIVE CONTROL: the deck's size switch books one deck row each: {booked:?}"
    );
}

/// `inspect <view> text` of the page says the sentence of every row its head draws: each row the
/// deck's paint names at the head's width, and the CPU's for the cores.
/// NEGATIVE CONTROL: the sentences of the reading before it are gone, so the words follow the
/// reading the page was handed.
#[test]
fn inspect_of_the_resources_page_says_every_row_its_head_draws() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    // A first reading that has read nothing yet: every row says so.
    let older = aterm_vitals::Snapshot {
        seq: 1,
        ..aterm_vitals::Snapshot::default()
    };
    let snap = crate::stats_deck::tests::snapshot();
    app.vitals.snapshot = Some(Arc::new(older.clone()));
    open(&mut app);
    let text = |app: &App| {
        semantic_text_lines(&app.compiled_native_ui(wid).expect("the page compiles")).join("\n")
    };
    let says = |text: &str, sentence: &str| {
        let escaped = format!("{sentence:?}");
        text.contains(escaped.trim_matches('"'))
    };
    let sentences = |snap: &aterm_vitals::Snapshot| {
        let locale = crate::stats_deck::host_locale(Some(snap));
        crate::stats_deck::DECK_RESOURCES
            .map(|res| aterm_vitals::words::row_sentence(res, snap, &[], &locale))
    };
    let then = sentences(&older);
    let first = text(&app);
    assert!(
        then.iter().all(|sentence| says(&first, sentence)),
        "PRECONDITION: the first reading's rows\n{first}"
    );

    app.vitals.snapshot = Some(Arc::new(snap.clone()));
    app.publish_resources();
    let compiled = app.compiled_native_ui(wid).expect("the page compiles");
    let head = compiled
        .paint
        .iter()
        .find(|node| node.key.as_str() == HEAD_KEY)
        .expect("the head is painted");
    let crate::native_ui::UiContent::Resources(spec) = &head.content else {
        panic!("the head is the deck")
    };
    let roles = crate::settings::Roles::from_theme(aterm_render::Theme::default());
    let (_, hits) = spec.deck(
        head.rect.width,
        crate::native_resources::ResourcesPaint::inks(&roles),
    );
    let text = text(&app);
    let locale = crate::stats_deck::host_locale(Some(&snap));
    let mut rows = 0;
    for hit in &hits {
        let res = match hit.kind {
            HitKind::Row(instrument, _) => instrument.resource(),
            HitKind::Cores => Some(aterm_vitals::model::Resource::Cpu),
            _ => None,
        };
        let Some(res) = res else { continue };
        rows += 1;
        let sentence = aterm_vitals::words::row_sentence(res, &snap, &[], &locale);
        assert!(says(&text, &sentence), "{res:?}: {sentence:?} in\n{text}");
    }
    assert!(
        rows >= 6,
        "PRECONDITION: the head draws its rows ({rows} hits)"
    );
    let now = sentences(&snap);
    let moved: Vec<_> = then
        .iter()
        .zip(&now)
        .filter(|(was, is)| was != is)
        .map(|(was, _)| was)
        .collect();
    assert!(!moved.is_empty(), "PRECONDITION: the readings differ");
    for was in moved {
        assert!(
            !says(&text, was),
            "NEGATIVE CONTROL: the older reading's {was:?} is gone"
        );
    }
}

/// A scroll the page cannot take is taken and draws nothing: where every document fits,
/// wheel and page steps leave the presentation revision still.
/// NEGATIVE CONTROL: the same scroll in a short viewport moves and repaints the page.
#[test]
fn a_scroll_the_resources_page_cannot_take_draws_nothing() {
    use crate::native_app::{AppEvent, EventResult};
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.vitals.snapshot = Some(Arc::new(crate::stats_deck::tests::snapshot()));
    open(&mut app);
    let (instance, view) = app.active_native_view(wid).expect("the page is in front");
    let revision = |app: &App| {
        app.native_runtime
            .view_state(view)
            .expect("the page's view")
            .common()
            .presentation_revision
    };
    let tall = crate::native_ui::LogicalRect::new(0.0, 0.0, 1200.0, 100_000.0);
    let compiled = app
        .compiled_native_ui_for(wid, instance, view, tall)
        .expect("the whole page compiles");
    assert!(
        compiled
            .paint
            .iter()
            .any(|node| node.key.as_str() == HEAD_KEY),
        "PRECONDITION: the head is laid out whole"
    );
    let before = revision(&app);
    assert_eq!(
        app.scroll_native_view(wid, view, 3, None),
        Ok(EventResult::Handled)
    );
    assert_eq!(
        app.dispatch_native_event(wid, AppEvent::ScrollPage(1)),
        Ok(EventResult::Handled)
    );
    assert_eq!(revision(&app), before, "nothing moved, nothing drawn");

    let short = crate::native_ui::LogicalRect::new(0.0, 0.0, 800.0, 90.0);
    app.compiled_native_ui_for(wid, instance, view, short)
        .expect("the page compiles short");
    assert_eq!(
        app.scroll_native_view(wid, view, 3, None),
        Ok(EventResult::Handled)
    );
    assert_ne!(
        revision(&app),
        before,
        "NEGATIVE CONTROL: a scroll that moves draws"
    );
}

/// Copy uses the host's native clipboard effect: a control client's press is refused, while
/// a person's press copies the document's sentence or CPU report. The Memory control proves
/// Copy is bound to the document rather than to one shared sentence.
#[test]
fn a_copy_press_on_the_page_copies_for_a_person_and_is_refused_a_control_client() {
    use crate::app_take_over::CLIPBOARD_REFUSED;
    use crate::native_app::ActionInvocation;
    use crate::native_ui::{ActionId, UiKey};
    use crate::stats_paint::Instrument;
    const WID: WindowId = WindowId(0);
    let board = || crate::control::PBPASTE_STUB.with(|board| board.borrow().clone());
    let set_board = |text: Option<&str>| {
        crate::control::PBPASTE_STUB.with(|board| *board.borrow_mut() = text.map(str::to_owned))
    };
    let mut app = App::headless_for_test();
    app.open_resources_tab().expect("the page opens");
    let snap = Arc::new(crate::stats_deck::tests::snapshot());
    app.vitals.snapshot = Some(Arc::clone(&snap));
    app.publish_resources();
    let (_, view) = app.active_native_view(WID).expect("the page is in front");
    let locale = crate::stats_deck::host_locale(Some(&snap));
    let cpu = crate::stats_lens::lens_sentence(&snap, Instrument::Cpu, &[], &locale);
    let memory = crate::stats_lens::lens_sentence(&snap, Instrument::Memory, &[], &locale);
    assert_ne!(cpu, memory, "the control document has its own words");
    for (action, expected) in [
        ("resources/doc/cpu/copy", cpu),
        (
            "resources/doc/cpu/copy-report",
            crate::stats_lens::cpu_diagnostic_report(&snap),
        ),
    ] {
        set_board(Some("before"));
        app.act_app(crate::app_control::ActRequest {
            view,
            ui_key: action.into(),
            action: action.into(),
            value: None,
        })
        .expect("the controller's press is handled");
        assert_eq!(
            board().as_deref(),
            Some("before"),
            "the controller copies nothing"
        );
        let compiled = app.compiled_native_ui(WID).expect("the page compiles");
        let said = compiled
            .semantic(&UiKey::new("resources/doc/cpu/said"))
            .expect("the refusal has a status line");
        assert!(said.label.contains(CLIPBOARD_REFUSED), "{}", said.label);
        app.dispatch_native_control_action(
            WID,
            view,
            UiKey::new(action),
            ActionInvocation {
                id: ActionId::new(action),
                value: None,
            },
            true,
        )
        .expect("the person's press is handled");
        assert_eq!(board(), Some(expected), "the person's copy is exact");
    }
    let action = "resources/doc/memory/copy";
    app.dispatch_native_control_action(
        WID,
        view,
        UiKey::new(action),
        ActionInvocation {
            id: ActionId::new(action),
            value: None,
        },
        true,
    )
    .expect("the person's Memory press is handled");
    assert_eq!(board(), Some(memory), "the control copies its own document");
    set_board(None);
}

/// Every front-tab and occlusion transition is observed between readings. A fresh host makes
/// its survey due without advancing the wall clock; the page's model retains its visibility.
/// CONTROL: another sync of the already-visible page asks no survey even when one is due.
#[cfg(unix)]
#[test]
fn a_resources_page_visibility_edge_starts_a_due_survey_without_an_adopted_reading() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let (terminal, _) = front(&app, wid);
    open(&mut app);
    let (page, _) = front(&app, wid);
    assert!(app.resources_page_on_screen());
    assert!(
        app.vitals.survey_asked(),
        "opening the page starts a survey"
    );

    // Give the retained page a due survey, as after the minimum interval has elapsed. No
    // reading is adopted, and a sync alone must not turn steady visibility into a new edge.
    app.vitals = Default::default();
    app.sync_window(wid);
    assert!(
        !app.vitals.survey_asked(),
        "CONTROL: no new visibility edge"
    );
    let terminal_index = index_of(&app, wid, terminal);
    app.switch_tab_in(wid, terminal_index);
    assert!(!app.resources_page_on_screen());
    assert!(!app.vitals.survey_asked(), "hiding asks no survey");
    let page_index = index_of(&app, wid, page);
    app.switch_tab_in(wid, page_index);
    assert!(app.resources_page_on_screen());
    assert!(
        app.vitals.survey_asked(),
        "the tab's return starts the due survey"
    );
    assert!(app.vitals.snapshot.is_none(), "no reading was adopted");

    // Occlusion can also hide and reveal the same page between two readings.
    app.vitals = Default::default();
    app.on_occlusion_changed(wid, false, std::time::Instant::now());
    assert!(
        !app.vitals.survey_asked(),
        "CONTROL: it was already on screen"
    );
    app.on_occlusion_changed(wid, true, std::time::Instant::now());
    assert!(!app.resources_page_on_screen());
    assert!(!app.vitals.survey_asked(), "occluding asks no survey");
    app.on_occlusion_changed(wid, false, std::time::Instant::now());
    assert!(app.resources_page_on_screen());
    assert!(
        app.vitals.survey_asked(),
        "uncovering starts the due survey"
    );
    assert!(app.vitals.snapshot.is_none(), "no reading was adopted");
}

thread_local! {
    /// The mdfind runs the counting probe made on this thread.
    static MDFIND_RUNS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// A probe port that counts its runs: a scope of five folders none of which the receipt names,
/// and the build cache, so the six folders a tick may ask outnumber what one tick asks.
fn counting_port() -> crate::resources_spotlight::ScopePort {
    use crate::resources_spotlight::{Found, ScopeListing, ScopePort};
    use std::path::PathBuf;
    ScopePort {
        list: || {
            let home = PathBuf::from("/Users//t");
            ScopeListing {
                receipt: Ok(Default::default()),
                refused: Default::default(),
                wanted: Ok(["a", "b", "c", "d", "e"]
                    .into_iter()
                    .map(|name| home.join(name))
                    .collect()),
                cache: Some(aterm_toolchain::build_cache::cache_root(&home)),
                yours: Default::default(),
                pause: None,
                home: Some(home),
            }
        },
        found: |_| {
            MDFIND_RUNS.with(|runs| runs.set(runs.get() + 1));
            Found::Nothing
        },
    }
}

/// Spotlight's scope probe runs only while the page is on screen, through the host's own gate
/// (`publish_resources`, then `service_spotlight_probe`): with the page in front it asks of four
/// of its six folders, nothing again within the minute, the other two once the minute passed,
/// nothing while every answer stands, nothing while a terminal tab is in front of the page or
/// its window is occluded however long that lasts, and four again once the page is back in
/// front after their answers aged out.
/// NEGATIVE CONTROL: the same steps serviced ungated probe behind the tab and in the occluded
/// window, so the counts tell a host that gates on the page being on screen from one that does
/// not.
#[test]
fn the_spotlight_documents_probe_runs_only_while_the_page_is_on_screen() {
    use crate::resources_spotlight::ScopeSlot;
    use std::time::{Duration, Instant};
    type Probe<'a> = &'a dyn Fn(&mut App, Option<Instant>);
    let runs = |app: &mut App, probe: Probe<'_>, at: Option<Instant>| {
        MDFIND_RUNS.with(|runs| runs.set(0));
        probe(app, at);
        MDFIND_RUNS.with(std::cell::Cell::get)
    };
    let steps = |probe: Probe<'_>| -> Vec<usize> {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let (terminal, _) = front(&app, wid);
        open(&mut app);
        let (page, _) = front(&app, wid);
        app.vitals.spotlight_scope = Arc::new(ScopeSlot::new(counting_port()));
        let mut counts = vec![runs(&mut app, probe, None)];
        // The first tick ran at or before t0, so each later step is at least its offset after.
        let t0 = Instant::now();
        let at = |s: u64| Some(t0 + Duration::from_secs(s));
        counts.push(runs(&mut app, probe, at(30)));
        counts.push(runs(&mut app, probe, at(61)));
        counts.push(runs(&mut app, probe, at(5 * 60)));
        let terminal_index = index_of(&app, wid, terminal);
        app.switch_tab_in(wid, terminal_index);
        assert!(
            !app.resources_page_on_screen(),
            "PRECONDITION: behind a tab"
        );
        counts.push(runs(&mut app, probe, at(11 * 60)));
        let page_index = index_of(&app, wid, page);
        app.switch_tab_in(wid, page_index);
        app.on_occlusion_changed(wid, true, std::time::Instant::now());
        assert!(!app.resources_page_on_screen(), "PRECONDITION: occluded");
        counts.push(runs(&mut app, probe, at(22 * 60)));
        app.on_occlusion_changed(wid, false, std::time::Instant::now());
        assert!(app.resources_page_on_screen(), "PRECONDITION: in front");
        counts.push(runs(&mut app, probe, at(33 * 60)));
        counts
    };
    let host = |app: &mut App, at: Option<Instant>| match at {
        None => app.publish_resources(),
        Some(at) => app.service_spotlight_probe(at),
    };
    assert_eq!(steps(&host), [4, 0, 2, 0, 0, 0, 4]);
    let ungated = |app: &mut App, at: Option<Instant>| {
        app.vitals
            .spotlight_scope
            .service(true, at.unwrap_or_else(Instant::now));
    };
    assert_eq!(steps(&ungated), [4, 0, 2, 0, 4, 4, 4], "NEGATIVE CONTROL");
}

/// OPEN APP SYSTEM-RESOURCES AT MACOS-CONFIGURATION SCROLLS TO IT: `open app system-resources
/// section=macos-configuration` opens the page with its documents laid above the section and
/// scrolls the section's heading into the room, the page's title gone above it, replying with
/// the section.
/// CONTROLS: a plain `open app system-resources` leaves the page at its top, the section out of
/// sight; `section=bogus` is a usage error.
#[test]
fn open_app_system_resources_at_macos_configuration_scrolls_to_it() {
    use crate::native_resources::MACOS_KEY;
    use crate::native_ui::UiKey;
    const WID: WindowId = WindowId(0);
    let offscreen = |app: &App, key: &str| {
        app.compiled_native_ui(WID)
            .expect("the page compiles")
            .semantic(&UiKey::new(key))
            .unwrap_or_else(|| panic!("the page lays {key}"))
            .offscreen
    };
    let opened = |words: &str| {
        let mut app = App::headless_for_test();
        app.vitals.snapshot = Some(Arc::new(crate::stats_deck::tests::snapshot()));
        let request = parse_open(words).expect("the request parses");
        let reply = app.open_app(request).expect("the page opens");
        (app, reply)
    };

    let (app, reply) = opened("app system-resources");
    assert_eq!(reply, "app system-resources");
    assert!(
        offscreen(&app, MACOS_KEY) && !offscreen(&app, "resources/title"),
        "CONTROL: a plain open is at the page's top, the section below the room"
    );

    let (app, reply) = opened("app system-resources section=macos-configuration");
    assert_eq!(reply, "app system-resources section=macos-configuration");
    assert!(
        !offscreen(&app, MACOS_KEY),
        "the section's heading is in the room"
    );
    assert!(
        offscreen(&app, "resources/title"),
        "the page scrolled: its title is above the room"
    );

    assert!(
        parse_open("app system-resources section=bogus").is_err(),
        "CONTROL: an unknown section is refused"
    );
}

/// The page in front of `WID`'s window reading the scope `list` lists (each folder's probe
/// answering nothing) and the data volume's indexing as `data`, through the host's own path
/// (`publish_resources`): its view.
#[cfg(target_os = "macos")]
pub(crate) fn spotlight_page(
    app: &mut App,
    list: fn() -> crate::resources_spotlight::ScopeListing,
    data: aterm_vitals::spotlight::Indexing,
    pause_dir: Option<&std::path::Path>,
) -> crate::tab_model::ViewId {
    use crate::resources_spotlight::{Found, ScopePort, ScopeSlot, tests};
    let mut slot = ScopeSlot::new(ScopePort {
        list,
        found: |_| Found::Nothing,
    });
    if let Some(dir) = pause_dir {
        slot = slot.with_pause_dir(dir.to_path_buf());
    }
    app.vitals.spotlight_scope = Arc::new(slot);
    app.vitals.snapshot = Some(Arc::new(tests::snapshot(tests::volumes(data))));
    app.open_resources_tab().expect("the page opens");
    front(app, WindowId(0)).1
}

/// The scope with every folder kept out on setup's receipt (the scope reads applied).
#[cfg(target_os = "macos")]
pub(crate) fn applied() -> crate::resources_spotlight::ScopeListing {
    crate::resources_spotlight::tests::covered().listing
}

/// The scope with $HOME/trust missing from setup's receipt (Apply scope… has work).
#[cfg(target_os = "macos")]
pub(crate) fn unapplied() -> crate::resources_spotlight::ScopeListing {
    crate::resources_spotlight::tests::missing_trust().listing
}

/// A person's press of the Spotlight document's button `step` on `view`.
#[cfg(target_os = "macos")]
pub(crate) fn press_spotlight(
    app: &mut App,
    view: crate::tab_model::ViewId,
    step: aterm_types::macos_setup::root_only::Step,
) -> Result<crate::native_app::EventResult, String> {
    use crate::native_resources::{SPOTLIGHT_CONTROL_PREFIX, spotlight_control_word};
    use crate::native_ui::{ActionId, UiKey};
    let id = format!("{SPOTLIGHT_CONTROL_PREFIX}{}", spotlight_control_word(step));
    app.dispatch_native_control_action(
        WindowId(0),
        view,
        UiKey::new(id.clone()),
        crate::native_app::ActionInvocation {
            id: ActionId::new(id),
            value: None,
        },
        false,
    )
}

/// EACH SPOTLIGHT CONTROL RUNS ITS ROOT COMMAND UNDER THE PAGE'S PASSWORD SHEET (SD-96): a
/// person's press of each of the Spotlight document's buttons, each live on a page whose scope
/// and indexing give it work, starts one root step bound to the page's view whose sudo runs this
/// aterm's single-step plan (`--only spotlight-scope`, `spotlight-pause`, `spotlight-resume`,
/// `spotlight-rebuild`, and `spotlight-keep-out --keep-out <the folder the open panel chose>`);
/// its password request raises the (headless) sheet on the page's window standing for the view,
/// in words that name what it changes and the command it runs (`mdutil -i off`, `-i on` and
/// `-E` on the data volume; Rebuild's names the hours it costs). While it runs the page shows its
/// button busy and every button idle. Bringing a terminal tab in front
/// withdraws the sheet and cancels the step. A keep-out that applied lists its folder as kept
/// out by you.
/// CONTROLS: Apply on an applied scope and Apply and Rebuild with indexing off are idle and their
/// press runs nothing; Rebuild pressed while a terminal tab is in front, a second press while
/// one is pending, and a keep-out of a folder Apply scope already keeps out (~/Library/Caches)
/// run nothing, each saying why.
#[cfg(target_os = "macos")]
#[test]
fn each_spotlight_control_runs_its_root_command_under_the_pages_password_sheet() {
    use crate::askpass::AskpassRequest;
    use crate::askpass_sheet::SheetOrigin;
    use crate::cpu_policy_authorization::{Origin, read_identity, slot_for_test};
    use crate::root_step_authorization::{
        OwnedOperation, active_for_test, request, sudo_args, vacate_for_test,
    };
    use aterm_types::macos_setup::root_only::Step;
    use aterm_vitals::spotlight::Indexing;
    use std::path::Path;
    const WID: WindowId = WindowId(0);
    const ARCHIVE: &str = "/Users//t/Documents/Archive";
    let _slot = slot_for_test();
    vacate_for_test();
    let exe = Path::new("/Applications/aterm.app/Contents/MacOS/aterm");
    let cases: [(
        Step,
        fn() -> crate::resources_spotlight::ScopeListing,
        Indexing,
        &[&str],
        &str,
    ); 5] = [
        (
            Step::SpotlightScope,
            unapplied,
            Indexing::Enabled,
            &["--only", "spotlight-scope"],
            "keep the folders setup scopes out of Spotlight",
        ),
        (
            Step::SpotlightPause,
            applied,
            Indexing::Enabled,
            &["--only", "spotlight-pause"],
            "/usr/bin/mdutil -i off /System/Volumes/Data",
        ),
        (
            Step::SpotlightResume,
            applied,
            Indexing::Disabled,
            &["--only", "spotlight-resume"],
            "/usr/bin/mdutil -i on /System/Volumes/Data",
        ),
        (
            Step::SpotlightRebuild,
            applied,
            Indexing::Enabled,
            &["--only", "spotlight-rebuild"],
            "/usr/bin/mdutil -E /System/Volumes/Data",
        ),
        (
            Step::SpotlightKeepOut,
            applied,
            Indexing::Enabled,
            &["--only", "spotlight-keep-out", "--keep-out", ARCHIVE],
            "keep /Users//t/Documents/Archive out of Spotlight",
        ),
    ];
    for (step, list, data, tail, words) in cases {
        let mut app = App::headless_for_test();
        app.folder_picker = |_, _| Some(std::path::PathBuf::from(ARCHIVE));
        let terminal = app.windows[&WID].tab_set.active().expect("a tab").id;
        let view = spotlight_page(&mut app, list, data, None);
        assert_eq!(
            press_spotlight(&mut app, view, step),
            Ok(crate::native_app::EventResult::Handled)
        );
        let op = active_for_test().unwrap_or_else(|| panic!("{step:?}: a person's press runs"));
        assert_eq!(op.step(), step);
        let mut wanted: Vec<std::ffi::OsString> = [
            "-k",
            "-A",
            "--",
            "/Applications/aterm.app/Contents/MacOS/aterm",
            "machines",
            "here",
            "apply",
            "--plain",
        ]
        .map(std::ffi::OsString::from)
        .to_vec();
        wanted.extend(tail.iter().map(std::ffi::OsString::from));
        assert_eq!(sudo_args(exe, op.step(), op.folder()), wanted, "{step:?}");

        op.bind_sudo_for_test(read_identity(std::process::id()).expect("this process reads"));
        let (owned, ticket) = request(std::process::id() as i32)
            .expect("the bound sudo's one request")
            .expect("this root step's");
        let owned = OwnedOperation::RootStep(owned);
        let (message, details) = owned.sheet_words();
        let said = format!("{message} {details}");
        assert!(said.contains(words), "{step:?}: {said}");
        assert!(said.contains("System Resources ▸ Spotlight"), "{said}");
        if step == Step::SpotlightRebuild {
            assert!(said.contains("hours"), "{said}");
        }
        let (reply, answer) = std::sync::mpsc::channel();
        app.present_askpass_sheet(AskpassRequest {
            ticket: Arc::clone(&ticket),
            attribute_from: 0,
            owned: Some(owned),
            exact: true,
            command: details,
            purpose: Some(message),
            invoker: "System Resources ▸ Spotlight".into(),
            reply,
        });
        assert!(
            matches!(answer.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty)),
            "{step:?}: the sheet is raised, not refused"
        );
        assert_eq!(
            app.askpass_sheet
                .as_ref()
                .map(|sheet| sheet.origin_for_test()),
            Some((WID, SheetOrigin::View(view))),
            "{step:?}: it hangs on the page's window and stands for its view"
        );
        assert_eq!(op.origin_for_test(), Origin::View { window: WID, view });

        // While it runs the page shows its button busy and every button idle.
        let compiled = app.compiled_native_ui(WID).expect("the page compiles");
        let mut shown = Vec::new();
        for other in [
            Step::SpotlightScope,
            Step::SpotlightPause,
            Step::SpotlightResume,
            Step::SpotlightRebuild,
            Step::SpotlightKeepOut,
        ] {
            let id = format!(
                "{}{}",
                crate::native_resources::SPOTLIGHT_CONTROL_PREFIX,
                crate::native_resources::spotlight_control_word(other)
            );
            if let Some(node) = compiled.semantic(&crate::native_ui::UiKey::new(id)) {
                shown.push(other);
                assert!(
                    node.state
                        .is_some_and(|state| !state.enabled && state.busy == (other == step)),
                    "{step:?} pending: {other:?} {:?}",
                    node.state
                );
            }
        }
        assert!(
            shown.contains(&step) && shown.len() == 4,
            "{step:?}: {shown:?}"
        );
        // CONTROL: a second press while this one runs runs nothing more.
        let _ = press_spotlight(&mut app, view, Step::SpotlightKeepOut);
        assert!(
            active_for_test().is_some_and(|other| Arc::ptr_eq(&other, &op)),
            "CONTROL: one root step at a time"
        );

        let back = index_of(&app, WID, terminal);
        app.switch_tab_in(WID, back);
        assert!(
            app.askpass_sheet.is_none(),
            "{step:?}: the sheet goes with the view"
        );
        assert!(
            op.cancelled_for_test(),
            "{step:?}: and the step is cancelled"
        );
        vacate_for_test();
        if step == Step::SpotlightRebuild {
            app.press_spotlight_control(WID, view, Step::SpotlightRebuild);
            assert!(
                active_for_test().is_none(),
                "CONTROL: behind a terminal tab"
            );
            assert!(
                app.vitals
                    .spotlight_scope
                    .said()
                    .is_some_and(|said| said.contains("not in front")),
                "{:?}",
                app.vitals.spotlight_scope.said()
            );
        }
    }

    // A keep-out that applied lists its folder as the person's.
    let mut reading = crate::resources_spotlight::tests::covered();
    reading.listing.yours = [std::path::PathBuf::from(ARCHIVE)].into();
    let rows =
        crate::resources_spotlight::what_is_indexed(Some(&reading), Some(&Indexing::Enabled));
    assert!(
        rows.iter().any(|row| row.name == "~/Documents/Archive"
            && row.now == crate::resources_spotlight::Now::KeptOut
            && row.words.contains("you kept it out")),
        "{rows:?}"
    );

    // CONTROLS: idle buttons run nothing.
    for (step, list, data) in [
        (
            Step::SpotlightScope,
            applied as fn() -> _,
            Indexing::Enabled,
        ),
        (Step::SpotlightScope, unapplied, Indexing::Disabled),
        (Step::SpotlightRebuild, applied, Indexing::Disabled),
    ] {
        let mut app = App::headless_for_test();
        let view = spotlight_page(&mut app, list, data, None);
        let _ = press_spotlight(&mut app, view, step);
        assert!(active_for_test().is_none(), "CONTROL: {step:?} is idle");
    }
    let mut app = App::headless_for_test();
    app.folder_picker = |_, _| Some(std::path::PathBuf::from("/Users//t/Library/Caches"));
    let view = spotlight_page(&mut app, applied, Indexing::Enabled, None);
    assert_eq!(
        press_spotlight(&mut app, view, Step::SpotlightKeepOut),
        Ok(crate::native_app::EventResult::Handled)
    );
    assert!(
        active_for_test().is_none(),
        "CONTROL: a folder Apply scope keeps out"
    );
    let said = app.vitals.spotlight_scope.said().unwrap_or_default();
    assert!(
        said.contains("~/Library/Caches") && said.contains("Apply scope keeps"),
        "{said}"
    );
}

/// A PAUSE READS PAUSED BY YOU UNTIL RESUME (SD-96): a person's press of Pause whose run ended
/// applied records its time (by the run's own end, [`crate::root_step_authorization::finish`],
/// not the App), and that end empties the slot; the document then says Paused by you, and offers
/// Resume with Apply scope… and Rebuild index… idle, though the last reading of the data volume,
/// made before the pause, said indexing was on; a
/// reading made after it that says off still says Paused by you, and so does System State's
/// Spotlight indexing row (FL-188) reading it, and a new App over the same record (a restart).
/// A reading made after it that says on (macOS turned it back on) ends the record, and the
/// document says the reading's own word. A Resume that applied reads Resuming until a reading made
/// after it, which ends that record too.
/// CONTROLS: a Disabled reading with no record reads Off, never Paused by you, and System State's
/// row then says Off, left as found; before the pause, with indexing on and no record, Rebuild
/// index… is live; a Pause whose run ended not applied records nothing.
#[cfg(target_os = "macos")]
#[test]
fn a_pause_reads_paused_by_you_until_resume() {
    use crate::cpu_policy_authorization::slot_for_test;
    use crate::resources_spotlight::{ScopeListing, ScopeReading, controls, tests};
    use crate::root_step_authorization::{Ended, active_for_test, finish, vacate_for_test};
    use aterm_types::macos_setup::root_only::Step;
    use aterm_types::macos_setup::spotlight_scope::{self, Mark};
    use aterm_vitals::spotlight::Indexing;
    let dir = aterm_tempfile::tempdir().expect("a scratch folder");
    let dir = dir.path();
    // The fixture's volumes reading started at this time (its split's time less its age).
    let read_at = 1_763_300_000_000_i64 - 42_000;
    let snap_at = |data: Indexing, started: i64| {
        let mut snap = tests::snapshot(tests::volumes(data));
        let reading = snap.spotlight.as_mut().expect("a Spotlight reading");
        reading.at_unix_ms = started + 42_000;
        snap
    };
    let words = |app: &App, snap: &aterm_vitals::Snapshot| -> (String, ScopeReading) {
        let (scope, _) = app
            .vitals
            .spotlight_scope
            .reading()
            .expect("the probe read");
        let said = crate::native_resources::document_words(tests::laid(snap, &scope));
        (said, (*scope).clone())
    };
    // The page's host reads `snap`, its probe reading the record now.
    let reread = |app: &mut App, snap: aterm_vitals::Snapshot| {
        app.vitals.snapshot = Some(Arc::new(snap));
        app.vitals.spotlight_scope.relist_now();
        app.publish_resources();
    };
    // A person's press of `step` on `view`, its run then ending `ended` at `at` as a worker's
    // run ends: the record is the run's end's, and the slot is empty after it.
    let run = |app: &mut App, view, step: Step, ended: Ended, at: i64| {
        assert_eq!(
            press_spotlight(app, view, step),
            Ok(crate::native_app::EventResult::Handled)
        );
        let op = active_for_test().unwrap_or_else(|| panic!("{step:?}: a person's press runs"));
        assert_eq!(op.step(), step);
        let crate::Wake::RootStepEnded { step: woke, ended } = finish(&op, ended, Some(dir), at)
        else {
            panic!("{step:?}: the run's end wakes the App with how it ended");
        };
        assert_eq!(woke, step);
        assert!(
            active_for_test().is_none(),
            "{step:?}: its end empties the slot"
        );
        app.root_step_ended(woke, &ended);
    };
    let _slot = slot_for_test();
    vacate_for_test();

    let paused_at = read_at + 1_000;
    let mut app = App::headless_for_test();
    let view = spotlight_page(&mut app, applied, Indexing::Enabled, Some(dir));
    let before = snap_at(Indexing::Enabled, read_at);
    reread(&mut app, before.clone());
    let (_, scope) = words(&app, &before);
    assert!(
        controls(Some(&scope), Some(&before))
            .buttons
            .contains(&(Step::SpotlightRebuild, true)),
        "CONTROL: Rebuild is live while indexing is on"
    );
    // CONTROL: a Pause that did not apply records nothing.
    run(
        &mut app,
        view,
        Step::SpotlightPause,
        Ended::NotApplied("exit 1".to_owned()),
        paused_at,
    );
    assert_eq!(
        spotlight_scope::read_pause(dir),
        None,
        "CONTROL: not applied"
    );
    run(
        &mut app,
        view,
        Step::SpotlightPause,
        Ended::Applied,
        paused_at,
    );
    assert_eq!(
        spotlight_scope::read_pause(dir).map(|r| (r.mark, r.at_unix_ms)),
        Some((Mark::Paused, paused_at))
    );
    reread(&mut app, before.clone());
    let (said, scope) = words(&app, &before);
    assert!(said.contains("Paused by you since"), "{said}");
    let buttons = controls(Some(&scope), Some(&before)).buttons;
    assert!(
        buttons.contains(&(Step::SpotlightResume, true)),
        "Resume is live: {buttons:?}"
    );
    assert!(
        buttons.contains(&(Step::SpotlightRebuild, false)),
        "Rebuild is idle while paused: {buttons:?}"
    );
    // The same reading over a scope that misses a folder, the record still read.
    let mut unapplied_scope = scope.clone();
    unapplied_scope.listing = ScopeListing {
        pause: scope.listing.pause.clone(),
        ..unapplied()
    };
    assert!(
        controls(Some(&unapplied_scope), Some(&before))
            .buttons
            .contains(&(Step::SpotlightScope, false)),
        "Apply scope… is idle while paused"
    );

    let off = snap_at(Indexing::Disabled, paused_at + 5_000);
    reread(&mut app, off.clone());
    let (said, _) = words(&app, &off);
    assert!(said.contains("Paused by you since"), "{said}");
    // System State's Spotlight indexing row, as its reader reads the record with indexing off.
    let paused =
        aterm_types::macos_setup::spotlight_paused(spotlight_scope::read_pause(dir).as_ref(), true)
            .expect("the row reads the pause");
    let row = system_state_spotlight_row(paused.0, Some(&paused.1));
    assert!(row.label.contains("Paused by you"), "{}", row.label);

    let mut restarted = App::headless_for_test();
    spotlight_page(&mut restarted, applied, Indexing::Disabled, Some(dir));
    reread(&mut restarted, off.clone());
    let (said, _) = words(&restarted, &off);
    assert!(
        said.contains("Paused by you since"),
        "after a restart: {said}"
    );

    let on = snap_at(Indexing::Enabled, paused_at + 60_000);
    reread(&mut app, on.clone());
    assert_eq!(
        spotlight_scope::read_pause(dir),
        None,
        "macOS turned it on: the record ends"
    );
    let (said, _) = words(&app, &on);
    assert!(
        !said.contains("Paused by you") && said.contains("Idle"),
        "{said}"
    );

    let resumed_at = paused_at + 120_000;
    run(
        &mut app,
        view,
        Step::SpotlightPause,
        Ended::Applied,
        resumed_at - 10_000,
    );
    reread(&mut app, on.clone());
    run(
        &mut app,
        view,
        Step::SpotlightResume,
        Ended::Applied,
        resumed_at,
    );
    assert_eq!(
        spotlight_scope::read_pause(dir).map(|r| r.mark),
        Some(Mark::Resumed)
    );
    let stale = snap_at(Indexing::Disabled, resumed_at - 20_000);
    reread(&mut app, stale.clone());
    let (said, _) = words(&app, &stale);
    assert!(said.contains("Resuming"), "{said}");
    let later = snap_at(Indexing::Enabled, resumed_at + 30_000);
    reread(&mut app, later.clone());
    assert_eq!(
        spotlight_scope::read_pause(dir),
        None,
        "a reading after the resume ends it"
    );
    let (said, _) = words(&app, &later);
    assert!(!said.contains("Resuming"), "{said}");

    // CONTROL: off with no record of the person's is Off.
    let (said, _) = words(&app, &off);
    assert!(
        !said.contains("Paused by you") && said.contains("Off"),
        "CONTROL: {said}"
    );
    assert_eq!(
        aterm_types::macos_setup::spotlight_paused(None, true),
        None,
        "CONTROL: no record, no pause"
    );
    let row = system_state_spotlight_row(aterm_types::macos_setup::SPOTLIGHT_OFF, None);
    assert!(
        row.label.contains("Off, left as found"),
        "CONTROL: {}",
        row.label
    );
}

/// System State's Spotlight indexing row (FL-188) where its reader read `now` and `note`.
#[cfg(target_os = "macos")]
fn system_state_spotlight_row(now: &str, note: Option<&str>) -> crate::system_state::RowView {
    use aterm_types::macos_setup::{Access, Standing, Subject};
    let mut status = crate::system_state::fixture_status("set-up").expect("the set-up scenario");
    let row = status
        .observed
        .iter_mut()
        .find(|row| row.subject == Subject::Access(Access::SpotlightScope))
        .expect("the scenario reads the scope");
    row.standing = Standing::InPlace;
    row.now = Some(now.to_owned());
    row.note = note.map(str::to_owned);
    let selection = status
        .plan
        .as_ref()
        .map_or_else(aterm_types::macos_setup::Selection::none, |plan| {
            plan.selection
        });
    crate::system_state::row_view(Subject::Access(Access::SpotlightScope), &status, selection)
}
