// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE SEAMS (`docs/DESIGN-harness-autopilot-2026-10-15.md` §12.5.2, the 5a tests of §12.5.6), on a headless App: a development build's stand-in for the person — `person look <window>`, which reaches the one looking rule alone and arms no timer, and `person menubar press <at>`, which clicks a menu-bar row by the path `chrome` prints and goes where a click goes — and `status`'s `end= end_seen= end_cheered=`, two facts kept apart.

use std::sync::Arc;
use std::time::{Duration, Instant};

use aterm_agent::harness::acts::TaskFace;

use crate::control::control_media::{PERSON_DENIED, PERSON_NEEDS_DEBUG, PersonAsked, parse_person};
use crate::presence::ChipLevel;
use crate::{App, WindowId};

const SEC: u64 = 1_000;

/// Shut the test's host down on success and assertion unwind alike.
struct TestHost(crate::harness_host::HostHandle);

impl Drop for TestHost {
    fn drop(&mut self) {
        self.0.shutdown_and_join();
    }
}

/// A host that supervises nothing: what the App's posture needs.
fn host() -> TestHost {
    TestHost(crate::harness_host::HostHandle::for_supervised_test(
        Vec::new(),
        Arc::new(|_| crate::harness_host::BodyEnd::Stopped),
    ))
}

/// This window's host holds `session`'s live supervisor claim.
fn claim(app: &App, session: u64) {
    let ctx = app.pool.get(session).expect("session").ctx.clone();
    crate::session_timeline::claim_supervisor(
        &ctx,
        &crate::harness_host::holder(),
        None,
        None,
        crate::metrics::now_us(),
    )
    .expect("claimed");
}

/// A headless App, as the lab's is: session 0 runs Claude Code under Land,
/// claimed by this host, in window 0's first tab, a second tab in front,
/// window 0 frontmost and on no screen — nobody can look at it.
fn desk() -> (App, String, TestHost) {
    let mut app = App::headless_for_test();
    let host = host();
    app.harness = Some(host.0.clone());
    let ctx = app.pool.get(0).expect("session 0").ctx.clone();
    {
        let mut tl = ctx.timeline.lock().expect("timeline");
        tl.note_foreground_group(7);
        tl.set_program(7, Some("claude".into()));
    }
    ctx.meta
        .lock()
        .expect("meta")
        .set("autopilot", Some("land".into()));
    claim(&app, 0);
    let id = app.next_session_id;
    app.push_stub_tab(WindowId(0), crate::stub_session(id));
    app.frontmost_window = Some(WindowId(0));
    assert!(
        !crate::messages_host::band_on_screen(&app.windows[&WindowId(0)]),
        "PRECONDITION: a headless window is on no screen"
    );
    (app, ctx.self_id.as_str().to_string(), host)
}

/// The agent word the status sweep publishes for session 0.
fn publish(app: &App, word: &'static str) -> bool {
    app.pool
        .get(0)
        .expect("session 0")
        .ctx
        .timeline
        .lock()
        .expect("timeline")
        .publish_agent(
            word,
            None,
            None,
            Some(aterm_phase::Program::Claude),
            crate::session_timeline::AgentStamp {
                generation: crate::control::ScreenGen { epoch: 1, seq: 1 },
                fp: 1,
            },
        )
}

/// Session `sid`'s loop tells `face` through the host's wake.
fn tell(app: &mut App, sid: &str, face: Option<TaskFace>) {
    app.apply_harness_fact(sid, crate::HarnessFact::Task(face));
}

/// Session 0's task opens, its agent works `ms` on the ledger clock (moved
/// on, not waited), and its loop tells landed: an end past the floor.
fn end_after(app: &mut App, sid: &str, ms: u64) {
    tell(app, sid, Some(TaskFace::Open));
    assert!(publish(app, "busy"), "busy published");
    app.note_harness_phase(0);
    crate::turn_ledger::advance_for_test(ms);
    assert!(publish(app, "idle"), "idle published");
    app.note_harness_phase(0);
    tell(app, sid, Some(TaskFace::Landed));
}

/// Every tab's mark in window 0, as its strip is stamped.
fn marks(app: &App) -> Vec<ChipLevel> {
    let ws = &app.windows[&WindowId(0)];
    let mut metadata: Vec<crate::tab_bar::TabStripMetadata> = ws
        .tab_set
        .tabs()
        .iter()
        .map(|t| crate::tab_bar::TabStripMetadata::from_presentation(&t.presentation))
        .collect();
    app.stamp_presence_chips(WindowId(0), &mut metadata);
    metadata.iter().map(|m| m.attention).collect()
}

/// The value of `key=` among `line`'s words.
fn field(line: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}=");
    line.split_whitespace()
        .find_map(|w| w.strip_prefix(prefix.as_str()))
        .map(str::to_owned)
}

/// `person look <window>` (`None`: off) on the main thread.
fn look(app: &mut App, window: Option<u64>) -> Result<String, String> {
    app.person_stand_in(PersonAsked::Look(window))
}

/// THE VERB'S WORDS, AND WHO MAY SAY THEM (§12.5.2): an armed debug build
/// reads `look <n>`, `look off` and `menubar press <at>`; anything else is
/// the usage line.
#[test]
fn the_person_verb_reads_its_forms_and_refuses_unarmed_and_in_release() {
    let armed = |rest: &str| parse_person(rest, true, || true);
    assert_eq!(armed("look 0"), Ok(PersonAsked::Look(Some(0))));
    assert_eq!(armed(" look  12 "), Ok(PersonAsked::Look(Some(12))));
    assert_eq!(armed("look off"), Ok(PersonAsked::Look(None)));
    assert_eq!(
        armed("menubar press 3.0"),
        Ok(PersonAsked::Press(
            crate::status_item::MenuAt::parse("3.0").expect("a path")
        ))
    );
    for junk in [
        "",
        "look",
        "look -1",
        "look +1",
        "look 0 1",
        "look front",
        "menubar",
        "menubar press",
        "menubar press 1.",
        "menubar press x",
        "menubar click 1",
        "invoke TakeOver",
    ] {
        let refusal = armed(junk).expect_err(junk);
        assert!(refusal.starts_with("usage: person "), "{junk:?}: {refusal}");
    }
    for rest in ["look 0", "look off", "menubar press 0", "junk"] {
        assert_eq!(
            parse_person(rest, true, || false),
            Err(PERSON_DENIED.to_string()),
            "{rest}"
        );
        assert_eq!(
            parse_person(rest, false, || panic!("a release build reads no arming")),
            Err(PERSON_NEEDS_DEBUG.to_string()),
            "{rest}"
        );
    }
    assert!(PERSON_DENIED.starts_with("denied: person is a development build's stand-in"));
    assert!(PERSON_DENIED.ends_with("(ATERM_DEBUG_PERSON_INVOKE=1 at launch)"));
}

/// A STAND-IN LOOK SEES THE FRONT TAB'S END BY THE ONE RULE (§12.5.6): an
/// unseen landing on window 0's first tab, with the second in front: `person
/// look 0` leaves its `•`, its Done for you row and `❯•`; once the first tab
/// comes to the front it is seen, and the mark, the row and `❯•` go; with the
/// look standing, a landing in the front tab is seen as it lands.
#[test]
fn a_stand_in_look_sees_the_front_tabs_end_by_the_one_rule() {
    let (mut app, sid, _host) = desk();
    end_after(&mut app, &sid, 45 * SEC);
    assert!(app.end_unseen(0), "PRECONDITION: a background end stands");
    assert_eq!(marks(&app)[0], ChipLevel::Done);
    assert_eq!(app.status_glance().done.len(), 1);
    assert_eq!(app.status_glance().button_title(), "\u{276f}\u{2022}");
    app.switch_tab_in(WindowId(0), 0);
    assert!(app.end_unseen(0), "a headless window: nobody looks");
    app.switch_tab_in(WindowId(0), 1);
    assert_eq!(look(&mut app, Some(9)), Err("no window 9".to_string()));
    assert!(app.end_unseen(0), "a refused look sees nothing");

    assert_eq!(
        look(&mut app, Some(0)),
        Ok("person look window=0".to_string())
    );
    assert!(app.end_unseen(0), "a look sees the front tab alone");
    assert_eq!(marks(&app)[0], ChipLevel::Done, "the `•` stands");
    assert_eq!(app.status_glance().done.len(), 1, "the row stands");
    assert_eq!(app.status_glance().button_title(), "\u{276f}\u{2022}");

    app.take_status_rebuild();
    app.switch_tab_in(WindowId(0), 0);
    assert!(!app.end_unseen(0), "seen by the one rule");
    assert!(app.take_status_rebuild(), "the look dirties the glance");
    let g = app.status_glance();
    assert!(g.done.is_empty(), "the row goes: {:?}", g.done);
    assert_eq!(g.button_title(), "\u{276f}", "and `❯•`");
    assert_eq!(marks(&app)[0], ChipLevel::Off);

    end_after(&mut app, &sid, 40 * SEC);
    assert!(!app.end_unseen(0), "seen as it lands where someone looks");

    assert_eq!(look(&mut app, None), Ok("person look off".to_string()));
    end_after(&mut app, &sid, 40 * SEC);
    assert!(app.end_unseen(0), "nobody looks: it stands");
    assert_eq!(app.status_glance().done.len(), 1, "a Done for you row");
}

/// A STAND-IN LOOK SEES AT ONCE (§12.5.2: the one rule runs where a look
/// begins): the landed tab in front of window 0 and nobody looking, its end
/// stands (the front tab wears no `•`: the Done for you row and `❯•` say
/// it); `person look 0`, with no tab switch or landing after, sees it then,
/// and the row and `❯•` go, the glance told.
#[test]
fn a_stand_in_look_sees_the_front_tab_at_once() {
    let (mut app, sid, _host) = desk();
    end_after(&mut app, &sid, 45 * SEC);
    app.switch_tab_in(WindowId(0), 0);
    assert!(app.end_unseen(0), "in front, nobody looks: it stands");
    assert_eq!(app.status_glance().done.len(), 1);
    assert_eq!(app.status_glance().button_title(), "\u{276f}\u{2022}");
    app.take_status_rebuild();

    assert_eq!(
        look(&mut app, Some(0)),
        Ok("person look window=0".to_string())
    );
    assert!(!app.end_unseen(0), "seen at once");
    assert!(app.take_status_rebuild(), "the glance told");
    let g = app.status_glance();
    assert!(g.done.is_empty(), "the row goes: {:?}", g.done);
    assert_eq!(g.button_title(), "\u{276f}", "and `❯•`");
}

/// NOBODY LOOKS AT A WINDOW BEHIND ANOTHER (§12.5.2: the stand-in supplies
/// only "on screen"; main's rule is `frontmost && on screen`): with a second
/// window in front, `person look 0` and a tab switch in window 0 bring its
/// landed tab to window 0's front, and its end stands; once window 1 closes
/// and window 0 is the frontmost again, the same tab switch sees it.
#[test]
fn a_look_at_a_window_behind_sees_nothing() {
    let (mut app, sid, _host) = desk();
    end_after(&mut app, &sid, 45 * SEC);
    assert!(app.end_unseen(0), "PRECONDITION: a background end stands");
    let w1 = app
        .open_active_session_in_new_window_logical()
        .expect("a second window");
    assert_eq!(
        app.frontmost_window,
        Some(w1),
        "PRECONDITION: window 1 in front"
    );
    app.windows
        .get_mut(&WindowId(0))
        .expect("window 0")
        .band_on_screen_for_test = true;
    app.switch_tab_in(WindowId(0), 0);
    assert!(app.end_unseen(0), "a window behind is looked at by nobody");
    app.switch_tab_in(WindowId(0), 1);
    app.windows
        .get_mut(&WindowId(0))
        .expect("window 0")
        .band_on_screen_for_test = false;

    assert_eq!(
        look(&mut app, Some(0)),
        Ok("person look window=0".to_string())
    );
    app.switch_tab_in(WindowId(0), 0);
    assert!(app.end_unseen(0), "a look at a window behind sees nothing");
    assert_eq!(app.status_glance().done.len(), 1, "the row stands");

    let _ = app.close_window_logical(w1);
    assert_eq!(
        app.frontmost_window,
        Some(WindowId(0)),
        "PRECONDITION: window 0 in front again"
    );
    app.switch_tab_in(WindowId(0), 1);
    app.switch_tab_in(WindowId(0), 0);
    assert!(!app.end_unseen(0), "in front, looked at: seen");
    assert!(app.status_glance().done.is_empty(), "the row goes");
}

/// The fixtures of a Claude Code launch card on the glass
/// (`claude_card_tests`' own, trimmed to what a card on show needs).
mod card {
    use std::sync::Arc;

    use aterm_agent::harness::footer::FooterFacts;
    use aterm_core::terminal::{CutKind, HolderCut};

    use crate::{App, WindowId};

    /// The build the card names, and the process the facts are for.
    const VERSION: &str = "2.1.283";
    const PGID: i32 = 4242;

    /// A Claude Code screen `rows` × `cols`: the wide launch card at row 1,
    /// then the composer between its rules and the mode row at the bottom.
    fn screen(rows: usize, cols: usize) -> Vec<String> {
        let rule = "\u{2500}".repeat(cols);
        let mut screen = vec![
            String::new(),
            format!(
                " \u{2590}\u{259B}\u{2588}\u{2588}\u{2588}\u{259B}\u{2588}   {}{VERSION}",
                aterm_phase::anchor_text("launch.banner")
            ),
            "\u{259D}\u{259C}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2580}  Opus 5.5 \u{b7} Claude Max"
                .to_string(),
            " \u{259D}\u{259D}   \u{259D}\u{259D}   ~/aterm".to_string(),
        ];
        while screen.len() < rows - 4 {
            screen.push(String::new());
        }
        screen.extend([
            rule.clone(),
            "\u{276F} ".to_string(),
            rule,
            "  \u{23F5}\u{23F5} bypass permissions on (shift+tab to cycle)".to_string(),
        ]);
        screen
    }

    /// Paint `rows` into `session`'s terminal, the caret on the composer.
    fn feed(app: &App, session: u64, rows: &[String]) {
        let mut out = String::from("\x1b[2J");
        for (i, row) in rows.iter().enumerate() {
            let sgr = if row.ends_with("~/aterm") {
                "\x1b[38;2;130;130;130m"
            } else {
                ""
            };
            out.push_str(&format!("\x1b[{};1H{sgr}{row}\x1b[0m", i + 1));
        }
        let caret = rows
            .iter()
            .rposition(|r| r.starts_with('\u{276F}'))
            .unwrap_or(0);
        out.push_str(&format!("\x1b[{};3H", caret + 1));
        let term = app.pool.get(session).expect("session").term.clone();
        crate::term_lock(&term).process(out.as_bytes());
    }

    /// A managed-store Claude Code of [`VERSION`] takes `session`'s front as
    /// group [`PGID`]: its image published, the fence cut to it, its facts.
    fn launch(app: &App, session: u64) {
        use atpkg::image::{ImageBuild, ImageSource, ProgramTag};
        let build = atpkg::vendor_direct::Version::parse(VERSION)
            .expect("a vendor build")
            .build_id();
        let image = crate::holder_image::HolderImage::of(
            PGID,
            Some(1),
            Some(std::path::PathBuf::from(format!(
                "/p/pkg/store/claude/{build}/bin/claude"
            ))),
            ImageBuild {
                program: ProgramTag::new("claude"),
                build,
                source: ImageSource::Store,
            },
            1,
        );
        let token = image.token.expect("a token");
        {
            let entry = app.pool.get(session).expect("session");
            let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            timeline.note_foreground_group(PGID);
            timeline.offer_holder_image(Arc::new(image), crate::holder_image::EdgeKind::Group);
            timeline.set_claude_footer(
                PGID,
                None,
                Some(FooterFacts {
                    model: Some("Opus 5.5".into()),
                    version: Some(VERSION.into()),
                    ..FooterFacts::default()
                }),
            );
        }
        let term = app.pool.get(session).expect("session").term.clone();
        crate::term_lock(&term).note_holder_cut(HolderCut {
            to: Some(token),
            kind: CutKind::Group,
            from_alive: false,
        });
    }

    /// One frame through the capture route's order (`App::render_image`'s terminal arm): words, the strip, then the footer's splice.
    pub(super) fn frame(app: &mut App, wid: WindowId) {
        app.capture_words(wid, std::time::Instant::now());
        let (plan, route) = app.active_visible_frame_layout(wid).expect("a layout");
        app.splice_tab_strip(wid);
        app.splice_claude_footer(wid, &plan, route);
    }

    /// A headless window 120 columns wide, its front session (returned) a Claude Code whose launch card is on its screen.
    pub(super) fn glass() -> (App, u64) {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let session = app.focused_session_id(wid).expect("the front session");
        assert!(app.apply_term_resize(wid, 24, 120));
        launch(&app, session);
        feed(&app, session, &screen(24, 120));
        (app, session)
    }
}

/// A STAND-IN LOOK ARMS NO TIMER (§12.5.6): with a live row on the band, a
/// card on show and a pill standing in window 0, `person look 0` moves none
/// of the wakes that read whether a window is on screen — the card's, the
/// band's motion, the band's own, presence's (the pill's age), the harness
/// glass's — nor `session_on_screen`.
#[test]
fn a_stand_in_look_arms_no_timer() {
    let wid = WindowId(0);
    let (mut app, session) = card::glass();
    let host = host();
    app.harness = Some(host.0.clone());
    claim(&app, session);
    let other = app.next_session_id;
    app.push_stub_tab(wid, crate::stub_session(other));
    app.pool
        .get(other)
        .expect("the second tab")
        .ctx
        .meta
        .lock()
        .expect("meta")
        .attention = Some("approve the deploy?".to_string());
    app.refresh_presence_session(other);
    app.switch_tab_in(wid, 0);
    app.announce_toolchain_pass("installing 10 ALab program(s)", true);
    app.prepare_band_motion(wid, Instant::now());
    card::frame(&mut app, wid);
    assert!(app.claude_cards_live(), "PRECONDITION: a card on show");
    assert!(app.presence_pill(wid).is_some(), "PRECONDITION: a pill");
    let later = Instant::now() + Duration::from_secs(2);
    let wakes = |app: &App| {
        (
            app.claude_card_wake(),
            app.band_motion_deadline(later),
            app.messages_deadline(),
            app.presence_deadline(later),
            app.harness_acts_deadline(),
            app.session_on_screen(session),
        )
    };
    let before = wakes(&app);
    assert_eq!(
        (before.0, before.1, before.5),
        (None, None, false),
        "PRECONDITION: headless, nothing on screen wakes"
    );
    assert_eq!(
        look(&mut app, Some(0)),
        Ok("person look window=0".to_string())
    );
    assert_eq!(wakes(&app), before, "the look armed nothing");
    app.windows
        .get_mut(&wid)
        .expect("window 0")
        .band_on_screen_for_test = true;
    let on_screen = wakes(&app);
    assert!(on_screen.0.is_some(), "on a screen the card wants a frame");
    assert!(on_screen.1.is_some(), "and the band's row moves");
    assert!(on_screen.5, "and the session is on screen");
}

/// The `statusmenu` line `chrome` prints after the one starting `head`, and
/// its path (macOS: the menu-bar item's own platform).
#[cfg(target_os = "macos")]
fn row_after(app: &App, head: &str) -> (String, crate::status_item::MenuAt) {
    let lines = app.status_item_chrome_lines();
    let at = lines
        .iter()
        .position(|l| l.starts_with(head))
        .unwrap_or_else(|| panic!("no {head:?} line: {lines:#?}"));
    let line = lines[at + 1].clone();
    let path = line
        .rsplit_once(" at=")
        .and_then(|(_, at)| crate::status_item::MenuAt::parse(at))
        .unwrap_or_else(|| panic!("no path: {line}"));
    (line, path)
}

/// A STAND-IN PRESS GOES WHERE A CLICK GOES (§12.5.6): the Done for you row
/// `chrome` prints, pressed by its path, posts the row's action as the
/// AppKit relay posts a click; that wake's dispatch brings the landed tab
/// to the front and its end is seen, and the row and `❯•` go — a click's Go
/// is a look by its own act (§11.4.3, item 4), look or not.
#[cfg(target_os = "macos")]
#[test]
fn a_stand_in_press_goes_where_a_click_goes() {
    let (mut app, sid, _host) = desk();
    end_after(&mut app, &sid, 45 * SEC);
    assert!(app.end_unseen(0), "PRECONDITION: a background end stands");
    let (line, at) = row_after(&app, "statusmenu \"Done for you (1)\"");
    let label = line
        .strip_prefix("statusmenu ")
        .and_then(|l| l.split_once("\" enabled=yes"))
        .map(|(label, _)| format!("{label}\""))
        .unwrap_or_else(|| panic!("an action row: {line}"));
    let mut posted = Vec::new();
    assert_eq!(
        app.person_menubar_press(&at, &mut |wake| {
            posted.push(wake);
            true
        }),
        Ok(format!("pressed {label}"))
    );
    assert!(app.end_unseen(0), "nothing ran before the wake");
    assert_eq!(posted.len(), 1, "one click posted: {posted:?}");
    for wake in posted {
        let crate::Wake::StatusAction { action } = wake else {
            panic!("a status action: {wake:?}");
        };
        app.dispatch_status_action(action);
    }
    assert_eq!(
        app.focused_session_id(WindowId(0)),
        Some(0),
        "the landed tab in front"
    );
    assert!(!app.end_unseen(0), "its end seen");
    let g = app.status_glance();
    assert!(g.done.is_empty(), "the row goes: {:?}", g.done);
    assert_eq!(g.button_title(), "\u{276f}");

    let lines = app.status_item_chrome_lines();
    let info = lines
        .iter()
        .find(|l| l.contains(" enabled=no at="))
        .unwrap_or_else(|| panic!("an information line: {lines:#?}"));
    let info_at = info
        .rsplit_once(" at=")
        .and_then(|(_, at)| crate::status_item::MenuAt::parse(at))
        .expect("a path");
    let mut posted = Vec::new();
    let mut post = |wake| {
        posted.push(wake);
        true
    };
    assert_eq!(
        app.person_menubar_press(&info_at, &mut post),
        Err("not an action".to_string())
    );
    let past = crate::status_item::MenuAt::parse(&(lines.len() + 5).to_string()).expect("a path");
    assert_eq!(
        app.person_menubar_press(&past, &mut post),
        Err(format!("no row at {}", lines.len() + 5))
    );
    assert!(posted.is_empty(), "nothing posted: {posted:?}");

    let ctx = app.pool.get(0).expect("session 0").ctx.clone();
    let dir = aterm_tempfile::Builder::new()
        .prefix("aterm-person-press-")
        .tempdir()
        .expect("exclusive result dir");
    let path = dir.path().join("result.md");
    std::fs::write(&path, "# the worker's result\n").expect("result");
    for state in [
        crate::task_record::TaskState::Working,
        crate::task_record::TaskState::Done,
    ] {
        let mut mark = crate::task_record::TaskMark::new(state);
        if state == crate::task_record::TaskState::Done {
            mark.result_bytes = 22;
            mark.result_path = Some(path.clone());
        }
        crate::task_record::mark_session_task(&ctx, 0, &mark).expect("marked");
        app.on_task_changed(0);
    }
    app.switch_tab_in(WindowId(0), 1);
    let entered = app
        .status_glance()
        .reads
        .first()
        .map(|r| r.entered)
        .expect("PRECONDITION: a result to read");
    assert_ne!(
        app.done_seen.get(&0),
        Some(&entered),
        "PRECONDITION: unread"
    );
    let (line, at) = row_after(&app, "statusmenu \"Done for you (1)\"");
    let mut posted = Vec::new();
    let pressed = app.person_menubar_press(&at, &mut |wake| {
        posted.push(wake);
        true
    });
    assert!(pressed.is_ok(), "{line}: {pressed:?}");
    for wake in posted {
        let crate::Wake::StatusAction { action } = wake else {
            panic!("a status action: {wake:?}");
        };
        assert!(
            matches!(action, crate::status_item::StatusAction::ReadResult(_)),
            "{line}: {action:?}"
        );
        app.dispatch_status_action(action);
    }
    assert_eq!(
        app.done_seen.get(&0),
        Some(&entered),
        "the result opened, its done seen"
    );
}

/// `status` SAYS THE END, SEEN AND CHEERED APART (§12.5.2, §12.5.6): no end,
/// `end=- end_seen=- end_cheered=-`; a background landing, `end=<the unix
/// second its face moved there> end_seen=0 end_cheered=0`; cheered by a pet
/// and not yet looked at, `end_seen=0 end_cheered=1`; looked at,
/// `end_seen=1`.
#[test]
fn status_says_the_end_seen_and_cheered_apart() {
    let (mut app, sid, _host) = desk();
    let status = |app: &App| app.session_status_record(0).expect("status");
    let ends = |line: &str| {
        (
            field(line, "end"),
            field(line, "end_seen"),
            field(line, "end_cheered"),
        )
    };
    let dash = || Some("-".to_string());
    assert_eq!(ends(&status(&app)), (dash(), dash(), dash()), "no end");
    end_after(&mut app, &sid, 45 * SEC);
    let line = status(&app);
    let at_ms = app
        .pool
        .get(0)
        .expect("session 0")
        .ctx
        .timeline
        .lock()
        .expect("timeline")
        .task_face_at(TaskFace::Landed)
        .expect("the landing's instant");
    let at = Some((at_ms / 1000).to_string());
    let (zero, one) = (Some("0".to_string()), Some("1".to_string()));
    assert_eq!(
        ends(&line),
        (at.clone(), zero.clone(), zero.clone()),
        "{line}"
    );
    assert_eq!(field(&line, "task").as_deref(), Some("landed"), "{line}");
    assert_eq!(field(&line, "pause").as_deref(), Some("-"), "{line}");
    assert!(field(&line, "task_state").is_some(), "{line}");
    assert!(
        line.contains(" land_since=- end="),
        "right after `pause=` and Land's hold (slice 3c): {line}"
    );
    let serial = app.harness_glass[&0].end().expect("an end").serial;
    assert!(
        app.harness_glass
            .get_mut(&0)
            .expect("the glass")
            .cheer_end(serial)
    );
    assert_eq!(
        ends(&status(&app)),
        (at.clone(), zero.clone(), one.clone()),
        "cheered apart from seen"
    );
    assert!(look(&mut app, Some(0)).is_ok());
    app.switch_tab_in(WindowId(0), 0);
    assert_eq!(ends(&status(&app)), (at, one.clone(), one), "seen");
    let ctx = app.pool.get(0).expect("session 0").ctx.clone();
    assert!(
        crate::session_timeline::release_supervisor(
            &ctx,
            None,
            crate::metrics::now_us(),
            aterm_slog::Actor::Aterm,
        ),
        "PRECONDITION: this host's claim was live"
    );
    assert!(
        app.harness_glass[&0].end().is_some(),
        "PRECONDITION: the end still stands on the glass"
    );
    let line = status(&app);
    assert_eq!(field(&line, "task").as_deref(), Some("-"), "{line}");
    assert_eq!(
        ends(&line),
        (dash(), dash(), dash()),
        "no claim, no end told: {line}"
    );
    let other = app.next_session_id - 1;
    assert_eq!(
        ends(&app.session_status_record(other).expect("status")),
        (dash(), dash(), dash()),
        "no supervised agent, no face told"
    );
}

/// A CARRIED END'S CHEER IS SPENT (§11.2.3, item 8; §12.5.2): a seamless
/// update puts the outgoing process's end on the glass cheered, so no pet
/// here replays it, and `status` says `end_seen=0 end_cheered=1` for it
/// unseen: `end_cheered=1` is that the end's cheer is spent, by a pet's
/// frame or by the carry, not that a pet in this process cheered it.
#[test]
fn status_says_a_carried_ends_cheer_is_spent() {
    let (mut app, _sid, _host) = desk();
    assert!(
        app.harness_glass.entry(0).or_default().carry_end(
            crate::harness_glass::EndFace::Landed,
            None,
            false
        ),
        "PRECONDITION: carried where no end stands"
    );
    let line = app.session_status_record(0).expect("status");
    assert_eq!(field(&line, "end_seen").as_deref(), Some("0"), "{line}");
    assert_eq!(
        field(&line, "end_cheered").as_deref(),
        Some("1"),
        "spent: {line}"
    );
}
