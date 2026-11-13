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
use crate::autopilot_menu::{MenuKey, MenuOwner, MenuRow};
use crate::permission_box::{COST, Told, Would};
use crate::permission_click::{Clicker, Defaults};
use crate::{App, WindowId};
use aterm_agent::supervise::policy::claude_settings::{PermissionDefault, Resolved};

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

/// The settings file a scripted default names.
const SETTINGS: &str = "/etc/claude-code/managed-settings.json";

/// The settings' default `mode`, as [`SETTINGS`] names it.
fn named(mode: Mode) -> Resolved {
    Resolved::Mode {
        mode,
        source: SETTINGS.into(),
    }
}

/// Script the settings the click reads: their default `mode` named by [`SETTINGS`], or none
/// named anywhere, and bypass turned off by [`SETTINGS`] where `bypass_off`.
fn settings(app: &mut App, mode: Option<Mode>, bypass_off: bool) {
    app.permission_clicks
        .set_defaults(Defaults::new(move |_| PermissionDefault {
            mode: mode.map_or(Resolved::Unset, named),
            bypass_disabled: bypass_off.then(|| SETTINGS.into()),
        }));
}

/// Script the settings' default mode the click reads, bypass allowed.
fn default_mode(app: &mut App, mode: Option<Mode>) {
    settings(app, mode, false);
}

/// What the open permission box says ([`Told`]), or `None` where none is open.
fn box_told(app: &App) -> Option<Told> {
    app.autopilot_menu
        .as_ref()
        .filter(|menu| menu.owner == MenuOwner::Permission)
        .and_then(|menu| menu.told.as_ref())
        .map(|(told, _)| told.clone())
}

/// The open permission box's words, its header and sentences joined by a blank, or `None`
/// where no box of the permission chip is open.
fn box_words(app: &App) -> Option<String> {
    let menu = app
        .autopilot_menu
        .as_ref()
        .filter(|menu| menu.owner == MenuOwner::Permission)?;
    Some(
        menu.rows
            .iter()
            .filter_map(|row| match row {
                MenuRow::Header(text) => Some(text.as_str()),
                // A wrapped sentence's first row carries it whole, as a reader is told it.
                MenuRow::Note { spoken, .. } if !spoken.is_empty() => Some(spoken.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(" "),
    )
}

/// Whether every cell of the mode light is slanted: a standing click's face.
fn light_slanted(app: &App, wid: WindowId) -> bool {
    let (cells, _) = light_cells(app, wid);
    !cells.is_empty() && cells.iter().all(|&at| cell_at(app, wid, at).italic)
}

/// One frame with the open card laid on it, which closes a card whose anchor is gone.
fn lay(app: &mut App, wid: WindowId) {
    frame(app, wid);
    app.splice_autopilot_menu(wid);
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

/// THE CLICK CONFORMS TO ITS MODEL EACH WAY, ITS RESTART ITS ONE END, AND ITS BOX SAYS WHAT IT
/// DID: a click on the off chip (here `auto`) types nothing and stands an ask for the bypass
/// line (NEGATIVE CONTROLS: the retired toggle, which typed shift+tab and stood nothing; a click
/// on the off chip that asks the settings' line), and the chip's box opens on what yolo is and
/// what the switch costs; its lane's look that signalled keeps it standing, a click on it then
/// takes nothing back, and the look that found the restart landed ends it while the pill still
/// reads `auto` (a home that disables bypass; NEGATIVE CONTROL: an ask kept until the pill moves,
/// which would restart again). A click on the on chip stands an ask for the line with no
/// permission flag (NEGATIVE CONTROL: the retired one-way click, which asked bypass or stood
/// nothing), and its box names the mode the settings land it in and the file that says so.
#[test]
fn the_permission_click_conforms_to_its_model() {
    let model = aterm_models_gui::claude_permission_click_model();
    let (mut app, wid, session, mut reader, _host, script) = claude_app(120, true);
    let tab = sid_of(&app, session);
    let at = (wid, session, tab.as_str());
    draw_claude(&app, session, AUTO);
    frame(&mut app, wid);
    let mut state = model.init_state();
    let human = human_seq(&app, session);
    assert!(!light_slanted(&app, wid), "control: upright at rest");
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
        project_permission_click(&app, wid, session, &tab, &state)["to"],
        1,
        "CONTROL: the projection reads the ask's line off the App"
    );
    assert_eq!(
        standing(&app, session).map(|s| (s.from_yolo, s.click.to)),
        Some((false, PermissionTo::Bypass)),
        "auto is not yolo: a click asks bypass"
    );
    assert_eq!(
        box_told(&app),
        Some(Told::TurningOn),
        "the click opens the chip's box on what it did"
    );
    let said = box_words(&app).expect("the box");
    assert!(said.contains(COST), "a switch says its cost: {said:?}");
    assert!(!said.contains('\u{2026}'), "nothing cut: {said:?}");
    frame(&mut app, wid);
    assert_eq!(light_cells(&app, wid).1, "yolo", "the word keeps its cells");
    assert!(light_slanted(&app, wid), "a standing click's face");
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
    default_mode(&mut app, Some(Mode::Plan));
    draw_claude(&app, session, BYPASS);
    frame(&mut app, wid);
    let mut state = model.init_state();
    check_click_transition(&app, at, "ToYolo", &mut state, None);
    assert_eq!(light_cells(&app, wid).1, "yolo", "yolo on, at rest");
    hover_light(&mut app, wid);
    let human = human_seq(&app, session);
    drive_click_action("Click", || press_at(&mut app, wid, a_minute_ago()));
    check_click_transition(&app, at, "Click", &mut state, Some(&[("to", 1)][..]));
    assert_eq!(
        standing(&app, session).map(|s| (s.from_yolo, s.click.to)),
        Some((true, PermissionTo::Settings)),
        "a click on the on chip asks the settings' line"
    );
    assert!(pty(&mut reader, 0).is_empty(), "and types nothing");
    assert_eq!(human_seq(&app, session), human, "no person's input");
    assert_eq!(
        box_told(&app),
        Some(Told::Leaving(named(Mode::Plan))),
        "the box is on the mode the settings land it in"
    );
    let said = box_words(&app).expect("the box");
    assert!(said.contains(SETTINGS), "it names the file: {said:?}");
    frame(&mut app, wid);
    assert_eq!(
        light_cells(&app, wid).1,
        "yolo",
        "the same word, the same width"
    );
    assert!(light_slanted(&app, wid), "standing");
}

/// WHERE THE DEFAULT IS YOLO A CLICK ON THE ON CHIP CHANGES NOTHING AND SAYS SO: the settings
/// name bypass as their default, so a restart would land where it left; the model offers no
/// `Click` there, only `Explain`. The press stands nothing, types nothing and says no refusal;
/// the chip's box says the default here is yolo and names the settings file (NEGATIVE CONTROL:
/// an ask stood all the same). With no default named anywhere, the same press stands the ask
/// and the box says Claude Code picks its own and what that was measured as.
#[test]
fn a_click_on_the_on_chip_changes_nothing_where_the_default_is_yolo() {
    let model = aterm_models_gui::claude_permission_click_model();
    let (mut app, wid, session, mut reader, _host, _script) = claude_app(120, true);
    let tab = sid_of(&app, session);
    let at = (wid, session, tab.as_str());
    default_mode(&mut app, Some(Mode::Bypass));
    draw_claude(&app, session, BYPASS);
    frame(&mut app, wid);
    let mut state = model.init_state();
    check_click_transition(&app, at, "ToYolo", &mut state, None);
    check_click_transition(&app, at, "DefaultYolo", &mut state, None);
    assert!(
        model.successors("Click", &state).is_empty(),
        "the model offers no click where the default is yolo"
    );
    hover_light(&mut app, wid);
    let human = human_seq(&app, session);
    drive_click_action("Explain", || press_at(&mut app, wid, a_minute_ago()));
    check_click_transition(
        &app,
        at,
        "Explain",
        &mut state,
        Some(&[("stands", 1), ("lane", 1)][..]),
    );
    assert!(standing(&app, session).is_none(), "nothing stands");
    assert!(pty(&mut reader, 0).is_empty(), "and nothing is typed");
    assert_eq!(human_seq(&app, session), human, "no person's input");
    assert!(
        app.windows[&wid].claude_lights.sessions[&session]
            .refused
            .is_none(),
        "it is no refusal"
    );
    assert_eq!(
        box_told(&app),
        Some(Told::DefaultIsYolo(SETTINGS.into())),
        "the box says why"
    );
    let said = box_words(&app).expect("the box");
    for part in [SETTINGS, "permissions.defaultMode"] {
        assert!(said.contains(part), "{part:?} in {said:?}");
    }
    assert!(!said.contains(COST), "no switch, no cost: {said:?}");
    frame(&mut app, wid);
    assert!(!light_slanted(&app, wid), "still on, at rest");

    let (mut app, wid, session, _reader, _host, _script) = claude_app(120, true);
    default_mode(&mut app, None);
    draw_claude(&app, session, BYPASS);
    frame(&mut app, wid);
    hover_light(&mut app, wid);
    press_at(&mut app, wid, a_minute_ago());
    assert_eq!(
        standing(&app, session).map(|s| s.click.to),
        Some(PermissionTo::Settings),
        "control: no default named, the click stands"
    );
    assert_eq!(
        box_told(&app),
        Some(Told::Leaving(Resolved::Unset)),
        "the box is on Claude Code's own default"
    );
}

/// WHERE THE SETTINGS TURN BYPASS OFF A CLICK ON THE OFF CHIP CHANGES NOTHING AND SAYS SO:
/// Claude Code would not start on the bypass line, so the model offers no `Click` there, only
/// `Explain`. The press stands nothing, types nothing and says no refusal; the chip's box says
/// bypass is turned off and names the settings file (NEGATIVE CONTROL: an ask stood for the
/// bypass line all the same). CONTROL: with bypass allowed, the same press stands the ask.
#[test]
fn a_click_on_the_off_chip_changes_nothing_where_bypass_is_turned_off() {
    let model = aterm_models_gui::claude_permission_click_model();
    let (mut app, wid, session, mut reader, _host, _script) = claude_app(120, true);
    let tab = sid_of(&app, session);
    let at = (wid, session, tab.as_str());
    settings(&mut app, None, true);
    draw_claude(&app, session, AUTO);
    frame(&mut app, wid);
    let mut state = model.init_state();
    check_click_transition(&app, at, "BypassBarred", &mut state, None);
    assert!(
        model.successors("Click", &state).is_empty(),
        "the model offers no click onto a line the settings turn off"
    );
    hover_light(&mut app, wid);
    let human = human_seq(&app, session);
    drive_click_action("Explain", || press_at(&mut app, wid, a_minute_ago()));
    check_click_transition(
        &app,
        at,
        "Explain",
        &mut state,
        Some(&[("stands", 1), ("lane", 1)][..]),
    );
    assert!(standing(&app, session).is_none(), "nothing stands");
    assert!(pty(&mut reader, 0).is_empty(), "and nothing is typed");
    assert_eq!(human_seq(&app, session), human, "no person's input");
    assert!(
        app.windows[&wid].claude_lights.sessions[&session]
            .refused
            .is_none(),
        "it is no refusal"
    );
    assert_eq!(
        box_told(&app),
        Some(Told::BypassOff(SETTINGS.into())),
        "the box says why"
    );
    let said = box_words(&app).expect("the box");
    for part in [SETTINGS, "permissions.disableBypassPermissionsMode"] {
        assert!(said.contains(part), "{part:?} in {said:?}");
    }
    assert!(!said.contains(COST), "no switch, no cost: {said:?}");
    frame(&mut app, wid);
    assert!(!light_slanted(&app, wid), "still off, at rest");

    let (mut app, wid, session, _reader, _host, _script) = claude_app(120, true);
    settings(&mut app, None, false);
    draw_claude(&app, session, AUTO);
    frame(&mut app, wid);
    hover_light(&mut app, wid);
    press_at(&mut app, wid, a_minute_ago());
    assert_eq!(
        standing(&app, session).map(|s| s.click.to),
        Some(PermissionTo::Bypass),
        "CONTROL: bypass allowed, the click stands"
    );
}

/// THE BOX FOLLOWS ITS CLICK AND ITS PANE. A refusal ends the click, and the box that announced
/// its restart closes with it (NEGATIVE CONTROL: while the click only waits, the box stays). A
/// box opened without a click on a standing restart offers its take-back until the restart is
/// signalled, says the signal after, and closes when the restart lands. The chip dismisses a box
/// its click would only open again (the default here is yolo), and a later press opens it
/// again. A pane narrowed under an open box has its words wrapped again, none cut.
#[test]
fn the_box_follows_its_click_and_its_pane() {
    let first = a_minute_ago();
    let (mut app, wid, session, _reader, _host, script) = claude_app(120, true);
    let tab = sid_of(&app, session);
    draw_claude(&app, session, AUTO);
    frame(&mut app, wid);
    hover_light(&mut app, wid);
    press_at(&mut app, wid, first);
    lay(&mut app, wid);
    app.step_click(&tab);
    assert_eq!(
        box_told(&app),
        Some(Told::TurningOn),
        "CONTROL: a click that waits keeps its box"
    );
    script.then(Look::Refused(Refusal::Claimed));
    app.step_click(&tab);
    assert!(standing(&app, session).is_none(), "the refusal ended it");
    assert_eq!(box_told(&app), None, "and its box, which said a restart");

    frame(&mut app, wid);
    hover_light(&mut app, wid);
    press_at(&mut app, wid, first + interval() * 3);
    assert!(standing(&app, session).is_some(), "a click stands again");
    app.close_autopilot_menu();
    assert!(app.explain_permission(wid, session));
    assert_eq!(
        box_told(&app),
        Some(Told::Shown(Mode::Auto, Some(Would::TakeBack))),
        "before the signal a click takes it back"
    );
    script.then(Look::InFlight("signalled".into()));
    app.step_click(&tab);
    assert_eq!(
        box_told(&app),
        Some(Told::Shown(Mode::Auto, Some(Would::Signalled))),
        "after it, the box promises no take-back"
    );
    app.close_autopilot_menu();
    assert!(app.explain_permission(wid, session));
    assert_eq!(
        box_told(&app),
        Some(Told::Shown(Mode::Auto, Some(Would::Signalled))),
        "nor does one opened after the signal"
    );
    script.then(Look::Done);
    app.step_click(&tab);
    assert_eq!(box_told(&app), None, "the restart landed: the box is over");

    let (mut app, wid, session, _reader, _host, _script) = claude_app(120, true);
    default_mode(&mut app, Some(Mode::Bypass));
    draw_claude(&app, session, BYPASS);
    frame(&mut app, wid);
    hover_light(&mut app, wid);
    press_at(&mut app, wid, first);
    lay(&mut app, wid);
    assert_eq!(box_told(&app), Some(Told::DefaultIsYolo(SETTINGS.into())));
    hover_light(&mut app, wid);
    press_at(&mut app, wid, first + interval() * 3);
    assert_eq!(box_told(&app), None, "the chip dismisses its own box");
    assert!(standing(&app, session).is_none());
    press_at(&mut app, wid, first + interval() * 6);
    assert_eq!(
        box_told(&app),
        Some(Told::DefaultIsYolo(SETTINGS.into())),
        "CONTROL: the next press is a click, and opens it again"
    );

    let (mut app, wid, session, _reader, _host, _script) = claude_app(120, true);
    draw_claude(&app, session, AUTO);
    frame(&mut app, wid);
    hover_light(&mut app, wid);
    press_at(&mut app, wid, first);
    lay(&mut app, wid);
    let widest = |app: &App| {
        let menu = app.autopilot_menu.as_ref().expect("the box");
        menu.rows
            .iter()
            .filter_map(|row| match row {
                MenuRow::Note { text, .. } => Some(crate::tab_menu::display_cells(text)),
                _ => None,
            })
            .max()
            .expect("its sentences")
    };
    // A press on the card is the card's, where the card covers the chip too (a pane too
    // short to hang it clear of its anchor row): the standing click is not taken back.
    let chip = light_cells(&app, wid).0[0];
    {
        let menu = app.autopilot_menu.as_mut().expect("the box");
        let rect = menu.rect.expect("laid");
        menu.rect = Some(crate::tab_menu::MenuRect {
            row: chip.0 + 1 - rect.h,
            ..rect
        });
        assert!(menu.rect.is_some_and(|r| r.contains(chip.0, chip.1)));
    }
    hover_light(&mut app, wid);
    press_at(&mut app, wid, first + interval() * 3);
    assert!(
        standing(&app, session).is_some() && box_told(&app) == Some(Told::TurningOn),
        "the press was the card's: nothing taken back, the box open"
    );
    lay(&mut app, wid);
    assert!(
        !app.autopilot_menu
            .as_ref()
            .and_then(|m| m.rect)
            .expect("laid")
            .contains(chip.0, chip.1),
        "CONTROL: hung clear of its chip again"
    );
    hover_light(&mut app, wid);
    press_at(&mut app, wid, first + interval() * 6);
    assert!(
        standing(&app, session).is_none() && box_told(&app).is_none(),
        "CONTROL: beside the card the press is the chip's, and takes the click back"
    );
    hover_light(&mut app, wid);
    press_at(&mut app, wid, first + interval() * 9);
    lay(&mut app, wid);
    assert_eq!(
        box_told(&app),
        Some(Told::TurningOn),
        "a click stands again"
    );
    const NARROW: u16 = 34;
    let room = crate::autopilot_menu::content_room(usize::from(NARROW));
    assert!(
        widest(&app) > room,
        "precondition: wrapped for the wide pane"
    );
    assert!(app.apply_term_resize(wid, 24, NARROW));
    lay(&mut app, wid);
    assert_eq!(
        box_told(&app),
        Some(Told::TurningOn),
        "the box stays: its click stands"
    );
    assert!(widest(&app) <= room, "wrapped again to the narrow pane");
    let said = box_words(&app).expect("the box");
    assert!(
        said.contains(COST) && !said.contains('\u{2026}'),
        "every sentence whole: {said:?}"
    );
}

/// A SECONDARY PRESS OPENS THE BOX AND CLICKS NOTHING; ESC AND AN OUTSIDE PRESS CLOSE IT: a
/// right press on the chip opens the box on the mode its pill reads and what a click would do,
/// stands nothing and types nothing. Escape closes it; so does a press off the card, which is
/// the card's own and reaches no program. CONTROL: a plain press then stands the click.
#[test]
fn a_right_press_on_the_chip_opens_its_box_and_clicks_nothing() {
    use aterm_window::event::{ElementState, MouseButton};
    let (mut app, wid, session, mut reader, _host, _script) = claude_app(120, true);
    default_mode(&mut app, Some(Mode::AcceptEdits));
    for (pill, told) in [
        (PLAN, Told::Shown(Mode::Plan, Some(Would::TurnOn))),
        (
            BYPASS,
            Told::Shown(Mode::Bypass, Some(Would::Leave(named(Mode::AcceptEdits)))),
        ),
    ] {
        draw_claude(&app, session, pill);
        frame(&mut app, wid);
        hover_light(&mut app, wid);
        for state in [ElementState::Pressed, ElementState::Released] {
            app.on_mouse_input_at(wid, state, MouseButton::Right, a_minute_ago());
        }
        assert_eq!(box_told(&app), Some(told), "{pill}: the box");
        let lines = app.fact_menu_controls();
        assert!(
            lines[0].contains(" box=permission ") && !lines[0].contains(" fact="),
            "{pill}: a controller reads it as the permission box: {lines:?}"
        );
        assert!(
            lines[1..]
                .iter()
                .all(|l| l.starts_with("fact-menu label ") || l.starts_with("fact-menu rule ")),
            "{pill}: words alone, no row to choose: {lines:?}"
        );
        assert!(standing(&app, session).is_none(), "{pill}: no click made");
        assert!(pty(&mut reader, 0).is_empty(), "{pill}: nothing typed");
        lay(&mut app, wid);
        assert!(box_words(&app).is_some(), "{pill}: it stays over a frame");
        assert!(app.autopilot_menu_nav(wid, MenuKey::Close), "Escape");
        assert!(box_words(&app).is_none(), "{pill}: Escape closes it");
    }
    hover_light(&mut app, wid);
    for state in [ElementState::Pressed, ElementState::Released] {
        app.on_mouse_input_at(wid, state, MouseButton::Right, a_minute_ago());
    }
    lay(&mut app, wid);
    assert!(box_words(&app).is_some());
    let rect = app
        .autopilot_menu
        .as_ref()
        .and_then(|menu| menu.rect)
        .expect("the box is laid");
    let beside = if rect.col >= 2 {
        rect.col - 2
    } else {
        rect.col + rect.w + 2
    };
    let (x, y) = px_of(&app, wid, (rect.row, beside));
    app.on_cursor_moved(wid, x, y);
    press_at(&mut app, wid, a_minute_ago());
    assert!(box_words(&app).is_none(), "an outside press closes it");
    assert!(pty(&mut reader, 0).is_empty(), "and reaches no program");
    assert!(standing(&app, session).is_none());
    hover_light(&mut app, wid);
    press_at(&mut app, wid, Instant::now());
    assert!(
        standing(&app, session).is_some(),
        "CONTROL: a plain press on the chip stands its click"
    );
}

/// THE BOX STAYS WHILE ITS CLICK STANDS, ITS CHIP GONE WITH THE AGENT: between the signal and
/// the relaunch Claude Code's footer is off the screen, and the box, anchored at a chip no
/// longer drawn, stays open because its click stands. NEGATIVE CONTROL: once the restart has
/// landed and nothing stands, the same frame with no chip closes it.
#[test]
fn the_box_stays_through_the_relaunch_while_its_click_stands() {
    let (mut app, wid, session, _reader, _host, script) = claude_app(120, true);
    let tab = sid_of(&app, session);
    draw_claude(&app, session, AUTO);
    frame(&mut app, wid);
    hover_light(&mut app, wid);
    press_at(&mut app, wid, a_minute_ago());
    lay(&mut app, wid);
    assert!(box_words(&app).is_some(), "the click's box");
    script.then(Look::InFlight("signalled".into()));
    app.step_click(&tab);
    feed(&app, session, b"\x1b[2J\x1b[H$ ");
    {
        let ctx = app.pool.get(session).expect("session").ctx.clone();
        let mut timeline = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
        timeline.set_program(PGID, Some("zsh".into()));
    }
    lay(&mut app, wid);
    assert!(
        light_cells(&app, wid).0.is_empty(),
        "precondition: the chip is gone with the agent"
    );
    assert!(box_words(&app).is_some(), "the box stays: its click stands");
    script.then(Look::Done);
    app.step_click(&tab);
    assert!(standing(&app, session).is_none(), "the restart landed");
    lay(&mut app, wid);
    assert!(
        box_words(&app).is_none(),
        "control: nothing stands and no chip is drawn, so it closes"
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
    draw_claude(&app, session, AUTO);
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
    draw_claude(&app, session, AUTO);
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
    draw_claude(&app, session, AUTO);
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

/// A RUN OF CLICKS ON THE OFF CHIP STANDS ONE CLICK (review F1): the first press stands the ask
/// and opens the chip's box; the double click's second press, at the interval's last instant, is
/// the same gesture and takes nothing back, nor closes the box; a slow triple click's third
/// press, at the last instant of the interval after the second though past the first's, is the
/// same run and does nothing either. A press a millisecond after the interval of the run's last
/// press is a click of its own: it closes the box and is the take-back.
#[test]
fn a_run_of_clicks_on_the_off_chip_stands_one_click() {
    let model = aterm_models_gui::claude_permission_click_model();
    let (mut app, wid, session, mut reader, _host, _script) = claude_app(120, true);
    let tab = sid_of(&app, session);
    let at = (wid, session, tab.as_str());
    draw_claude(&app, session, AUTO);
    frame(&mut app, wid);
    let mut state = model.init_state();
    assert_eq!(light_cells(&app, wid).1, "yolo", "off at rest");
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
        assert!(box_words(&app).is_some(), "press {n} leaves the box open");
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
    assert!(box_words(&app).is_none(), "the take-back closes the box");
    frame(&mut app, wid);
    assert!(
        !light_slanted(&app, wid),
        "a click of its own takes it back"
    );
    assert!(pty(&mut reader, 0).is_empty());
}
