// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::io::{ErrorKind, Read};
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::time::{Duration, Instant};

use aterm_agent::harness::footer::FooterFacts;
use aterm_agent::harness::lights::Light;
use aterm_agent::harness::permission_click::{Look, Wait};
use aterm_agent::supervise::Autopilot;
use aterm_core::terminal::RenderCell;
use aterm_session::sink::SinkWriter;
use aterm_window::event::{ElementState, MouseButton};

use super::{Chip, Target};
use crate::autopilot::AutopilotView;
use crate::{App, WindowId, term_lock};

/// The mode light's chip.
pub(super) const MODE: Chip = Chip::Light(Light::Mode);

/// One synthetic key press and release, as a person taps it.
#[cfg(target_os = "macos")]
pub(super) fn tap(
    app: &mut App,
    wid: WindowId,
    named: aterm_window::keyboard::NamedKey,
    code: aterm_window::keyboard::KeyCode,
    text: &str,
) {
    use aterm_window::keyboard::{Key, KeyLocation, PhysicalKey, SmolStr};
    for state in [ElementState::Pressed, ElementState::Released] {
        app.on_key(
            wid,
            aterm_window::event::KeyEvent::synthetic_for_test(
                PhysicalKey::Code(code),
                Key::Named(named),
                Some(SmolStr::new(text)),
                KeyLocation::Standard,
                state,
                false,
            ),
        );
    }
}

pub(super) const BYPASS: &str = "  \u{23F5}\u{23F5} bypass permissions on (shift+tab to cycle)";
pub(super) const AUTO: &str = "  \u{23F5}\u{23F5} auto mode on (shift+tab to cycle)";
pub(super) const PLAN: &str = "  \u{23F8} plan mode on (shift+tab to cycle)";
const MANUAL: &str = "  \u{23F8} manual mode on \u{00B7} ? for shortcuts";
/// Claude Code's foreground process group, as the resolver names it.
pub(super) const PGID: i32 = 4242;

/// The conversation the stand-in Claude Code's published snapshot names ([`clickable`]).
pub(super) const FIRST: &str = "0badf00d-1111-2222-3333-444455556666";

/// `session`'s tab sid.
pub(super) fn sid_of(app: &App, session: u64) -> String {
    app.pool
        .get(session)
        .expect("session")
        .ctx
        .self_id
        .as_str()
        .to_owned()
}

/// A relaunch snapshot of the stand-in Claude Code in tab `sid`, on `conversation`.
pub(super) fn snapshot(sid: &str, conversation: &str) -> aterm_agent::harness::relaunch::Snapshot {
    aterm_agent::harness::relaunch::Snapshot {
        launch_env: None,
        auth_observed: true,
        route: aterm_agent::harness::accounts::AuthRoute::Outside,
        profile: None,
        tab: sid.to_owned(),
        pid: 4243,
        start: "Thu Sep 24 01:02:03 2026".to_owned(),
        shell: PGID.unsigned_abs(),
        program: std::path::PathBuf::from("/opt/claude"),
        argv: vec!["/opt/claude".to_owned()],
        session: Some(conversation.to_owned()),
        cwd: "/".to_owned(),
        version: Some("2.1.318".to_owned()),
        run: aterm_agent::harness::relaunch::AgentRun::Claude,
        dialect: Some(aterm_agent::harness::upgrade::Dialect::Zsh),
        login: None,
        home: None,
        claude_dir: None,
    }
}

/// Ready `session` for a permission click: its conversation [`FIRST`] read off a published
/// snapshot, and every look of its lane answering `look`.
pub(super) fn clickable(app: &mut App, session: u64, look: Look) {
    let sid = sid_of(app, session);
    crate::harness_host::publish_snapshot_for_test(&sid, Some(&snapshot(&sid, FIRST)));
    app.permission_clicks
        .set_clicker(crate::permission_click::Clicker::new(move |_, _, _| {
            look.clone()
        }));
}

/// `session`'s standing permission click.
pub(super) fn standing(app: &App, session: u64) -> Option<crate::permission_click::Standing> {
    app.permission_clicks.clicks.get(&sid_of(app, session))
}

/// Whether a press reached `session`'s permission light: a click stands, or a refusal shows.
pub(super) fn light_reached(app: &App, wid: WindowId, session: u64) -> bool {
    standing(app, session).is_some()
        || app.windows[&wid]
            .claude_lights
            .sessions
            .get(&session)
            .is_some_and(|s| s.refused.is_some())
}

/// A headless App whose one session writes its PTY input to a private socket.
pub(super) fn app() -> (App, WindowId, u64, UnixStream) {
    let (reader, writer) = UnixStream::pair().expect("private PTY observer");
    reader.set_nonblocking(true).expect("nonblocking observer");
    let app = App::headless_for_test_with_sink(Arc::new(SinkWriter::new_owned(writer.into())));
    let wid = WindowId(0);
    let session = app.focused_session_id(wid).expect("the front session");
    (app, wid, session, reader)
}

/// Inert harness host for test windows, shut down on drop.
pub(super) struct Hosted(crate::harness_host::HostHandle);

impl Drop for Hosted {
    fn drop(&mut self) {
        self.0.shutdown_and_join();
    }
}

/// Give `app` a harness host ([`Hosted`]).
pub(super) fn host(app: &mut App) -> Hosted {
    let host = crate::harness_host::HostHandle::for_supervised_test(
        Vec::new(),
        Arc::new(|_| crate::harness_host::BodyEnd::Stopped),
    );
    app.harness = Some(host.clone());
    Hosted(host)
}

/// `ctrl+shift+tab`, as a person presses it on macOS.
#[cfg(target_os = "macos")]
pub(super) fn chord(app: &mut App, wid: WindowId) {
    use aterm_window::keyboard::{KeyCode, ModifiersState, NamedKey};
    app.windows.get_mut(&wid).expect("window").mods =
        ModifiersState::CONTROL | ModifiersState::SHIFT;
    tap(app, wid, NamedKey::Tab, KeyCode::Tab, "\t");
    app.windows.get_mut(&wid).expect("window").mods = ModifiersState::empty();
}

/// [`app`] in a 120-column pane.
pub(super) fn wide_app() -> (App, WindowId, u64, UnixStream) {
    let (mut app, wid, session, reader) = app();
    assert!(app.apply_term_resize(wid, 24, 120));
    (app, wid, session, reader)
}

/// [`wide_app`] at 160 columns.
pub(super) fn wider_app() -> (App, WindowId, u64, UnixStream) {
    let (mut app, wid, session, reader) = app();
    assert!(app.apply_term_resize(wid, 24, 160));
    (app, wid, session, reader)
}

/// The pane's columns.
pub(super) fn cols(app: &App, session: u64) -> usize {
    let term = app.pool.get(session).expect("session").term.clone();
    usize::from(term_lock(&term).cols())
}

/// The whole facts, as [`publish_facts`] publishes them.
pub(super) const WHOLE_FACTS: &str =
    "\u{25C6} Opus 5.5 xhigh   \u{2302} ~/aterm   \u{2387} main \u{2500}";

/// Publish default Claude Code footer facts for `session`.
pub(super) fn publish_facts(app: &App, session: u64) {
    publish_facts_at(app, session, "~/aterm");
}

/// [`publish_facts`] with working directory `path`.
pub(super) fn publish_facts_at(app: &App, session: u64, path: &str) {
    let entry = app.pool.get(session).expect("session");
    let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
    timeline.note_foreground_group(PGID);
    assert!(timeline.set_claude_footer(
        PGID,
        None,
        Some(FooterFacts {
            model: Some("Opus 5.5".into()),
            effort: Some("xhigh".into()),
            path: Some(path.into()),
            branch: Some("main".into()),
            ..FooterFacts::default()
        }),
    ));
}

pub(super) fn feed(app: &App, session: u64, bytes: &[u8]) {
    let term = app.pool.get(session).expect("session").term.clone();
    term_lock(&term).process(bytes);
}

/// Draw Claude Code's bottom block on the 24-row engine with `mode` on the last row.
pub(super) fn draw_claude(app: &App, session: u64, mode: &str) {
    let rule = "\u{2500}".repeat(cols(app, session));
    let screen = format!(
        "\x1b[?2004h\x1b[2J\x1b[19;1H\u{25CF} done\x1b[21;1H{rule}\x1b[22;1H\u{276F} \
         \x1b[23;1H{rule}\x1b[24;1H{mode}\x1b[22;3H"
    );
    feed(app, session, screen.as_bytes());
}

/// The frame row of the pane's footer.
pub(super) fn footer_row(app: &App, wid: WindowId) -> usize {
    app.windows[&wid].input_scratch.cells.len() - usize::from(app.windows[&wid].rows) + 22
}

/// The frame row of Claude's own mode row.
fn mode_row(app: &App, wid: WindowId) -> usize {
    footer_row(app, wid) + 1
}

/// Redraw the mode row with `mode` and return the cursor to the caret.
pub(super) fn redraw_mode_row(app: &App, session: u64, mode: &str) {
    feed(
        app,
        session,
        format!("\x1b[24;1H\x1b[2K{mode}\x1b[22;3H").as_bytes(),
    );
}

/// Run one frame through the capture route's splices.
pub(super) fn frame(app: &mut App, wid: WindowId) {
    let prepared = app.prepare_terminal_capture_grid_with_cursor_fx_and_plan_outcome(
        wid,
        crate::app_render::ComposedCursorFxClock::Advance(Instant::now()),
    );
    let crate::app_render::CapturePreparation::Ready((grid, plan)) = prepared else {
        panic!("the headless capture must produce a frame");
    };
    app.splice_tab_strip(wid);
    app.splice_claude_footer(
        wid,
        &plan,
        crate::VisibleContentRoute::Terminal {
            composed: grid.composed,
        },
    );
}

/// Frame `(row, col)` of `chip`'s first painted cell.
pub(super) fn hit(app: &App, wid: WindowId, chip: impl Into<Chip>) -> (usize, usize) {
    let chip = chip.into();
    let h = app.windows[&wid]
        .claude_lights
        .hits
        .iter()
        .find(|h| h.chip == Some(chip))
        .unwrap_or_else(|| panic!("{chip:?} is painted"));
    (h.frame_row, h.col)
}

/// Whether the painter put `chip` on the glass this frame.
pub(super) fn painted(app: &App, wid: WindowId, chip: impl Into<Chip>) -> bool {
    let chip = chip.into();
    app.windows[&wid]
        .claude_lights
        .hits
        .iter()
        .any(|h| h.chip == Some(chip))
}

pub(super) fn cell_at(app: &App, wid: WindowId, (row, col): (usize, usize)) -> RenderCell {
    app.windows[&wid].input_scratch.cells[row][col]
}

/// The glass's tokens on `wid`'s one pane under `palette`, its chips' first cell at frame
/// column `col`.
fn chips_at(
    app: &App,
    wid: WindowId,
    palette: Option<aterm_effects::harness_card::CardPalette>,
    col: usize,
) -> super::ChipAt {
    let scratch = &app.windows[&wid].input_scratch;
    super::ChipAt {
        col,
        cols: scratch.cols,
        ..super::ChipAt::test(palette, scratch.implicit_blank)
    }
}

/// The lit ink of `wid`'s pane: a hovered or selected element's.
pub(super) fn lit_ink(app: &App, wid: WindowId) -> [u8; 3] {
    chips_at(app, wid, None, 0).lit()
}

/// The quiet ink of `wid`'s pane: the off chips' (`○`, `⊘ off`).
pub(super) fn quiet_ink(app: &App, wid: WindowId) -> [u8; 3] {
    chips_at(app, wid, None, 0).quiet()
}

/// The palette `session`'s glass wears: [`crate::autopilot::harness_theme`] of its reading,
/// none for a Cruise or Land this window's host holds no live claim on.
pub(super) fn worn(app: &App, session: u64) -> Option<aterm_effects::harness_card::CardPalette> {
    let (word, view, supervised) = app.session_autopilot(session);
    crate::autopilot::harness_theme(view, supervised, word, false)
}

/// The letter a chip's cell at frame cell `at` of `wid`'s one pane wears under the palette
/// `session`'s glass wears ([`worn`]): the lit ink where it wears none.
pub(super) fn letter_ink(
    app: &App,
    wid: WindowId,
    session: u64,
    (_, col): (usize, usize),
) -> [u8; 3] {
    chips_at(app, wid, worn(app, session), col).letter_at(0)
}

/// The row as it reads on the glass (the footer's facts read as set).
pub(super) fn row_text(app: &App, wid: WindowId, row: usize) -> String {
    crate::claude_footer::read_glass(&app.windows[&wid].input_scratch.cells[row])
}

/// The window pixel at the centre of frame cell `(row, col)`.
pub(super) fn px_of(app: &App, wid: WindowId, (row, col): (usize, usize)) -> (f64, f64) {
    let (cw, ch) = app.win_cell_size(wid);
    let (ox, oy) = app.frame_origin(wid);
    let x = ox as f64 + (app.win_pad(wid) + col * cw + cw / 2) as f64;
    let y = oy as f64 + (app.win_pad_top(wid) + app.win_head(wid) + row * ch + ch / 2) as f64;
    assert_eq!(
        app.frame_cell_at(wid, x, y),
        Some((row, col)),
        "the pixel names the cell"
    );
    (x, y)
}

/// Read up to `want` bytes (and any trailing available bytes) from the PTY observer.
pub(super) fn pty(reader: &mut UnixStream, want: usize) -> Vec<u8> {
    let mut got = Vec::new();
    let mut buf = [0u8; 256];
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => got.extend_from_slice(&buf[..n]),
            Err(e) if e.kind() == ErrorKind::WouldBlock => {
                if got.len() >= want || Instant::now() >= deadline {
                    break;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(e) => panic!("reading the PTY observer: {e}"),
        }
    }
    got
}

/// A double click's interval, whose last instant still continues a run of presses.
pub(super) fn interval() -> Duration {
    Duration::from_millis(
        u64::try_from(crate::multi_click_ms()).expect("a double click's interval"),
    )
}

/// Where a test's stated presses start: a minute ago, far enough from the clock that a press or
/// a stamp that read the clock instead of its stated time lands on the wrong side of a double
/// click's interval.
pub(super) fn a_minute_ago() -> Instant {
    Instant::now()
        .checked_sub(Duration::from_secs(60))
        .expect("a minute of uptime")
}

/// A left click at `when` where the pointer is.
pub(super) fn press_at(app: &mut App, wid: WindowId, when: Instant) {
    app.on_mouse_input_at(wid, ElementState::Pressed, MouseButton::Left, when);
    app.on_mouse_input_at(wid, ElementState::Released, MouseButton::Left, when);
}

#[test]
fn the_effort_tail_stays_on_claudes_row_and_every_chip_stands_clear_in_the_rule() {
    use aterm_phase::prompt::fixtures::{FOOTER_AUTO_EFFORT_HINT_MEASURED, screen};
    let measured = screen(FOOTER_AUTO_EFFORT_HINT_MEASURED)
        .into_iter()
        .find(|r| r.contains("auto mode on") && r.contains("/effort"))
        .expect("the measured mode row");
    let measured = measured.trim_end().to_string();
    for mode in [measured.as_str(), MANUAL] {
        let (mut app, wid, session, _reader) = app();
        let _host = host(&mut app);
        assert!(app.apply_term_resize(wid, 24, 144));
        publish_facts(&app, session);
        app.teams.insert(
            session,
            crate::team::Team {
                manager: Some(crate::team::Manager {
                    local: None,
                    label: std::sync::Arc::from("lab-sweep"),
                }),
                ..crate::team::Team::default()
            },
        );
        draw_claude(&app, session, mode);
        frame(&mut app, wid);
        let claude = row_text(&app, wid, mode_row(&app, wid));
        assert_eq!(claude.trim_end(), mode, "Claude's row is whole");
        if mode == measured {
            app.windows.get_mut(&wid).unwrap().claude_lights.selected =
                Some((session, Target::from(MODE)));
            frame(&mut app, wid);
        }
        let hits = &app.windows[&wid].claude_lights.hits;
        assert!(!hits.is_empty(), "{mode:?}: a chip is drawn");
        let rule = footer_row(&app, wid);
        let text: Vec<char> = app.windows[&wid].input_scratch.cells[rule]
            .iter()
            .map(|c| c.ch)
            .collect();
        assert!(text.contains(&'\u{2500}'), "control: the rule's glyphs");
        for chip in Chip::row() {
            let cols: Vec<usize> = hits
                .iter()
                .filter(|h| h.chip == Some(chip))
                .map(|h| {
                    assert_eq!(h.frame_row, rule, "the chip is in the rule");
                    h.col
                })
                .collect();
            let (Some(&first), Some(&last)) = (cols.iter().min(), cols.iter().max()) else {
                panic!("{mode:?}: {chip:?} is drawn");
            };
            let row: String = text.iter().collect();
            assert_eq!(text[first - 1], ' ', "{chip:?} before: {row:?}");
            assert_eq!(text[last + 1], ' ', "{chip:?} after: {row:?}");
        }
    }
}

/// AT REST THE RULE CARRIES THE FACTS AND THE HARNESS CHIPS ALONE: a hosted session's rule
/// draws Autopilot and the bit (`yolo`, on bypass and on auto alike) and no title, Claude's row
/// as drawn (no rewrite asked). CONTROL: an unhosted session's rule draws no chip.
#[test]
fn at_rest_the_rule_carries_the_facts_and_the_harness_chips_alone() {
    for hosted in [true, false] {
        for (mode, word) in [(BYPASS, "yolo"), (AUTO, "yolo")] {
            let (mut app, wid, session, _reader) = app();
            let _host = hosted.then(|| host(&mut app));
            publish_facts(&app, session);
            draw_claude(&app, session, mode);
            frame(&mut app, wid);
            let lights = &app.windows[&wid].claude_lights;
            let mut chips: Vec<Chip> = lights.hits.iter().filter_map(|h| h.chip).collect();
            chips.dedup();
            let want = if hosted {
                vec![Chip::Autopilot, MODE]
            } else {
                Vec::new()
            };
            assert_eq!(chips, want, "{mode:?} hosted={hosted}");
            assert_eq!(
                lights
                    .footers
                    .iter()
                    .map(|(s, _, _)| *s)
                    .collect::<Vec<_>>(),
                vec![session],
                "{mode:?}"
            );
            let text = row_text(&app, wid, footer_row(&app, wid));
            assert!(text.contains(WHOLE_FACTS), "{text:?}");
            for gone in [
                "\u{23F5}",
                "autopilot",
                "no prompts",
                "mode on",
                "permissions on",
                "click: ",
                "lame",
            ] {
                assert!(!text.contains(gone), "{gone:?} in {text:?}: no title");
            }
            assert_eq!(
                text.contains('\u{221e}'),
                hosted,
                "the harness's mark: {text:?}"
            );
            assert_eq!(text.contains(word), hosted, "the bit's word: {text:?}");
            if hosted {
                let at = hit(&app, wid, Chip::Autopilot);
                assert_eq!(
                    cell_at(&app, wid, at).fg,
                    letter_ink(&app, wid, session, at),
                    "cruise: its column's letter, under the palette its glass wears"
                );
            }
            let claude = row_text(&app, wid, mode_row(&app, wid));
            assert_eq!(claude.trim_end(), mode, "Claude's row, as drawn");
        }
    }
}

/// THE KEYBOARD WALKS THE HARNESS CHIPS AT REST (macOS): on a hosted session
/// `ctrl+shift+tab` walks the row in its order, Autopilot then the light (`yolo`, drawn at
/// rest), each selection painted on the glass and typing nothing; Return on the light stands a
/// click, typing nothing, the selection let go and the standing click's light drawn `◌`;
/// Autopilot stays. CONTROL: an unhosted session draws no chip, and the chord selects the
/// first fact on the glass.
#[cfg(target_os = "macos")]
#[test]
fn ctrl_shift_tab_walks_the_harness_chips_at_rest() {
    use aterm_window::keyboard::{KeyCode, NamedKey};

    {
        let (mut app, wid, session, _reader) = app();
        publish_facts(&app, session);
        draw_claude(&app, session, BYPASS);
        frame(&mut app, wid);
        assert!(
            app.windows[&wid].claude_lights.hits.is_empty(),
            "CONTROL: no host, no chip"
        );
        chord(&mut app, wid);
        let facts = app.windows[&wid].claude_lights.facts_on_glass(session);
        assert_eq!(
            app.windows[&wid].claude_lights.selected,
            facts.first().map(|fact| (session, Target::Fact(*fact))),
            "CONTROL: the walk starts at the facts"
        );
    }
    let (mut app, wid, session, mut reader) = app();
    let _host = host(&mut app);
    publish_facts(&app, session);
    draw_claude(&app, session, PLAN);
    frame(&mut app, wid);
    assert!(painted(&app, wid, MODE), "the bit, at rest");
    for want in [Chip::Autopilot, MODE] {
        chord(&mut app, wid);
        assert_eq!(
            app.windows[&wid].claude_lights.selected,
            Some((session, Target::from(want)))
        );
        frame(&mut app, wid);
        let at = hit(&app, wid, want);
        assert_eq!(
            cell_at(&app, wid, at).underline,
            super::UnderlineStyle::Single,
            "{want:?}: the selection is on the glass"
        );
    }
    assert!(pty(&mut reader, 0).is_empty(), "the chord is the lights'");
    let text = row_text(&app, wid, footer_row(&app, wid));
    assert!(text.contains("yolo"), "the bit: {text:?}");
    clickable(&mut app, session, Look::Waits(Wait::Turn));
    tap(&mut app, wid, NamedKey::Enter, KeyCode::Enter, "\r");
    assert!(
        pty(&mut reader, 0).is_empty(),
        "Return on the light stands a click, and is not typed"
    );
    assert!(standing(&app, session).is_some(), "a click stands");
    assert_eq!(app.windows[&wid].claude_lights.selected, None);
    frame(&mut app, wid);
    assert!(
        cell_at(&app, wid, hit(&app, wid, MODE)).italic,
        "the standing click's chip is drawn, its word slanted"
    );
    assert!(painted(&app, wid, Chip::Autopilot), "Autopilot stays");
}

/// The keyboard route (macOS): the chord walks a hosted pane's Autopilot chip, then its light,
/// and types nothing (again, it goes on to the facts and the switch the glass shows, and round
/// to the light); Return clicks it, typing neither the Return nor anything else, and lets go;
/// the click's restart landed, a Return at rest is the program's.
#[cfg(target_os = "macos")]
#[test]
fn ctrl_shift_tab_selects_a_light_and_return_clicks_it() {
    use aterm_window::keyboard::{KeyCode, ModifiersState, NamedKey};

    fn set_mods(app: &mut App, wid: WindowId, mods: ModifiersState) {
        app.windows.get_mut(&wid).expect("window").mods = mods;
    }

    let (mut app, wid, session, mut reader) = wide_app();
    let _host = host(&mut app);
    publish_facts(&app, session);
    draw_claude(&app, session, PLAN);
    frame(&mut app, wid);

    set_mods(
        &mut app,
        wid,
        ModifiersState::CONTROL | ModifiersState::SHIFT,
    );
    let walk = app.windows[&wid].claude_lights.revealable(session);
    assert_eq!(
        walk[..2],
        [Target::from(Chip::Autopilot), Target::from(MODE)],
        "the harness chips lead the walk"
    );
    assert!(
        walk.iter().any(|t| matches!(t, Target::Fact(_))),
        "the facts are on the glass"
    );
    for want in walk.iter().chain(&walk[..2]) {
        tap(&mut app, wid, NamedKey::Tab, KeyCode::Tab, "\t");
        assert_eq!(
            app.windows[&wid].claude_lights.selected,
            Some((session, *want)),
            "the walk goes on, and round to the light"
        );
    }
    assert!(pty(&mut reader, 0).is_empty(), "the chord is the lights'");
    frame(&mut app, wid);
    let auto = hit(&app, wid, Light::Mode);
    let text = row_text(&app, wid, auto.0);
    assert!(text.contains("click: yolo"), "{text:?}");
    assert_eq!(
        cell_at(&app, wid, auto).underline,
        super::UnderlineStyle::Single,
        "the selected light is marked"
    );

    clickable(&mut app, session, Look::Waits(Wait::Turn));
    set_mods(&mut app, wid, ModifiersState::empty());
    tap(&mut app, wid, NamedKey::Enter, KeyCode::Enter, "\r");
    assert!(
        pty(&mut reader, 0).is_empty(),
        "the click, and not the Return"
    );
    assert_eq!(
        standing(&app, session).map(|s| s.click.to),
        Some(aterm_agent::supervise::policy::turn_end::PermissionTo::Bypass),
        "plan is not yolo: the click asks bypass"
    );
    assert_eq!(app.windows[&wid].claude_lights.selected, None);
    frame(&mut app, wid);
    assert!(cell_at(&app, wid, hit(&app, wid, Light::Mode)).italic);
    let text = row_text(&app, wid, hit(&app, wid, Light::Mode).0);
    assert!(text.contains("restarts at turn end"), "{text:?}");

    let tab = sid_of(&app, session);
    let id = standing(&app, session).expect("standing").id;
    assert_eq!(
        app.permission_clicks
            .clicks
            .note(&tab, id, &Look::Done, Instant::now()),
        crate::permission_click::Noted::Changed
    );
    app.apply_click_look(&tab, Look::Done);
    redraw_mode_row(&app, session, BYPASS);
    frame(&mut app, wid);
    assert!(standing(&app, session).is_none(), "landed, it ends");
    assert!(
        app.autopilot_menu.is_none(),
        "the click's box said a restart that has landed: it closed with it"
    );
    tap(&mut app, wid, NamedKey::Enter, KeyCode::Enter, "\r");
    assert_eq!(pty(&mut reader, 1), b"\r");
}

#[test]
fn a_standing_click_keeps_its_lights_in_an_80_column_pane() {
    let (mut app, wid, session, mut reader) = app();
    let _host = host(&mut app);
    publish_facts(&app, session);
    draw_claude(&app, session, PLAN);
    frame(&mut app, wid);
    clickable(&mut app, session, Look::Waits(Wait::Turn));
    let approve = hit(&app, wid, Light::Mode);
    let (x, y) = px_of(&app, wid, approve);
    app.on_cursor_moved(wid, x, y);
    app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
    app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
    assert!(pty(&mut reader, 0).is_empty(), "a click types nothing");
    frame(&mut app, wid);
    assert!(
        !app.windows[&wid].claude_lights.hits.is_empty(),
        "the lights stay while the click stands: {:?}",
        row_text(&app, wid, approve.0)
    );
    let text = row_text(&app, wid, approve.0);
    assert!(
        text.contains(WHOLE_FACTS),
        "the facts stay whole while the click stands: {text:?}"
    );
    assert!(
        text.contains("restarts at turn end"),
        "the title's reason, beside the lights and the facts: {text:?}"
    );
    assert_eq!(
        hit(&app, wid, Light::Mode),
        approve,
        "the chip stays under the pointer while its title comes: {text:?}"
    );
}

/// A REFUSAL SAYS ITS REASON, NEVER IN THE MODEL'S ROOM: the lane's refusal of a click (an
/// unknown flag's spelling on its line) titles the light with the plain reason, which outranks
/// the facts in a narrow rule and stands beside them whole in a wide one.
#[test]
fn a_click_refused_says_the_reason_alone() {
    use aterm_agent::harness::permission_click::Refusal;
    for cols in [80_u16, 120] {
        let (mut app, wid, session, mut reader) = app();
        let _host = host(&mut app);
        if cols != 80 {
            assert!(app.apply_term_resize(wid, 24, cols));
        }
        publish_facts(&app, session);
        draw_claude(&app, session, MANUAL);
        frame(&mut app, wid);
        clickable(
            &mut app,
            session,
            Look::Refused(Refusal::Other("unknown-flag".into())),
        );
        click(&mut app, wid, Light::Mode);
        assert!(pty(&mut reader, 0).is_empty(), "{cols}: nothing typed");
        assert!(standing(&app, session).is_none(), "{cols}: it ends");
        frame(&mut app, wid);
        let chip = hit(&app, wid, Light::Mode);
        let text = row_text(&app, wid, chip.0);
        assert!(
            text.contains("its command line can't be rewritten"),
            "{cols}: the reason: {text:?}"
        );
        assert!(
            text.contains("\u{25C6} Opus 5.5 xhigh"),
            "{cols}: never the model's or the effort's room: {text:?}"
        );
        if cols != 80 {
            assert!(
                text.contains(WHOLE_FACTS),
                "{cols}: the reason beside the whole facts: {text:?}"
            );
        }
        assert!(text.contains("yolo"), "{cols}: control: the chip: {text:?}");
    }
}

/// A light selected with ctrl+shift+tab is let go by any mouse press, so clicking to
/// another pane and back leaves no selection armed to take the next Return: Return
/// submits Claude's prompt.
#[cfg(target_os = "macos")]
#[test]
fn a_mouse_press_lets_a_keyboard_selection_go() {
    use aterm_window::keyboard::{KeyCode, ModifiersState, NamedKey};
    fn click(app: &mut App, wid: WindowId, cell: (usize, usize)) {
        let (x, y) = px_of(app, wid, cell);
        app.on_cursor_moved(wid, x, y);
        app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
        app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
    }
    let (mut app, wid, session, mut reader) = app();
    let _host = host(&mut app);
    let other = app.split_active_stub_tab(wid);
    let (rows, cols) = {
        let terminal = term_lock(&app.pool.get(session).unwrap().term);
        (terminal.rows(), usize::from(terminal.cols()))
    };
    publish_facts(&app, session);
    let rule = "\u{2500}".repeat(cols);
    let (top_rule, prompt, bottom_rule) = (rows - 3, rows - 2, rows - 1);
    feed(&app, session, format!(
        "\x1b[2J\x1b[{top_rule};1H{rule}\x1b[{prompt};1H\u{276F} \x1b[{bottom_rule};1H{rule}\x1b[{rows};1H  \u{23F5}\u{23F5} bypass permissions on\x1b[{prompt};3H"
    ).as_bytes());
    frame(&mut app, wid);
    let strip = app.windows[&wid].input_scratch.cells.len() - usize::from(app.windows[&wid].rows);
    let other_col = app.windows[&wid].cols as usize - 3;
    click(&mut app, wid, (strip + 5, 3));
    assert_eq!(
        app.focused_session_id(wid),
        Some(session),
        "Claude's pane has the keyboard"
    );
    frame(&mut app, wid);
    app.windows.get_mut(&wid).unwrap().mods = ModifiersState::CONTROL | ModifiersState::SHIFT;
    tap(&mut app, wid, NamedKey::Tab, KeyCode::Tab, "\t");
    tap(&mut app, wid, NamedKey::Tab, KeyCode::Tab, "\t");
    app.windows.get_mut(&wid).unwrap().mods = ModifiersState::empty();
    assert_eq!(
        app.windows[&wid].claude_lights.selected,
        Some((session, Target::from(MODE)))
    );
    click(&mut app, wid, (strip + 5, other_col));
    assert_eq!(app.focused_session_id(wid), Some(other));
    click(&mut app, wid, (strip + 5, 3));
    assert_eq!(app.focused_session_id(wid), Some(session));
    assert!(pty(&mut reader, 0).is_empty());
    tap(&mut app, wid, NamedKey::Enter, KeyCode::Enter, "\r");
    assert_eq!(pty(&mut reader, 1), b"\r", "Return submits Claude's prompt");
}

fn click(app: &mut App, wid: WindowId, chip: impl Into<Chip>) {
    let (x, y) = px_of(app, wid, hit(app, wid, chip));
    app.on_cursor_moved(wid, x, y);
    app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
    app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
}

/// Draw Claude Code's bottom block `cols` wide.
#[cfg(target_os = "macos")]
fn draw_claude_cols(app: &App, session: u64, mode: &str, cols: usize) {
    let rule = "\u{2500}".repeat(cols);
    let screen = format!(
        "\x1b[?2004h\x1b[2J\x1b[19;1H\u{25CF} done\x1b[21;1H{rule}\x1b[22;1H\u{276F} \
         \x1b[23;1H{rule}\x1b[24;1H{mode}\x1b[22;3H"
    );
    feed(app, session, screen.as_bytes());
}

#[test]
fn frame_selection_eviction_reuses_the_visible_plan() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Horizontal);
    let plan = app.active_visible_leaf_plan(wid).unwrap();
    let session = app.focused_session_id(wid).unwrap();
    app.windows
        .get_mut(&wid)
        .unwrap()
        .claude_lights
        .select(Some((session, Target::Chip(Chip::Autopilot))));
    crate::reset_visible_leaf_plan_builds();
    app.evict_stale_glass_selection(wid, Some(&plan));
    assert_eq!(crate::visible_leaf_plan_builds(), 0);
    assert_eq!(app.windows[&wid].claude_lights.selected, None);
}

/// NO SELECTION NOBODY CAN SEE: the keyboard selects only a chip the pane has ROOM for. The
/// light keeps its place beside Autopilot while the two fit, and a rule too narrow for both
/// draws neither, its Return Claude's, not an unseen light's.
#[cfg(target_os = "macos")]
#[test]
fn the_keyboard_never_selects_a_chip_the_pane_has_no_room_for() {
    use aterm_window::keyboard::{KeyCode, NamedKey};
    const ACCEPT_SHORT: &str = "  \u{23F5}\u{23F5} accept edits on";
    let (mut app, wid, session, mut reader) = app();
    let _host = host(&mut app);
    publish_facts(&app, session);
    assert!(app.apply_term_resize(wid, 24, 40));
    draw_claude_cols(&app, session, ACCEPT_SHORT, 40);
    frame(&mut app, wid);
    let at_rest = hit(&app, wid, Light::Mode);
    chord(&mut app, wid);
    chord(&mut app, wid);
    assert_eq!(
        app.windows[&wid].claude_lights.selected,
        Some((session, Target::from(MODE)))
    );
    frame(&mut app, wid);
    let text = row_text(&app, wid, at_rest.0);
    assert!(text.contains("yolo"), "the chip stays: {text:?}");
    assert_eq!(hit(&app, wid, Light::Mode).0, at_rest.0);
    assert!(pty(&mut reader, 0).is_empty());
    let narrowest = u16::try_from(ACCEPT_SHORT.chars().count()).unwrap();
    let no_room = (narrowest..40)
        .rev()
        .find(|&cols| {
            assert!(app.apply_term_resize(wid, 24, cols));
            draw_claude_cols(&app, session, ACCEPT_SHORT, usize::from(cols));
            frame(&mut app, wid);
            app.windows[&wid].claude_lights.hits.is_empty()
        })
        .expect("a rule too narrow for the two chips");
    tap(&mut app, wid, NamedKey::Enter, KeyCode::Enter, "\r");
    assert_eq!(
        pty(&mut reader, 1),
        b"\r",
        "the Return is Claude's, not an unseen light's"
    );
    assert_eq!(app.windows[&wid].claude_lights.selected, None);
    chord(&mut app, wid);
    let facts = app.windows[&wid].claude_lights.facts_on_glass(session);
    assert_eq!(
        app.windows[&wid].claude_lights.selected,
        facts.first().map(|fact| (session, Target::Fact(*fact))),
        "control: no chip to select where none fits, only a fact on the glass"
    );
    assert!(
        pty(&mut reader, 0).is_empty(),
        "and the chord is still the lights': never Claude's shift+tab"
    );
    tap(&mut app, wid, NamedKey::Escape, KeyCode::Escape, "\u{1b}");
    assert_eq!(app.windows[&wid].claude_lights.selected, None);
    tap(&mut app, wid, NamedKey::Enter, KeyCode::Enter, "\r");
    assert_eq!(pty(&mut reader, 1), b"\r");

    for (cols, chips) in [
        (40_u16, &[Chip::Autopilot, MODE][..]),
        (no_room + 1, &[Chip::Autopilot, MODE][..]),
        (no_room, &[][..]),
    ] {
        assert!(app.apply_term_resize(wid, 24, cols));
        draw_claude_cols(&app, session, ACCEPT_SHORT, usize::from(cols));
        frame(&mut app, wid);
        let facts = app.windows[&wid].claude_lights.facts_on_glass(session);
        let switch = app.windows[&wid]
            .claude_lights
            .shows(session, Target::Switch);
        let mut walk: Vec<Target> = chips
            .iter()
            .map(|chip| Target::Chip(*chip))
            .chain(facts.into_iter().map(Target::Fact))
            .chain(switch.then_some(Target::Switch))
            .collect();
        walk.push(walk[0]);
        for want in walk {
            chord(&mut app, wid);
            assert_eq!(
                app.windows[&wid].claude_lights.selected,
                Some((session, want)),
                "{cols} columns"
            );
            frame(&mut app, wid);
            assert!(
                app.windows[&wid].claude_lights.shows(session, want),
                "{cols}: {want:?} on the glass"
            );
        }
        assert_eq!(painted(&app, wid, MODE), cols != no_room, "{cols}");
        assert_eq!(
            painted(&app, wid, Chip::Autopilot),
            painted(&app, wid, MODE),
            "{cols}: the light beside Autopilot, or neither"
        );
        tap(&mut app, wid, NamedKey::Escape, KeyCode::Escape, "\u{1b}");
        assert_eq!(app.windows[&wid].claude_lights.selected, None);
    }
    assert!(pty(&mut reader, 0).is_empty(), "the walk types nothing");
    tap(&mut app, wid, NamedKey::Enter, KeyCode::Enter, "\r");
    assert_eq!(pty(&mut reader, 1), b"\r", "the Return is Claude's");
}

/// THE HOVER SAYS WHAT A CLICK'S RESTART ENDS, READ PAST THE ⬆️'S GATE
/// (`docs/DESIGN-yolo-binary-2026-11-04.md` Appendix "Chip"): with no ⬆️ on show, a hover on
/// the light reads Claude's running jobs off its row under any pill (auto's left as Claude
/// draws it) and the light's title adds `· ends 1 shell`, off and on alike, since a click
/// restarts either way. CONTROL: at rest the frame reads no job words.
#[test]
fn the_hover_says_what_a_clicks_restart_ends() {
    for (mode, head, ends) in [
        (
            "  \u{23F5}\u{23F5} auto mode on \u{B7} 1 shell",
            "auto mode on \u{b7} click: yolo",
            true,
        ),
        (
            "  \u{23F5}\u{23F5} bypass permissions on \u{B7} 1 shell",
            "bypass permissions on \u{b7} click: settings default",
            true,
        ),
    ] {
        let (mut app, wid, session, _reader) = wide_app();
        let _host = host(&mut app);
        publish_facts(&app, session);
        draw_claude(&app, session, mode);
        frame(&mut app, wid);
        let jobs = |app: &App| {
            app.windows[&wid].claude_lights.sessions[&session]
                .jobs
                .clone()
        };
        assert_eq!(
            jobs(&app),
            "",
            "CONTROL: {mode:?}: at rest no job words are read"
        );
        let (x, y) = px_of(&app, wid, hit(&app, wid, MODE));
        app.on_cursor_moved(wid, x, y);
        frame(&mut app, wid);
        assert_eq!(jobs(&app), "1 shell", "{mode:?}");
        let text = row_text(&app, wid, footer_row(&app, wid));
        assert!(text.contains(head), "{mode:?}: {text:?}");
        assert_eq!(
            text.contains("\u{b7} ends 1 shell"),
            ends,
            "{mode:?}: {text:?}"
        );
    }
}

/// The session's own Autopilot word and the posture it is read under.
pub(super) fn autopilot(app: &App, session: u64) -> (Option<Autopilot>, AutopilotView) {
    let (word, view, _) = app.session_autopilot(session);
    (word, view)
}

/// How many person gestures reached `session` (`status human_seq=`).
pub(super) fn human_seq(app: &App, session: u64) -> u64 {
    app.pool
        .get(session)
        .expect("session")
        .ctx
        .human_input
        .seq()
}

/// The footer's repaint key for the window's visible plan.
pub(super) fn footer_fp(app: &App, wid: WindowId) -> u64 {
    let plan = app.active_visible_leaf_plan(wid).expect("a visible plan");
    app.claude_footer_fp(wid, &plan)
}

#[test]
fn a_click_on_autopilot_cycles_its_word_and_types_nothing() {
    let (mut app, wid, session, mut reader) = wider_app();
    let _host = host(&mut app);
    publish_facts(&app, session);
    draw_claude(&app, session, BYPASS);
    feed(&app, session, b"\x1b[?1000h\x1b[?1006h");
    frame(&mut app, wid);
    let quiet = quiet_ink(&app, wid);
    let human = human_seq(&app, session);
    assert_eq!(
        autopilot(&app, session),
        (None, AutopilotView::Cruise),
        "the table's default"
    );
    let chip = hit(&app, wid, Chip::Autopilot);
    assert_eq!(
        (cell_at(&app, wid, chip).ch, cell_at(&app, wid, chip).fg),
        ('\u{221e}', letter_ink(&app, wid, session, chip))
    );
    let (x, y) = px_of(&app, wid, chip);
    app.on_cursor_moved(wid, x, y);
    frame(&mut app, wid);
    let text = row_text(&app, wid, chip.0);
    assert!(
        text.contains(&AutopilotView::Cruise.title()),
        "the hover's title: {text:?}"
    );
    for (want, face, lettered) in [
        (Autopilot::Land, "\u{2913} land", true),
        (Autopilot::Off, "\u{25cb} off", false),
        (Autopilot::Cruise, "\u{221e} cruise", true),
    ] {
        let before = footer_fp(&app, wid);
        click(&mut app, wid, Chip::Autopilot);
        assert_eq!(autopilot(&app, session).0, Some(want), "the word");
        assert_ne!(footer_fp(&app, wid), before, "{want:?}: the click repaints");
        frame(&mut app, wid);
        let at = hit(&app, wid, Chip::Autopilot);
        let text = row_text(&app, wid, at.0);
        assert!(text.contains(face), "{want:?}: {text:?}");
        let ink = if lettered {
            letter_ink(&app, wid, session, at)
        } else {
            quiet
        };
        // The pointer is still on the chip it clicked: the one hover, whatever the mode.
        let lit = chips_at(&app, wid, worn(&app, session), at.1).lit();
        let hovered = cell_at(&app, wid, at);
        assert_eq!(
            (hovered.fg, hovered.underline, hovered.bold),
            (lit, super::UnderlineStyle::Single, false),
            "{want:?}: lit under the pointer"
        );
        // Off it, the mode's own ink.
        app.on_cursor_left(wid);
        frame(&mut app, wid);
        assert_eq!(cell_at(&app, wid, at).fg, ink, "{want:?}");
        assert_eq!(
            cell_at(&app, wid, at).underline,
            super::UnderlineStyle::None
        );
        let (x, y) = px_of(&app, wid, at);
        app.on_cursor_moved(wid, x, y);
        frame(&mut app, wid);
        assert!(
            app.windows[&wid].claude_lights.sessions[&session]
                .autopilot_pending
                .is_none(),
            "{want:?}: read back"
        );
        let settled = footer_fp(&app, wid);
        frame(&mut app, wid);
        assert_eq!(footer_fp(&app, wid), settled, "{want:?}: still at rest");
    }
    assert!(
        pty(&mut reader, 0).is_empty(),
        "nothing to the PTY: no key, no mouse report"
    );
    assert_eq!(
        human_seq(&app, session),
        human,
        "never a person's keystroke"
    );
    redraw_mode_row(&app, session, PLAN);
    frame(&mut app, wid);
    clickable(&mut app, session, Look::Waits(Wait::Turn));
    click(&mut app, wid, Light::Mode);
    assert!(pty(&mut reader, 0).is_empty(), "the light types nothing");
    assert!(
        standing(&app, session).is_some(),
        "the light stands a click"
    );
}

#[test]
fn two_quick_clicks_on_the_autopilot_chip_each_step() {
    let (mut app, wid, session, mut reader) = app();
    let _host = host(&mut app);
    publish_facts(&app, session);
    draw_claude(&app, session, BYPASS);
    feed(&app, session, b"\x1b[?1000h\x1b[?1006h");
    frame(&mut app, wid);
    let human = human_seq(&app, session);
    click(&mut app, wid, Chip::Autopilot);
    click(&mut app, wid, Chip::Autopilot);
    assert_eq!(
        autopilot(&app, session),
        (Some(Autopilot::Off), AutopilotView::Off),
        "both clicks stepped"
    );
    assert!(
        app.windows[&wid].claude_lights.sessions[&session]
            .autopilot_pending
            .is_none(),
        "nothing in flight"
    );
    frame(&mut app, wid);
    let at = hit(&app, wid, Chip::Autopilot);
    assert!(
        row_text(&app, wid, at.0).contains("\u{25cb} off"),
        "the chip names off"
    );
    assert!(pty(&mut reader, 0).is_empty(), "nothing to the PTY");
    assert_eq!(
        human_seq(&app, session),
        human,
        "never a person's keystroke"
    );
}

#[test]
fn a_click_between_two_chips_does_nothing_and_reaches_no_program() {
    use crate::native_settings::SettingsRoute;
    let (mut app, wid, session, mut reader) = app();
    let _host = host(&mut app);
    publish_facts(&app, session);
    draw_claude(&app, session, PLAN);
    feed(&app, session, b"\x1b[?1000h\x1b[?1006h");
    frame(&mut app, wid);
    let first = hit(&app, wid, Chip::Autopilot);
    let light = hit(&app, wid, MODE);
    assert_eq!(light, (first.0, first.1 + 2), "`∞`, a blank, the light");
    let press = |app: &mut App, cell: (usize, usize)| {
        let (x, y) = px_of(app, wid, cell);
        app.on_cursor_moved(wid, x, y);
        app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
        app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
    };
    press(&mut app, (light.0, light.1 - 1));
    assert_eq!(
        app.windows[&wid].claude_lights.hover, None,
        "the gap: no chip hovered"
    );
    assert!(pty(&mut reader, 0).is_empty(), "the gap: no mouse report");
    assert_eq!(autopilot(&app, session).0, None, "no word written");
    assert!(
        app.native_settings_route_view_target(SettingsRoute::Harness)
            .is_none(),
        "no Settings opened"
    );
    assert!(!light_reached(&app, wid, session), "no light clicked");
    press(&mut app, (first.0, first.1 - 1));
    assert!(
        pty(&mut reader, 1).starts_with(b"\x1b[<0;"),
        "the pane's cell is the program's"
    );
}

#[test]
fn the_pointer_is_a_hand_over_a_chip_and_the_arrow_between_two() {
    let (mut app, wid, session, mut reader) = app();
    let _host = host(&mut app);
    publish_facts(&app, session);
    draw_claude(&app, session, PLAN);
    frame(&mut app, wid);
    let first = hit(&app, wid, Chip::Autopilot);
    let light = hit(&app, wid, MODE);
    let gap = (light.0, light.1 - 1);
    let pane = (first.0, first.1 - 1);
    let cursor = |app: &App| {
        let ws = &app.windows[&wid];
        (ws.hover_pointer, ws.native_text_cursor)
    };
    let (hand, arrow, beam) = ((true, false), (false, false), (false, true));
    for (cell, want, what) in [
        (pane, beam, "control: the pane's cell"),
        (light, hand, "the light"),
        (gap, arrow, "the blank"),
        (first, hand, "Autopilot"),
        (gap, arrow, "the blank again"),
        (pane, beam, "control: the pane's cell again"),
    ] {
        let (x, y) = px_of(&app, wid, cell);
        app.on_cursor_moved(wid, x, y);
        assert_eq!(cursor(&app), want, "{what}: on motion");
        app.update_hover_cursor(wid);
        assert_eq!(cursor(&app), want, "{what}: re-resolved");
    }
    assert!(pty(&mut reader, 0).is_empty(), "the motion types nothing");
}

#[test]
fn a_modifier_under_an_open_card_lights_no_chip() {
    let (mut app, wid, session, _reader) = app();
    let _host = host(&mut app);
    publish_facts(&app, session);
    draw_claude(&app, session, PLAN);
    frame(&mut app, wid);
    let first = hit(&app, wid, Chip::Autopilot);
    let light = hit(&app, wid, MODE);
    let pane = (first.0, first.1 - 1);
    let cursor = |app: &App| {
        let ws = &app.windows[&wid];
        (ws.hover_pointer, ws.native_text_cursor)
    };
    let (x, y) = px_of(&app, wid, pane);
    app.on_cursor_moved(wid, x, y);
    assert_eq!(cursor(&app), (false, true));
    app.open_front_fact_menu(super::Fact::Model)
        .expect("the model is on the glass");
    let (lx, ly) = px_of(&app, wid, light);
    app.on_cursor_moved(wid, lx, ly);
    assert!(app.autopilot_menu.is_some(), "the motion leaves it open");
    let before = cursor(&app);
    app.update_hover_cursor(wid);
    assert_eq!(cursor(&app), before, "no chip's hand under the menu");
    assert!(app.windows[&wid].link_hover.is_none());
    assert!(app.close_autopilot_menu());
    app.update_hover_cursor(wid);
    assert_eq!(cursor(&app), (true, false), "control: the chip's hand");
}

#[test]
fn a_middle_or_right_press_on_the_chips_reaches_no_program() {
    use crate::native_settings::SettingsRoute;
    let (mut app, wid, session, mut reader) = app();
    let _host = host(&mut app);
    publish_facts(&app, session);
    draw_claude(&app, session, PLAN);
    feed(&app, session, b"\x1b[?1000h\x1b[?1006h");
    frame(&mut app, wid);
    let first = hit(&app, wid, Chip::Autopilot);
    let light = hit(&app, wid, MODE);
    let chips = [first, (light.0, light.1 - 1), light];
    let pane = (first.0, first.1 - 1);
    let press = |app: &mut App, cell: (usize, usize), button: MouseButton| {
        let (x, y) = px_of(app, wid, cell);
        app.on_cursor_moved(wid, x, y);
        app.on_mouse_input(wid, ElementState::Pressed, button);
        app.on_mouse_input(wid, ElementState::Released, button);
    };
    // A secondary press on the permission chip opens its box and clicks nothing; on the
    // Autopilot chip and the gap between the two it opens nothing.
    let boxed = |app: &mut App, cell: (usize, usize), secondary: bool, what: &str| {
        let owner = app.autopilot_menu.as_ref().map(|menu| menu.owner);
        let want =
            (secondary && cell == light).then_some(crate::autopilot_menu::MenuOwner::Permission);
        assert_eq!(
            owner, want,
            "{what} {cell:?}: the permission chip's box alone"
        );
        app.close_autopilot_menu();
    };
    for (button, code) in [(MouseButton::Middle, "1"), (MouseButton::Right, "2")] {
        for cell in chips {
            press(&mut app, cell, button);
            assert!(
                pty(&mut reader, 0).is_empty(),
                "{button:?} {cell:?}: no report"
            );
            boxed(&mut app, cell, button == MouseButton::Right, "press");
        }
        press(&mut app, pane, button);
        let got = String::from_utf8_lossy(&pty(&mut reader, 1)).into_owned();
        assert!(
            got.starts_with(&format!("\x1b[<{code};")),
            "control: {button:?} on the pane's cell is the program's: {got:?}"
        );
    }
    feed(&app, session, b"\x1b[?1000l\x1b[?1006l");
    app.config.right_click = Some("copy_paste".to_owned());
    crate::control::PBPASTE_STUB.with(|board| *board.borrow_mut() = Some("PROBE-CLIP".into()));
    for cell in chips {
        press(&mut app, cell, MouseButton::Right);
        assert!(pty(&mut reader, 0).is_empty(), "{cell:?}: no paste");
        boxed(&mut app, cell, true, "right press");
    }
    #[cfg(target_os = "macos")]
    {
        app.windows.get_mut(&wid).unwrap().mods = aterm_window::keyboard::ModifiersState::CONTROL;
        for cell in chips {
            press(&mut app, cell, MouseButton::Left);
            assert!(
                pty(&mut reader, 0).is_empty(),
                "control-click {cell:?}: no toggle or paste"
            );
            boxed(&mut app, cell, true, "control-click");
        }
        app.windows.get_mut(&wid).unwrap().mods = aterm_window::keyboard::ModifiersState::empty();
    }
    press(&mut app, pane, MouseButton::Right);
    let got = String::from_utf8_lossy(&pty(&mut reader, "PROBE-CLIP".len())).into_owned();
    crate::control::PBPASTE_STUB.with(|board| *board.borrow_mut() = None);
    assert!(
        got.contains("PROBE-CLIP"),
        "control: the pane's cell pastes: {got:?}"
    );
    assert_eq!(autopilot(&app, session).0, None, "no word written");
    assert!(
        app.native_settings_route_view_target(SettingsRoute::Harness)
            .is_none(),
        "no Settings opened"
    );
    assert!(!light_reached(&app, wid, session), "no light clicked");
    press(&mut app, first, MouseButton::Left);
    assert_eq!(
        autopilot(&app, session).0,
        Some(Autopilot::Land),
        "CONTROL: a plain click on the Autopilot chip is its three-way toggle"
    );
}

/// The Autopilot chip is a three-way toggle and has no menu: with it selected, a menu key (↓,
/// Shift+F10, the Menu key) opens no card and writes no word. CONTROL: Return on the selected
/// chip is its click.
#[cfg(target_os = "macos")]
#[test]
fn a_menu_key_on_the_selected_autopilot_chip_opens_nothing() {
    use aterm_window::keyboard::{KeyCode, ModifiersState, NamedKey};
    let (mut app, wid, session, _reader) = app();
    let _host = host(&mut app);
    publish_facts(&app, session);
    draw_claude(&app, session, PLAN);
    frame(&mut app, wid);
    for (named, code, mods) in [
        (
            NamedKey::ArrowDown,
            KeyCode::ArrowDown,
            ModifiersState::empty(),
        ),
        (NamedKey::F10, KeyCode::F10, ModifiersState::SHIFT),
        (
            NamedKey::ContextMenu,
            KeyCode::ContextMenu,
            ModifiersState::empty(),
        ),
    ] {
        chord(&mut app, wid);
        assert_eq!(
            app.windows[&wid].claude_lights.selected,
            Some((session, Target::from(Chip::Autopilot))),
            "{named:?}: the chip selected"
        );
        app.windows.get_mut(&wid).unwrap().mods = mods;
        tap(&mut app, wid, named, code, "");
        app.windows.get_mut(&wid).unwrap().mods = ModifiersState::empty();
        assert!(app.autopilot_menu.is_none(), "{named:?}: no card");
        assert_eq!(autopilot(&app, session).0, None, "{named:?}: no word");
        assert_eq!(
            app.windows[&wid].claude_lights.selected, None,
            "{named:?}: the selection ends"
        );
    }
    chord(&mut app, wid);
    tap(&mut app, wid, NamedKey::Enter, KeyCode::Enter, "\r");
    assert_eq!(
        autopilot(&app, session).0,
        Some(Autopilot::Land),
        "CONTROL: Return on the selected chip cycles it"
    );
}

#[test]
fn a_click_on_the_chips_commits_an_open_in_grid_rename() {
    use crate::app_rename::RenameSurface;
    use crate::app_search::SearchEdit;
    use crate::session_timeline::MetaField;
    let (mut app, wid, session, mut reader) = app();
    let _host = host(&mut app);
    app.tab_strip_rows = 1;
    publish_facts(&app, session);
    // A bypass row, the one the rule rewrites, so a claimed session's row has vacated cells.
    draw_claude(&app, session, BYPASS);
    frame(&mut app, wid);
    let tab = app.windows[&wid].tab_set.active_id().expect("Claude's tab");
    let pinned = |app: &App| {
        app.pool
            .get(session)
            .expect("session")
            .ctx
            .meta
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .presentation_value(MetaField::Title.wire_name())
    };
    let rename = |app: &mut App, name: &str| {
        assert!(app.begin_session_rename(wid, tab), "the field opens");
        assert_eq!(
            app.inline_rename_edit(wid).map(|edit| edit.surface),
            Some(RenameSurface::InGrid)
        );
        assert!(app.rename_field_edit(wid, SearchEdit::KillToStart));
        assert!(app.rename_field_edit(wid, SearchEdit::Insert(name.to_owned())));
    };
    rename(&mut app, "left open");
    assert!(app.open_harness_settings(wid, crate::harness_screen::HarnessFocus::Top));
    assert!(
        app.inline_rename_edit(wid).is_some(),
        "control: a tab switch settles nothing"
    );
    app.end_session_rename_editor(wid);
    assert_eq!(pinned(&app), None, "control: nothing committed");
    app.switch_tab_in(wid, 0);
    frame(&mut app, wid);
    let policy = |text: &str| {
        crate::harness_policy_cell::HarnessPolicyCell::new(crate::app_config::HarnessPolicy::read(
            text,
        ))
    };
    // The switch stands for an agent the timeline names.
    app.pool
        .get(session)
        .expect("session")
        .ctx
        .timeline
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .set_program(PGID, Some("claude".into()));
    app.harness_policy = policy("[harness]\nenabled = false\n");
    frame(&mut app, wid);
    rename(&mut app, "off switch");
    assert!(
        !painted(&app, wid, Chip::Autopilot),
        "under the switch off no Autopilot chip stands"
    );
    let off = app.windows[&wid]
        .claude_lights
        .border_toggles
        .first()
        .map(|b| (b.frame_row, b.cols.start + b.cols.len() / 2))
        .expect("the off switch");
    let (x, y) = px_of(&app, wid, off);
    app.on_cursor_moved(wid, x, y);
    app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
    app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
    assert!(
        app.inline_rename_edit(wid).is_none(),
        "the off switch: closed"
    );
    assert_eq!(pinned(&app).as_deref(), Some("off switch"), "and committed");
    assert_eq!(
        app.focused_session_id(wid),
        Some(session),
        "`○ off`: nothing came to the front"
    );
    assert_eq!(autopilot(&app, session).0, None, "`○ off`: no word written");
    app.harness_policy = policy("[harness]\n");
    app.switch_tab_in(wid, 0);
    frame(&mut app, wid);
    super::switch_tests::later(&mut app, wid, session);
    rename(&mut app, "autopilot");
    click(&mut app, wid, Chip::Autopilot);
    assert!(app.inline_rename_edit(wid).is_none(), "Autopilot: closed");
    assert_eq!(pinned(&app).as_deref(), Some("autopilot"));
    assert_eq!(
        autopilot(&app, session).0,
        Some(Autopilot::Land),
        "the click still steps the word"
    );
    app.pool
        .get(session)
        .expect("session")
        .ctx
        .timeline
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .set_program(PGID, Some("claude".into()));
    frame(&mut app, wid);
    rename(&mut app, "gap");
    let light = hit(&app, wid, MODE);
    let (x, y) = px_of(&app, wid, (light.0, light.1 - 1));
    app.on_cursor_moved(wid, x, y);
    app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
    app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
    assert!(app.inline_rename_edit(wid).is_none(), "the blank: closed");
    assert_eq!(pinned(&app).as_deref(), Some("gap"));
    let b = &app.windows[&wid].claude_lights.border_toggles[0];
    let label = (b.frame_row, b.label.as_ref().expect("top label").start);
    rename(&mut app, "label");
    let (x, y) = px_of(&app, wid, label);
    app.on_cursor_moved(wid, x, y);
    app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
    app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
    assert!(app.inline_rename_edit(wid).is_none(), "the label: closed");
    assert_eq!(pinned(&app).as_deref(), Some("label"));
    let ctx = app.pool.get(session).expect("session").ctx.clone();
    crate::session_timeline::claim_supervisor(
        &ctx,
        &crate::harness_host::holder(),
        None,
        None,
        crate::metrics::now_us(),
    )
    .expect("the claim");
    frame(&mut app, wid);
    rename(&mut app, "swallowed");
    let (x, y) = px_of(&app, wid, (light.0 + 1, 20));
    assert!(
        app.glass_swallows_at(wid, x, y),
        "control: vacated mode-row cell is swallowed"
    );
    let (strip_x, strip_y) = px_of(&app, wid, (0, 20));
    assert!(
        !app.glass_swallows_at(wid, strip_x, strip_y),
        "tab-strip row above the terminal grid is never swallowed"
    );
    app.on_cursor_moved(wid, x, y);
    app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
    app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
    assert!(
        app.inline_rename_edit(wid).is_none(),
        "the vacated mode row: closed"
    );
    assert_eq!(pinned(&app).as_deref(), Some("swallowed"));
    rename(&mut app, "right-swallowed");
    app.on_cursor_moved(wid, x, y);
    app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Right);
    app.on_mouse_input(wid, ElementState::Released, MouseButton::Right);
    assert!(
        app.inline_rename_edit(wid).is_none(),
        "right-click on vacated mode row: closed"
    );
    assert_eq!(pinned(&app).as_deref(), Some("right-swallowed"));
    rename(&mut app, "right-light");
    let (x, y) = px_of(&app, wid, hit(&app, wid, MODE));
    app.on_cursor_moved(wid, x, y);
    app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Right);
    app.on_mouse_input(wid, ElementState::Released, MouseButton::Right);
    assert!(
        app.inline_rename_edit(wid).is_none(),
        "right-click on mode light: closed"
    );
    assert_eq!(pinned(&app).as_deref(), Some("right-light"));
    assert!(
        pty(&mut reader, 0).is_empty(),
        "nothing reached the program"
    );
}

#[test]
fn the_hovered_title_is_whole_where_it_fits_and_short_where_not() {
    let whole = &AutopilotView::Cruise.title();
    let click = "autopilot: cruise \u{b7} click: land";
    let explained = u16::try_from(
        format!(
            " \u{2014} {}",
            crate::autopilot::mode_words(Autopilot::Cruise).chip
        )
        .chars()
        .count(),
    )
    .unwrap();
    let (mut app, wid, session, _reader) = app();
    let _host = host(&mut app);
    publish_facts(&app, session);
    let mut seen = Vec::new();
    for width in (60..=170u16).rev() {
        assert!(app.apply_term_resize(wid, 24, width));
        draw_claude(&app, session, BYPASS);
        app.hover_claude_chip(wid, None);
        frame(&mut app, wid);
        app.hover_claude_chip(wid, Some((session, Chip::Autopilot)));
        frame(&mut app, wid);
        let rule = row_text(&app, wid, footer_row(&app, wid));
        let form: u8 = if rule.contains(whole) {
            3
        } else if rule.contains(click) {
            assert!(!rule.contains("\u{2014}"), "{width}: {rule:?}");
            2
        } else if rule.contains("\u{221e} yolo  autopilot: cruise ") {
            assert!(
                !rule.contains("click") && !rule.contains("\u{2014}"),
                "{width}: the short form is the mode phrase alone: {rule:?}"
            );
            1
        } else {
            assert!(
                !rule.contains("autopilot"),
                "{width}: no cut title: {rule:?}"
            );
            0
        };
        if form > 0 {
            assert!(
                rule.contains(WHOLE_FACTS),
                "{width}: the facts stay whole beside a title: {rule:?}"
            );
        }
        if let Some(&(wider, last)) = seen.last() {
            assert!(
                form <= last,
                "{width} after {wider}: a title came back: {rule:?}"
            );
        }
        seen.push((width, form));
    }
    let narrowest = |form: u8| {
        seen.iter()
            .filter(|(_, f)| *f == form)
            .map(|(w, _)| *w)
            .min()
    };
    assert!(
        (0..=3).all(|form| narrowest(form).is_some()),
        "CONTROL: every form is met: {seen:?}"
    );
    let apart = |wider: u8, narrower: u8| {
        narrowest(narrower)
            .zip(narrowest(wider))
            .map(|(n, w)| w - n)
    };
    assert_eq!(
        apart(3, 2),
        Some(explained),
        "the click where the explanation's {explained} cells give way: {seen:?}"
    );
    eprintln!(
        "the whole title from {:?} columns, the click from {:?}, the short form from {:?}",
        narrowest(3),
        narrowest(2),
        narrowest(1)
    );
}

#[test]
fn an_off_session_still_draws_its_chip() {
    for own in [true, false] {
        let (mut app, wid, session, _reader) = app();
        let _host = host(&mut app);
        if own {
            assert!(app.set_session_autopilot(
                session,
                Some(Autopilot::Off),
                crate::autopilot::Surface::Chip
            ));
        } else {
            app.harness_policy = crate::harness_policy_cell::HarnessPolicyCell::new(
                crate::app_config::HarnessPolicy::read("[harness]\nautopilot = \"off\"\n"),
            );
        }
        assert_eq!(autopilot(&app, session).1, AutopilotView::Off);
        publish_facts(&app, session);
        draw_claude(&app, session, BYPASS);
        frame(&mut app, wid);
        let chip = hit(&app, wid, Chip::Autopilot);
        let text = row_text(&app, wid, chip.0);
        assert_eq!(
            cell_at(&app, wid, chip).ch,
            '\u{25cb}',
            "own={own}: {text:?}"
        );
        assert!(
            !text.contains("\u{25cb} off"),
            "own={own}: no click, no name: {text:?}"
        );
        assert_eq!(
            cell_at(&app, wid, chip).fg,
            quiet_ink(&app, wid),
            "own={own}: the quiet ink"
        );
    }
}

#[test]
fn a_narrow_rule_gives_the_chips_up_before_the_model() {
    /// What the rule showed at one width.
    #[derive(Debug)]
    struct Seen {
        cols: u16,
        /// The model's fact.
        model: bool,
        /// The path's or the branch's.
        rest: bool,
        /// The effort's, the path's and the branch's.
        facts: [bool; 3],
        /// Autopilot's chip and the light's.
        chips: (bool, bool),
        text: String,
    }
    // The light keeps its place beside Autopilot at every rung wherever the bit is read
    // (`docs/DESIGN-yolo-binary-2026-11-04.md` §2): the chips go together or not at all.
    const LADDER: [(bool, bool); 2] = [(true, true), (false, false)];
    const PLAN_SHORT: &str = "  \u{23F8} plan mode on";
    const ACCEPT_SHORT: &str = "  \u{23F5}\u{23F5} accept edits on";
    for pill in [None, Some(PLAN_SHORT), Some(ACCEPT_SHORT)] {
        let (mut app, wid, session, _reader) = app();
        let _host = host(&mut app);
        publish_facts(&app, session);
        let narrowest = pill.map_or(18, |p| p.chars().count().max(18));
        let mut seen = Vec::new();
        for cols in (u16::try_from(narrowest).unwrap()..=80).rev() {
            if cols != 80 {
                assert!(app.apply_term_resize(wid, 24, cols));
            }
            let rule = "\u{2500}".repeat(usize::from(cols));
            let pill_row = pill.map_or(String::new(), |p| format!("\x1b[24;1H{p}"));
            feed(
                &app,
                session,
                format!(
                    "\x1b[2J\x1b[19;1H\u{25CF} done\x1b[21;1H{rule}\x1b[22;1H\u{276F} \
                     \x1b[23;1H{rule}{pill_row}\x1b[22;3H"
                )
                .as_bytes(),
            );
            frame(&mut app, wid);
            let text = row_text(&app, wid, footer_row(&app, wid));
            let model = text.contains("\u{25C6} Opus 5.5");
            let facts = ["xhigh", "\u{2302}", "\u{2387}"].map(|fact| text.contains(fact));
            let rest = facts[1] || facts[2];
            let chips = (
                painted(&app, wid, Chip::Autopilot),
                painted(&app, wid, MODE),
            );
            if rest {
                assert_eq!(
                    chips,
                    (true, pill.is_some()),
                    "{pill:?} {cols}: a fact outlived a chip: {text:?}"
                );
            }
            seen.push(Seen {
                cols,
                model,
                rest,
                facts,
                chips,
                text,
            });
        }
        let widest = &seen[0];
        assert!(
            widest.cols == 80
                && widest.model
                && widest.facts == [true; 3]
                && widest.chips == (true, pill.is_some()),
            "{pill:?} 80: everything: {:?}",
            widest.text
        );
        let chipped: Vec<&Seen> = seen.iter().filter(|s| s.chips != (false, false)).collect();
        for (fact, name) in ["the effort", "the path", "the branch"].iter().enumerate() {
            assert!(
                chipped
                    .windows(2)
                    .all(|pair| pair[0].facts[fact] || !pair[1].facts[fact]),
                "{pill:?}: {name} came back beside the chips: {seen:#?}"
            );
            assert!(
                chipped.iter().any(|s| s.model && !s.facts[fact]),
                "{pill:?}: control, {name} given up beside the chips: {seen:#?}"
            );
        }
        let rungs: Vec<usize> = seen
            .iter()
            .filter(|s| s.model)
            .map(|s| {
                LADDER
                    .iter()
                    .position(|&(autopilot, light)| (autopilot, light && pill.is_some()) == s.chips)
                    .unwrap_or_else(|| panic!("{pill:?}: off the ladder: {s:?}"))
            })
            .collect();
        assert!(
            rungs.windows(2).all(|pair| pair[0] <= pair[1]),
            "{pill:?}: never back up the ladder: {seen:#?}"
        );
        let shows = |chips: (bool, bool)| seen.iter().any(|s| s.model && s.chips == chips);
        assert!(
            seen.iter()
                .any(|s| s.model && !s.rest && s.chips == widest.chips),
            "{pill:?}: every chip beside the model alone: {seen:#?}"
        );
        if pill.is_none() {
            assert!(
                shows((false, false)),
                "the chips given up, the model kept: {seen:#?}"
            );
        }
    }
}

/// RETURN ACTS ON A SELECTED HARNESS CHIP (macOS): `ctrl+shift+tab`
/// selects Autopilot, and Return steps its word — cruise to land — with
/// NOTHING sent to the PTY (the Return is the chip's, not Claude's) and
/// no person stamped; the selection lets go.
#[cfg(target_os = "macos")]
#[test]
fn return_on_a_selected_autopilot_chip_cycles_it() {
    use aterm_window::keyboard::{KeyCode, NamedKey};
    let (mut app, wid, session, mut reader) = app();
    let _host = host(&mut app);
    publish_facts(&app, session);
    draw_claude(&app, session, BYPASS);
    frame(&mut app, wid);
    let human = human_seq(&app, session);
    chord(&mut app, wid);
    assert_eq!(
        app.windows[&wid].claude_lights.selected,
        Some((session, Target::from(Chip::Autopilot)))
    );
    frame(&mut app, wid);
    tap(&mut app, wid, NamedKey::Enter, KeyCode::Enter, "\r");
    assert_eq!(autopilot(&app, session).0, Some(Autopilot::Land));
    assert!(pty(&mut reader, 0).is_empty(), "the Return is the chip's");
    assert_eq!(app.windows[&wid].claude_lights.selected, None);
    frame(&mut app, wid);
    let text = row_text(&app, wid, footer_row(&app, wid));
    assert!(
        text.contains("\u{2913} land"),
        "the press names it: {text:?}"
    );
    assert_eq!(human_seq(&app, session), human, "no person stamped");
    tap(&mut app, wid, NamedKey::Enter, KeyCode::Enter, "\r");
    assert_eq!(pty(&mut reader, 1), b"\r");
    let human = human_seq(&app, session);
    chord(&mut app, wid);
    assert_eq!(
        app.windows[&wid].claude_lights.selected,
        Some((session, Target::from(Chip::Autopilot)))
    );
    frame(&mut app, wid);
    tap(&mut app, wid, NamedKey::Space, KeyCode::Space, " ");
    assert_eq!(
        autopilot(&app, session).0,
        Some(Autopilot::Off),
        "land to off"
    );
    assert!(pty(&mut reader, 0).is_empty(), "the Space is the chip's");
    assert_eq!(human_seq(&app, session), human, "no person stamped");
}
