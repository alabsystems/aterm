// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The permission click (`docs/DESIGN-yolo-binary-2026-11-04.md` §2, §3) on the genuine App:
//! a click on the mode light types nothing and stands an ask in the App's table, which its lane's
//! looks move ([`crate::permission_click`]), checked against `ClaudePermissionClick`
//! (`aterm_models_gui::claude_permission_click_model`), each transition through its shipping
//! function, with the retired post-states as negative controls.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aterm_agent::harness::lights::{Light, Mode};
use aterm_agent::harness::permission_click::{End, Look, Refusal, Wait};
use aterm_agent::supervise::policy::turn_end::PermissionTo;
use aterm_spec::interp::State;

use super::gesture_tests::{
    AUTO, BYPASS, FIRST, Hosted, PGID, PLAN, a_minute_ago, app, cell_at, draw_claude, feed, frame,
    hit, host, human_seq, interval, press_at, pty, publish_facts, px_of, row_text, sid_of,
    snapshot, standing,
};
use super::{Chip, Refusal as Said};
use crate::permission_click::Clicker;
use crate::{App, WindowId};

/// The conversation another agent in the tab holds ([`FIRST`] left it).
const SECOND: &str = "0badf00d-2222-3333-4444-555566667777";

/// The looks a scripted lane answers, in order; past the last, the click waits on a turn.
#[derive(Clone, Default)]
struct Script(Arc<Mutex<VecDeque<Look>>>);

impl Script {
    fn then(&self, look: Look) {
        self.0.lock().unwrap().push_back(look);
    }

    fn clicker(&self) -> Clicker {
        let looks = Arc::clone(&self.0);
        Clicker::new(move |_, _, stands| {
            let look = looks
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(Look::Waits(Wait::Turn));
            // The harness reads the fence before the hand and just before the signal.
            if matches!(look, Look::InFlight(_)) && !stands() {
                return Look::Ended(End::TakenBack);
            }
            look
        })
    }
}

/// A hosted Claude Code tab `cols` wide whose conversation [`FIRST`] is read, its lane scripted
/// by the returned [`Script`]; `rewrite` claims it for this window's host, so the glass rewrites
/// its mode row.
fn claude_app(
    cols: u16,
    rewrite: bool,
) -> (
    App,
    WindowId,
    u64,
    std::os::unix::net::UnixStream,
    Hosted,
    Script,
) {
    let (mut app, wid, session, reader) = app();
    let hosted = host(&mut app);
    if cols != 80 {
        assert!(app.apply_term_resize(wid, 24, cols));
    }
    publish_facts(&app, session);
    let ctx = app.pool.get(session).expect("session").ctx.clone();
    {
        let mut timeline = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
        timeline.note_foreground_group(PGID);
        timeline.set_program(PGID, Some("claude".into()));
    }
    if rewrite {
        crate::session_timeline::claim_supervisor(
            &ctx,
            &crate::harness_host::holder(),
            None,
            None,
            crate::metrics::now_us(),
        )
        .expect("the claim");
    }
    let sid = sid_of(&app, session);
    crate::harness_host::publish_snapshot_for_test(&sid, Some(&snapshot(&sid, FIRST)));
    let script = Script::default();
    app.permission_clicks.set_clicker(script.clicker());
    (app, wid, session, reader, hosted, script)
}

/// The mode light's cells and what they read.
fn light_cells(app: &App, wid: WindowId) -> (Vec<(usize, usize)>, String) {
    let cells: Vec<(usize, usize)> = app.windows[&wid]
        .claude_lights
        .hits
        .iter()
        .filter(|h| h.chip == Some(Chip::Light(Light::Mode)))
        .map(|h| (h.frame_row, h.col))
        .collect();
    let text = cells.iter().map(|&at| cell_at(app, wid, at).ch).collect();
    (cells, text)
}

/// Put the pointer on the mode light.
fn hover_light(app: &mut App, wid: WindowId) {
    let (cells, _) = light_cells(app, wid);
    let (x, y) = px_of(app, wid, cells[0]);
    app.on_cursor_moved(wid, x, y);
}

/// The model's `ClaudePermissionClick` state read off the genuine App: the pill from the engine,
/// the ask and its lane from the App's table, the conversation from the published snapshot, the
/// tab from the store, and whether a refusal is said from the light's title slot. The person's
/// run of presses, the tab's holds and lines, and the ghosts come from `expected`.
pub(super) fn project_permission_click(
    app: &App,
    wid: WindowId,
    session: u64,
    tab: &str,
    expected: &State,
) -> State {
    let mut observed = expected.clone();
    if let Some(mode) = app.claude_screen_now(session).and_then(|s| s.mode) {
        observed.insert("bit", i64::from(mode == Mode::Bypass));
    }
    let click = app.permission_clicks.clicks.get(tab);
    observed.insert("stands", i64::from(click.is_some()));
    observed.insert(
        "lane",
        match &click {
            Some(s) if s.signalled => 2,
            Some(_) => 1,
            None => 0,
        },
    );
    let conv = |c: &str| if c == FIRST { 1 } else { 2 };
    if let Some(s) = &click {
        observed.insert("from", i64::from(s.from_yolo));
        observed.insert("to", i64::from(s.click.to == PermissionTo::Bypass));
        observed.insert("asked", conv(&s.click.conversation));
    }
    if let Some(talk) = crate::harness_host::published_snapshot(tab).and_then(|s| s.session) {
        observed.insert("conv", conv(&talk));
    }
    let open = app
        .store
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .by_sid(&aterm_session::SessionId::new(tab))
        .is_some();
    observed.insert("tab", i64::from(open));
    let now = Instant::now();
    let said = app.windows.get(&wid).is_some_and(|ws| {
        ws.claude_lights.sessions.get(&session).is_some_and(|s| {
            matches!(
                s.refused,
                Some((Light::Mode, Said::Click(_) | Said::Failed, until)) if until > now
            )
        })
    });
    observed.insert("said", i64::from(said));
    observed
}

fn drive_click_action<T>(action: &str, drive: impl FnOnce() -> T) -> T {
    use aterm_spec::xref;

    assert!(xref::reset_entered_anchors());
    let result = drive();
    let entered = xref::entered_anchor_ids();
    xref::disarm_entered_anchors();
    let anchor = xref::refinements()
        .find(|anchor| anchor.machine == "claude_permission_click" && anchor.action == action)
        .expect("the click's action has a shipping anchor");
    assert!(
        entered.contains(anchor.entry_id),
        "{action} missed its shipping function"
    );
    result
}

/// One step of `action` checked: the model fires it from `state`, and the App's projection must
/// be a post-state the model allows; `historical`, a retired post-state, must not be.
fn check_click_transition(
    app: &App,
    (wid, session, tab): (WindowId, u64, &str),
    action: &str,
    state: &mut State,
    historical: Option<&[(&'static str, i64)]>,
) {
    let model = aterm_models_gui::claude_permission_click_model();
    let before = state.clone();
    assert!(model.fire(action, state), "{action} is enabled");
    let observed = project_permission_click(app, wid, session, tab, state);
    let (ok, evidence) = aterm_spec::verify::validate_transition_tiered(
        &model,
        &[],
        &before,
        &observed,
        Some(action),
        "Claude permission click real conformance",
    );
    assert!(ok, "{action}: {evidence}");
    if let Some(changes) = historical {
        let mut old = observed.clone();
        for &(var, value) in changes {
            old.insert(var, value);
        }
        let (ok, _) = aterm_spec::verify::validate_transition_tiered(
            &model,
            &[],
            &before,
            &old,
            Some(action),
            "Claude permission click negative control",
        );
        assert!(
            !ok,
            "{action} must reject the retired post-state {changes:?}"
        );
    }
}

/// THE CLICK CONFORMS TO ITS MODEL, ITS RESTART ITS ONE END: a click on `yolo` types nothing and
/// stands an ask for the settings' line (NEGATIVE CONTROL: the retired toggle, which typed
/// shift+tab and stood nothing); its lane's look that signalled keeps it standing, a click on it
/// then takes nothing back, and the look that found the restart landed ends it while the pill
/// still reads bypass (a home whose settings choose it; NEGATIVE CONTROL: an ask kept until the
/// pill moves, which would restart again). A click on `lame` from auto asks bypass (NEGATIVE
/// CONTROL: the retired reading of auto as on, whose click asked the settings' line).
#[test]
fn the_permission_click_conforms_to_its_model() {
    let model = aterm_models_gui::claude_permission_click_model();
    let (mut app, wid, session, mut reader, _host, script) = claude_app(120, true);
    let tab = sid_of(&app, session);
    let at = (wid, session, tab.as_str());
    draw_claude(&app, session, BYPASS);
    frame(&mut app, wid);
    let mut state = model.init_state();
    let human = human_seq(&app, session);
    hover_light(&mut app, wid);
    drive_click_action("Click", || press_at(&mut app, wid, a_minute_ago()));
    assert!(pty(&mut reader, 0).is_empty(), "a click types nothing");
    assert_eq!(
        human_seq(&app, session),
        human,
        "and reaches no program as a person's input"
    );
    check_click_transition(
        &app,
        at,
        "Click",
        &mut state,
        Some(&[("stands", 0), ("lane", 0)][..]),
    );
    assert_eq!(
        standing(&app, session).map(|s| s.click.to),
        Some(PermissionTo::Settings)
    );
    frame(&mut app, wid);
    assert!(
        light_cells(&app, wid)
            .1
            .contains(crate::autopilot::IN_FLIGHT),
        "a standing click's face"
    );
    script.then(Look::InFlight("signalled".into()));
    drive_click_action("Signal", || app.step_click(&tab));
    check_click_transition(&app, at, "Signal", &mut state, None);
    app.click_permission(wid, session, Light::Mode);
    assert!(
        standing(&app, session).is_some_and(|s| s.signalled),
        "after the signal a click takes nothing back"
    );
    script.then(Look::Done);
    drive_click_action("Land", || app.step_click(&tab));
    check_click_transition(
        &app,
        at,
        "Land",
        &mut state,
        Some(&[("stands", 1), ("lane", 1)][..]),
    );
    assert!(pty(&mut reader, 0).is_empty(), "nothing typed, ever");

    let (mut app, wid, session, mut reader, _host, _script) = claude_app(120, true);
    let tab = sid_of(&app, session);
    let at = (wid, session, tab.as_str());
    draw_claude(&app, session, AUTO);
    frame(&mut app, wid);
    let mut state = model.init_state();
    check_click_transition(&app, at, "ToLame", &mut state, None);
    hover_light(&mut app, wid);
    drive_click_action("Click", || press_at(&mut app, wid, a_minute_ago()));
    assert!(pty(&mut reader, 0).is_empty());
    check_click_transition(&app, at, "Click", &mut state, Some(&[("to", 0)][..]));
    assert_eq!(
        standing(&app, session).map(|s| s.click.to),
        Some(PermissionTo::Bypass),
        "auto is not yolo: a click asks bypass"
    );
}

/// A CLICK ON A STANDING CLICK TAKES IT BACK, AND THE ENDS ARE ITS OWN: a click past the double
/// click's interval takes the ask back before its signal (NEGATIVE CONTROL: the ask kept); a
/// refusal ends it and says its plain reason (NEGATIVE CONTROL: a refusal ended in silence), which
/// rests; the conversation leaving the tab ends it (NEGATIVE CONTROL: kept for a conversation no
/// longer there); the tab closing ends it (NEGATIVE CONTROL: kept past its tab).
#[test]
fn the_permission_clicks_ends_conform_to_its_model() {
    let model = aterm_models_gui::claude_permission_click_model();
    let first = a_minute_ago();
    let (mut app, wid, session, mut reader, _host, script) = claude_app(120, true);
    let tab = sid_of(&app, session);
    let at = (wid, session, tab.as_str());
    draw_claude(&app, session, BYPASS);
    frame(&mut app, wid);
    let mut state = model.init_state();
    hover_light(&mut app, wid);
    drive_click_action("Click", || press_at(&mut app, wid, first));
    check_click_transition(&app, at, "Click", &mut state, None);
    frame(&mut app, wid);
    hover_light(&mut app, wid);
    check_click_transition(&app, at, "Apart", &mut state, None);
    drive_click_action("TakeBack", || {
        press_at(&mut app, wid, first + interval() + Duration::from_millis(1));
    });
    check_click_transition(
        &app,
        at,
        "TakeBack",
        &mut state,
        Some(&[("stands", 1), ("lane", 1)][..]),
    );
    assert!(pty(&mut reader, 0).is_empty());

    check_click_transition(&app, at, "Apart", &mut state, None);
    frame(&mut app, wid);
    hover_light(&mut app, wid);
    drive_click_action("Click", || {
        press_at(&mut app, wid, first + interval() * 3);
    });
    check_click_transition(&app, at, "Click", &mut state, None);
    check_click_transition(&app, at, "NoLine", &mut state, None);
    script.then(Look::Refused(Refusal::Claimed));
    drive_click_action("Refuse", || app.step_click(&tab));
    check_click_transition(&app, at, "Refuse", &mut state, Some(&[("said", 0)][..]));
    frame(&mut app, wid);
    let text = row_text(&app, wid, hit(&app, wid, Light::Mode).0);
    assert!(
        text.contains("another supervisor has this tab"),
        "the plain reason: {text:?}"
    );
    drive_click_action("Rest", || {
        let s = app
            .windows
            .get_mut(&wid)
            .and_then(|ws| ws.claude_lights.sessions.get_mut(&session))
            .expect("the session's lights");
        s.refused.as_mut().expect("the refusal said").2 = Instant::now();
        app.drain_claude_lights();
    });
    check_click_transition(&app, at, "Rest", &mut state, None);

    let (mut app, wid, session, _reader, _host, script) = claude_app(120, true);
    let tab = sid_of(&app, session);
    let at = (wid, session, tab.as_str());
    draw_claude(&app, session, BYPASS);
    frame(&mut app, wid);
    let mut state = model.init_state();
    hover_light(&mut app, wid);
    drive_click_action("Click", || press_at(&mut app, wid, first));
    check_click_transition(&app, at, "Click", &mut state, None);
    crate::harness_host::publish_snapshot_for_test(&tab, Some(&snapshot(&tab, SECOND)));
    script.then(Look::Ended(End::ConversationLeft));
    drive_click_action("Leave", || app.step_click(&tab));
    check_click_transition(
        &app,
        at,
        "Leave",
        &mut state,
        Some(&[("stands", 1), ("lane", 1)][..]),
    );

    let (mut app, wid, session, _reader, _host, _script) = claude_app(120, true);
    let tab = sid_of(&app, session);
    let at = (wid, session, tab.as_str());
    draw_claude(&app, session, BYPASS);
    frame(&mut app, wid);
    let mut state = model.init_state();
    hover_light(&mut app, wid);
    drive_click_action("Click", || press_at(&mut app, wid, first));
    check_click_transition(&app, at, "Click", &mut state, None);
    drive_click_action("CloseTab", || app.retire_session_registration(session));
    check_click_transition(
        &app,
        at,
        "CloseTab",
        &mut state,
        Some(&[("stands", 1), ("lane", 1)][..]),
    );
}

/// A CLICK IS NEVER DECIDED ON HALF A FRAME: while Claude Code's DEC 2026 frame is unfinished its
/// pill is not read, so a click stands nothing and says the mode is not on screen; once the frame
/// is whole, a click on its `auto` asks bypass. Nothing is typed either way.
#[test]
fn a_click_is_never_decided_on_half_a_frame() {
    let (mut app, wid, session, mut reader, _host, _script) = claude_app(80, false);
    draw_claude(&app, session, PLAN);
    frame(&mut app, wid);
    feed(
        &app,
        session,
        "\x1b[?2026h\x1b[22;1H\x1b[2K\u{276F} ".as_bytes(),
    );
    let term = app.pool.get(session).expect("session").term.clone();
    assert!(
        crate::term_lock(&term).frame_unfinished_at(Instant::now()),
        "precondition: the frame is unfinished"
    );
    app.click_permission(wid, session, Light::Mode);
    assert!(
        standing(&app, session).is_none(),
        "no click on half a frame"
    );
    assert!(
        matches!(
            app.windows[&wid].claude_lights.sessions[&session].refused,
            Some((Light::Mode, Said::NotShown, _))
        ),
        "refused as not on screen"
    );
    feed(
        &app,
        session,
        format!("\x1b[24;1H\x1b[2K{AUTO}\x1b[22;3H\x1b[?2026l").as_bytes(),
    );
    app.click_permission(wid, session, Light::Mode);
    assert_eq!(
        standing(&app, session).map(|s| (s.from_yolo, s.click.to)),
        Some((false, PermissionTo::Bypass)),
        "the whole frame: auto, and a click asks bypass"
    );
    assert!(pty(&mut reader, 0).is_empty(), "nothing typed");
}

/// A TAB WITH NO SNAPSHOT IS CLICKED ON THE CONVERSATION ITS FOOTER READ: the host publishes no
/// snapshot of an agent that started under Off, so the click keys on the conversation Claude
/// Code's registry names for the program in front, as its footer facts were read, and stands.
/// Measured on a real Claude Code: under Off such a tab refused every click as unread. NEGATIVE
/// CONTROL: with no footer owner either, the click is refused as unread and nothing stands.
#[test]
fn a_tab_with_no_snapshot_is_clicked_on_the_conversation_its_footer_read() {
    for owned in [true, false] {
        let (mut app, wid, session, mut reader, _host, _script) = claude_app(80, false);
        let tab = sid_of(&app, session);
        crate::harness_host::publish_snapshot_for_test(&tab, None);
        if owned {
            let entry = app.pool.get(session).expect("session");
            let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            let mut facts = timeline
                .claude_footer()
                .cloned()
                .expect("the footer's facts");
            facts.owner = Some(aterm_agent::harness::footer::FactsOwner {
                floor: None,
                session_id: FIRST.into(),
            });
            assert!(timeline.set_claude_footer(PGID, None, Some(facts)));
        }
        draw_claude(&app, session, AUTO);
        frame(&mut app, wid);
        app.click_permission(wid, session, Light::Mode);
        if owned {
            assert_eq!(
                standing(&app, session).map(|s| (s.click.conversation, s.click.to)),
                Some((FIRST.to_string(), PermissionTo::Bypass)),
                "the click stands on the footer's conversation"
            );
        } else {
            assert!(standing(&app, session).is_none(), "control: nothing stands");
            assert!(
                matches!(
                    app.windows[&wid].claude_lights.sessions[&session].refused,
                    Some((Light::Mode, Said::Unread, _))
                ),
                "control: refused as unread"
            );
        }
        assert!(pty(&mut reader, 0).is_empty(), "nothing typed");
    }
}

/// A RUN OF CLICKS ON YOLO STANDS ONE CLICK (review F1): the first press stands the ask; the
/// double click's second press, at the interval's last instant, is the same gesture and takes
/// nothing back; a slow triple click's third press, at the last instant of the interval after the
/// second though past the first's, is the same run and does nothing either. A press a millisecond
/// after the interval of the run's last press is a click of its own: the take-back.
#[test]
fn a_run_of_clicks_on_yolo_turns_it_over_once() {
    let model = aterm_models_gui::claude_permission_click_model();
    let (mut app, wid, session, mut reader, _host, _script) = claude_app(120, true);
    let tab = sid_of(&app, session);
    let at = (wid, session, tab.as_str());
    draw_claude(&app, session, BYPASS);
    frame(&mut app, wid);
    let mut state = model.init_state();
    assert_eq!(light_cells(&app, wid).1, "yolo", "yolo at rest");
    hover_light(&mut app, wid);
    let first = a_minute_ago();
    drive_click_action("Click", || press_at(&mut app, wid, first));
    check_click_transition(&app, at, "Click", &mut state, None);
    let id = standing(&app, session)
        .expect("the first press stands a click")
        .id;

    for (n, when) in [(2, first + interval()), (3, first + interval() * 2)] {
        frame(&mut app, wid);
        hover_light(&mut app, wid);
        press_at(&mut app, wid, when);
        assert!(pty(&mut reader, 0).is_empty(), "press {n} types nothing");
        assert_eq!(
            standing(&app, session).map(|s| s.id),
            Some(id),
            "press {n} is the run's: the click stands, not taken back nor stood again"
        );
        assert_eq!(
            project_permission_click(&app, wid, session, &tab, &state),
            state,
            "press {n}: the model's state, unmoved"
        );
    }

    check_click_transition(&app, at, "Apart", &mut state, None);
    frame(&mut app, wid);
    hover_light(&mut app, wid);
    drive_click_action("TakeBack", || {
        press_at(
            &mut app,
            wid,
            first + interval() * 3 + Duration::from_millis(1),
        );
    });
    check_click_transition(&app, at, "TakeBack", &mut state, None);
    frame(&mut app, wid);
    assert_eq!(
        light_cells(&app, wid).1,
        "yolo",
        "a click of its own takes it back"
    );
    assert!(pty(&mut reader, 0).is_empty());
}
