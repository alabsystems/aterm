// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The agents panel on the glass: what is lit and where, the gates, the
//! skips, the record the undo points take back, the cost at rest, and the
//! wakes — a real engine, real frames through the capture route's splices
//! (`claude_footer::probe`), the real key and splice.

use super::*;

use std::sync::Arc;
use std::time::{Duration, Instant};

use aterm_agent::supervise::Autopilot;
use aterm_window::event::{ElementState, MouseButton};

use crate::claude_footer::probe::{self, COLS, DONE, Live, ROWS};

/// Claude Code's foreground process group, as the resolver names it.
const PGID: i32 = 4242;
/// 2.1.304's panel grey (`ESC[38;2;153;153;153m`, measured).
const GREY: [u8; 3] = [153, 153, 153];
/// Claude's success green (a done agent's bullet).
const GREEN: [u8; 3] = [78, 186, 101];
/// Claude's blue: a name drawn in it is not Claude's plain text.
const BLUE: [u8; 3] = [51, 153, 255];
/// 2.1.304's footer while an agent runs.
const FOOTER: &str = "  \u{23F8} manual mode on \u{B7} ? for shortcuts \u{B7} \u{2193} to manage";

/// The pane's rows under its bottom rule: the footer, the container's
/// blank, then the panel's rows.
const UNDER: usize = 7;
/// Pane rows: the bottom rule, then the panel's main, agents, slot and
/// workflow.
const BOTTOM: usize = ROWS as usize - 1 - UNDER;
const MAIN: usize = BOTTOM + 3;
const FIRST: usize = BOTTOM + 4;
const SECOND: usize = BOTTOM + 5;
const WORKFLOW: usize = BOTTOM + 7;

/// `formatDuration`, as Claude draws an agent's elapsed time.
fn elapsed(s: u64) -> String {
    if s < 60 {
        format!("{s}s")
    } else {
        format!("{}m {}s", s / 60, s % 60)
    }
}

/// An agent row: the prefix, the bullet, the name, its activity, its status
/// right-aligned two cells short of the pane's edge.
fn agent(name: &str, activity: &str, secs: u64, tokens: &str) -> String {
    let head = format!("  \u{25EF} {name}  {activity}");
    let status = format!("{} \u{B7} \u{2193} {tokens} tokens", elapsed(secs));
    let pad = usize::from(COLS) - 2 - head.chars().count() - status.chars().count();
    format!("{head}{}{status}", " ".repeat(pad))
}

/// What the panel shows: two agents' clocks and tokens, the workflow's.
#[derive(Clone, Copy)]
struct Agents {
    first: (u64, &'static str),
    second: (u64, &'static str),
}

const START: Agents = Agents {
    first: (21, "6.2k"),
    second: (9, "1.1k"),
};

/// How each part of the panel is inked when [`feed`] paints it.
#[derive(Clone, Copy, Default)]
struct Inks {
    /// The second agent's bullet in Claude's success green: done.
    second_done: bool,
    /// The first agent's name in Claude's blue.
    first_blue: bool,
    /// Claude's `❯` on the first agent's row: the person points at it.
    first_pointed: bool,
}

fn under(agents: Agents) -> Vec<String> {
    vec![
        FOOTER.to_owned(),
        String::new(),
        "  \u{23FA} main".to_owned(),
        agent(
            "general-purpose",
            "bling probe",
            agents.first.0,
            agents.first.1,
        ),
        agent("Explore", "map the crate", agents.second.0, agents.second.1),
        " ".to_owned(),
        "  \u{25EF} sweep  \u{25B0}\u{25B0}\u{25B0}\u{25B0}\u{25B0}\u{25B0}\u{25B0}\u{25B0}\u{25B0}\u{25B1}\u{25B1}\u{25B1}  3/4 \u{B7} 12m05s \u{B7} \u{2193} 88.1k tokens".to_owned(),
    ]
}

fn sgr([r, g, b]: [u8; 3]) -> String {
    format!("\x1b[38;2;{r};{g};{b}m")
}

/// Paint the screen: the transcript and composer as the probe draws them
/// (its rules in Claude's gray), the main row bold in the default ink, the
/// agent and workflow rows in the panel grey, and the caret on the composer.
fn feed(app: &App, session: u64, agents: Agents, inks: Inks) {
    let mut under = under(agents);
    if inks.first_pointed {
        under[3] = under[3].replacen("  ", "\u{276F} ", 1);
    }
    let refs: Vec<&str> = under.iter().map(String::as_str).collect();
    let rows = probe::screen(&Live {
        status: DONE,
        draft: &[""],
        under: &refs,
    });
    let mut out = String::from("\x1b[2J");
    for (i, row) in rows.iter().enumerate() {
        let painted = if row.starts_with('\u{2500}') {
            format!("{}{row}", sgr([90, 90, 90]))
        } else if i == MAIN {
            format!("\x1b[1m{row}")
        } else if (FIRST..=WORKFLOW).contains(&i) && !row.trim().is_empty() {
            let (bullet, rest) = row.split_at(row.char_indices().nth(2).unwrap().0);
            let (dot, rest) = rest.split_at('\u{25EF}'.len_utf8());
            let dot_ink = if i == SECOND && inks.second_done {
                GREEN
            } else {
                GREY
            };
            let rest = if i == FIRST && inks.first_blue {
                let name_end = rest.find("  ").unwrap_or(rest.len());
                let name_end = rest[1..].find("  ").map_or(name_end, |e| e + 1);
                format!(
                    "{}{}{}{}",
                    sgr(BLUE),
                    &rest[..name_end],
                    sgr(GREY),
                    &rest[name_end..]
                )
            } else {
                rest.to_owned()
            };
            format!(
                "{}{bullet}{}{dot}{}{rest}",
                sgr(GREY),
                sgr(dot_ink),
                sgr(GREY)
            )
        } else {
            row.clone()
        };
        out.push_str(&format!("\x1b[{};1H{painted}\x1b[0m", i + 1));
    }
    // The caret on the composer's row, over the bottom rule.
    out.push_str(&format!("\x1b[{};3H", BOTTOM));
    probe::bytes(app, session, out.as_bytes());
}

/// The session is Claude Code's, its facts published — a card drawer of the
/// build [`card`] paints (`probe::drawer`), in front before anything is
/// painted, so the screen and the launch card it paints from here are its
/// own (card engine §3.8's gate: a cut over painted cells taints its mark).
fn claude(app: &App, session: u64) {
    probe::drawer(app, session, PGID, "claude", "2.1.283");
    let entry = app.pool.get(session).expect("session");
    let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
    timeline.note_foreground_group(PGID);
    timeline.set_program(PGID, Some("claude".into()));
    assert!(timeline.set_claude_footer(PGID, None, Some(probe::facts())));
}

/// A headless window `ROWS` × `COLS` on Claude Code with its agents panel,
/// the test clock at `t0`.
fn glass(t0: Instant) -> (App, WindowId, u64) {
    crate::claude_card::set_test_now(Some(t0));
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let session = app.focused_session_id(wid).expect("the front session");
    assert!(app.apply_term_resize(wid, ROWS, COLS));
    claude(&app, session);
    feed(&app, session, START, Inks::default());
    (app, wid, session)
}

/// One frame: extract, the strip, the footer's key and splice.
fn frame(app: &mut App, wid: WindowId) -> u64 {
    probe::refill(app, wid);
    probe::pass(app, wid)
}

fn strip_rows(app: &App, wid: WindowId) -> usize {
    app.windows[&wid].input_scratch.cells.len() - usize::from(app.windows[&wid].rows)
}

/// The ink on pane row `row`, as `(column, colour)`.
fn ink_on(app: &App, wid: WindowId, row: usize) -> Vec<(u16, [u8; 3])> {
    let frame_row = strip_rows(app, wid) + row;
    app.windows[&wid]
        .input_scratch
        .ink
        .iter()
        .filter(|c| usize::from(c.row) == frame_row)
        .map(|c| (c.col, c.color))
        .collect()
}

fn painted_ink(app: &App, wid: WindowId) -> usize {
    (MAIN..=WORKFLOW)
        .map(|row| ink_on(app, wid, row).len())
        .sum()
}

/// The panel's light on the glass: a dark ground's halos, a light ground's
/// veils.
fn light(app: &App, wid: WindowId) -> Vec<RainHalo> {
    let p = &app.windows[&wid].claude_card;
    let entry = p.panels().entries.iter().find(|e| e.shown);
    entry.map_or(Vec::new(), |e| e.halo.clone())
}

/// The terminal's own text, as `text`/`screen` read it.
fn engine_text(app: &App, session: u64) -> Vec<String> {
    let mut fresh = RenderInput::empty();
    let term = app.pool.get(session).unwrap().term.clone();
    crate::term_lock(&term).cell_frame_into(&mut fresh, usize::from(ROWS), usize::from(COLS));
    fresh
        .cells
        .iter()
        .map(|row| row.iter().map(|c| c.ch).collect::<String>())
        .collect()
}

fn col_of(row: &str, needle: &str) -> usize {
    row[..row.find(needle).unwrap()].chars().count()
}

/// THE PANEL WEARS THE RAINBOW, ON THE GLASS ONLY: the main row's name, each
/// running agent's name and dot, the workflow's name, bar and dot are inked
/// — each row in its own stripe — and each task's dot lit (additive halos on
/// the dark ground); the activity, the elapsed time and the tokens keep
/// Claude's ink, and so does the main row's dot. The terminal's text and the
/// frame's cells are Claude's.
#[test]
fn the_panel_wears_the_rainbow_on_the_glass_only() {
    let t0 = Instant::now();
    let (mut app, wid, session) = glass(t0);
    frame(&mut app, wid);
    let strip = strip_rows(&app, wid);
    let text = |row: usize| {
        app.windows[&wid].input_scratch.cells[strip + row]
            .iter()
            .map(|c| c.ch)
            .collect::<String>()
    };
    let main = ink_on(&app, wid, MAIN);
    assert_eq!(
        main.iter().map(|(c, _)| *c).collect::<Vec<_>>(),
        vec![4, 5, 6, 7],
        "the main row's name, and not its dot"
    );
    let first = ink_on(&app, wid, FIRST);
    let row = text(FIRST);
    let name = col_of(&row, "general-purpose");
    let activity = col_of(&row, "bling probe");
    assert!(first.iter().any(|(c, _)| usize::from(*c) == 2), "the dot");
    assert!(
        first
            .iter()
            .all(|(c, _)| usize::from(*c) == 2 || (name..name + 15).contains(&usize::from(*c))),
        "the dot and the name, never the activity or the status: {first:?}"
    );
    assert_eq!(first.len(), 16);
    assert!(activity > name + 15);
    // Each row in its own stripe: the rows' first letters differ.
    let second = ink_on(&app, wid, SECOND);
    let workflow = ink_on(&app, wid, WORKFLOW);
    let lead = |ink: &[(u16, [u8; 3])], col: usize| {
        ink.iter().find(|(c, _)| usize::from(*c) == col).unwrap().1
    };
    assert_ne!(lead(&main, 4), lead(&first, 4));
    assert_ne!(lead(&first, 4), lead(&second, 4));
    // The workflow: its name, its whole bar, its dot.
    let wrow = text(WORKFLOW);
    let bar = col_of(&wrow, "\u{25B0}");
    let cols: Vec<usize> = workflow.iter().map(|(c, _)| usize::from(*c)).collect();
    assert!(
        (bar..bar + 12).all(|c| cols.contains(&c)),
        "the bar: {cols:?}"
    );
    assert!(cols.contains(&2) && cols.contains(&4));
    assert!(!cols.iter().any(|&c| c >= bar + 12), "not its progress");
    // Lit: a halo about each task's bullet, none about main's.
    let halos = light(&app, wid);
    assert!(
        halos
            .iter()
            .all(|h| h.mode == aterm_core::render::HaloMode::Add),
        "a dark ground's light is added"
    );
    for row in [FIRST, SECOND, WORKFLOW] {
        assert!(
            halos.iter().any(|h| usize::from(h.row) == strip + row),
            "row {row} lit"
        );
    }
    assert!(!halos.iter().any(|h| usize::from(h.row) == strip + MAIN));
    // GLASS ONLY: the grid's text is the panel Claude drew, and so are the
    // frame's cells.
    let engine = engine_text(&app, session);
    for (row, drawn) in engine.iter().enumerate().take(WORKFLOW + 1).skip(MAIN) {
        assert_eq!(text(row).trim_end(), drawn.trim_end(), "row {row}");
    }
    let cells = |app: &App| app.windows[&wid].input_scratch.cells[strip + FIRST].clone();
    let lit = cells(&app);
    app.config.claude_footer = Some(crate::app_config::ClaudeFooterConfig {
        agents: Some(false),
        ..Default::default()
    });
    frame(&mut app, wid);
    assert_eq!(cells(&app), lit, "the cells are Claude's either way");
    assert_eq!(painted_ink(&app, wid), 0, "switched off: Claude's panel");
    crate::claude_card::set_test_now(None);
}

/// The record goes back at the undo points: a re-splice of an unrefilled
/// scratch never doubles the ink or the light, and the strip leaves the
/// streams without any of it.
#[test]
fn the_undo_points_take_the_panel_back() {
    let (mut app, wid, _) = glass(Instant::now());
    frame(&mut app, wid);
    let ws = &app.windows[&wid];
    let (ink, halos) = (ws.input_scratch.ink.len(), ws.input_scratch.glow_halo.len());
    assert!(painted_ink(&app, wid) > 0 && halos > 0);
    for _ in 0..3 {
        probe::pass(&mut app, wid);
        let ws = &app.windows[&wid];
        assert_eq!(ws.input_scratch.ink.len(), ink, "never doubled");
        assert_eq!(ws.input_scratch.glow_halo.len(), halos);
    }
    let ws = app.windows.get_mut(&wid).unwrap();
    crate::claude_card::strip(&mut ws.claude_card, &mut ws.input_scratch);
    assert_eq!(painted_ink(&app, wid), 0, "the ink taken back");
    let ws = &app.windows[&wid];
    let ours = light(&app, wid);
    assert!(
        !ws.input_scratch.glow_halo.iter().any(|h| ours.contains(h)),
        "the light taken back"
    );
    crate::claude_card::set_test_now(None);
}

/// This window's host, holding `session`'s live supervisor claim.
fn supervise(app: &mut App, session: u64) -> crate::harness_host::HostHandle {
    let host = crate::harness_host::HostHandle::for_supervised_test(
        Vec::new(),
        Arc::new(|_| crate::harness_host::BodyEnd::Stopped),
    );
    app.harness = Some(host.clone());
    let ctx = app.pool.get(session).expect("session").ctx.clone();
    crate::session_timeline::claim_supervisor(
        &ctx,
        &crate::harness_host::holder(),
        None,
        None,
        crate::metrics::now_us(),
    )
    .expect("the claim");
    host
}

fn release(app: &App, session: u64) {
    let ctx = app.pool.get(session).unwrap().ctx.clone();
    crate::session_timeline::release_supervisor(
        &ctx,
        None,
        crate::metrics::now_us(),
        aterm_slog::Actor::Ctl,
    );
}

/// THE GATE IS THE BORDER'S (`autopilot::harness_theme`): the unhosted
/// window's Cruise, a hosted Cruise and a hosted Off (in white, with no
/// claim) light the panel; the master switch off, Claude Code's own switch
/// off — hosted or not (the cross-track review of 2026-10-19: an unhosted
/// window keeps the switches) — the unhosted window's word Off,
/// `[claude_footer] agents = false` and another program leave it as Claude
/// drew it. And Off reads near-neutral.
#[test]
fn the_panel_is_lit_wherever_the_border_is_drawn() {
    type Case = (&'static str, bool, fn(&mut App, u64));
    let cases: [Case; 10] = [
        ("unhosted cruise", true, |_, _| {}),
        ("unhosted, master switch off", false, |app, _| {
            app.harness_policy = crate::harness_policy_cell::HarnessPolicyCell::new(
                crate::app_config::HarnessPolicy::read("[harness]\nenabled = false\n"),
            );
        }),
        ("unhosted, agent switch off", false, |app, _| {
            app.harness_policy = crate::harness_policy_cell::HarnessPolicyCell::new(
                crate::app_config::HarnessPolicy::read(
                    "[harness]\nheadless = true\n[harness.claude]\nenabled = false\n",
                ),
            );
        }),
        ("hosted cruise", true, |app, session| {
            std::mem::forget(supervise(app, session));
        }),
        ("hosted off: white", true, |app, session| {
            std::mem::forget(supervise(app, session));
            app.set_session_autopilot(
                session,
                Some(Autopilot::Off),
                crate::autopilot::Surface::Chip,
            );
            release(app, session);
        }),
        ("unhosted word off", false, |app, session| {
            app.set_session_autopilot(
                session,
                Some(Autopilot::Off),
                crate::autopilot::Surface::Chip,
            );
        }),
        ("master switch off", false, |app, session| {
            std::mem::forget(supervise(app, session));
            app.harness_policy = crate::harness_policy_cell::HarnessPolicyCell::new(
                crate::app_config::HarnessPolicy::read("[harness]\nenabled = false\n"),
            );
        }),
        ("agent switch off", false, |app, session| {
            std::mem::forget(supervise(app, session));
            app.harness_policy = crate::harness_policy_cell::HarnessPolicyCell::new(
                crate::app_config::HarnessPolicy::read(
                    "[harness]\nheadless = true\n[harness.claude]\nenabled = false\n",
                ),
            );
            app.set_session_autopilot(
                session,
                Some(Autopilot::Off),
                crate::autopilot::Surface::Chip,
            );
        }),
        ("agents = false", false, |app, _| {
            app.config.claude_footer = Some(crate::app_config::ClaudeFooterConfig {
                agents: Some(false),
                ..Default::default()
            });
        }),
        ("another program", false, |app, session| {
            let entry = app.pool.get(session).unwrap();
            let mut timeline = entry.ctx.timeline.lock().unwrap();
            timeline.set_program(PGID, Some("codex".into()));
        }),
    ];
    for (what, lit, case) in cases {
        let (mut app, wid, session) = glass(Instant::now());
        case(&mut app, session);
        frame(&mut app, wid);
        let ink = painted_ink(&app, wid);
        assert_eq!(ink > 0, lit, "{what}: {ink} cells inked");
        if what.contains("white") {
            let (word, view, supervised) = app.session_autopilot(session);
            assert_eq!(
                crate::autopilot::harness_theme(
                    view,
                    supervised,
                    word,
                    app.unhosted_glass(Some(aterm_phase::Program::Claude))
                ),
                Some(CardPalette::Off)
            );
            for (_, [r, g, b]) in ink_on(&app, wid, FIRST) {
                let (hi, lo) = (r.max(g).max(b), r.min(g).min(b));
                assert!(hi - lo <= 40, "Off is near-neutral: {r},{g},{b}");
            }
        }
        if let Some(host) = app.harness.take() {
            host.shutdown_and_join();
        }
    }
    crate::claude_card::set_test_now(None);
}

/// AS DRAWN, OR NOT AT ALL: a name Claude drew in a colour keeps it (its dot
/// still lit); an agent Claude marked done keeps its green dot, no light, and
/// its name; the cursor's row and a selection in the window keep Claude's
/// panel.
#[test]
fn coloured_cells_the_cursors_row_and_a_selection_keep_claudes_look() {
    let (mut app, wid, session) = glass(Instant::now());
    feed(
        &app,
        session,
        START,
        Inks {
            second_done: true,
            first_blue: true,
            first_pointed: false,
        },
    );
    frame(&mut app, wid);
    let first = ink_on(&app, wid, FIRST);
    assert_eq!(
        first.iter().map(|(c, _)| *c).collect::<Vec<_>>(),
        vec![2],
        "a blue name keeps its blue; the dot is lit"
    );
    assert!(ink_on(&app, wid, SECOND).is_empty(), "done: Claude's look");
    let strip = strip_rows(&app, wid);
    let halos = light(&app, wid);
    assert!(!halos.iter().any(|h| usize::from(h.row) == strip + SECOND));
    assert!(halos.iter().any(|h| usize::from(h.row) == strip + FIRST));
    // The cursor on the first agent's row.
    feed(&app, session, START, Inks::default());
    probe::bytes(&app, session, format!("\x1b[{};40H", FIRST + 1).as_bytes());
    frame(&mut app, wid);
    assert!(ink_on(&app, wid, FIRST).is_empty(), "the cursor's row");
    assert!(!ink_on(&app, wid, SECOND).is_empty(), "the others lit");
    // A selection anywhere in the window.
    probe::bytes(&app, session, format!("\x1b[{};3H", BOTTOM).as_bytes());
    frame(&mut app, wid);
    assert!(painted_ink(&app, wid) > 0, "CONTROL: lit again");
    let px = |app: &App, (row, col): (usize, usize)| {
        let (cw, ch) = app.win_cell_size(wid);
        let (ox, oy) = app.frame_origin(wid);
        (
            ox as f64 + (app.win_pad(wid) + col * cw + cw / 2) as f64,
            oy as f64 + (app.win_pad_top(wid) + app.win_head(wid) + row * ch + ch / 2) as f64,
        )
    };
    let (x, y) = px(&app, (strip + 3, 0));
    app.on_cursor_moved(wid, x, y);
    app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
    let (x, y) = px(&app, (strip + 3, 8));
    app.on_cursor_moved(wid, x, y);
    app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
    frame(&mut app, wid);
    assert!(app.windows[&wid].input_scratch.any_selection());
    assert_eq!(painted_ink(&app, wid), 0, "a selection: Claude's panel");
    crate::claude_card::set_test_now(None);
}

/// A LIGHT GROUND IS PRINTED, NOT LIT: its dots wear round source-over veils
/// (a tint and the hollow's fill) on the glass over the glyph, no additive
/// light and nothing under the glyph; SERIOUS MODE keeps the state ink and
/// drops every light.
#[test]
fn a_light_ground_wears_round_veils_and_serious_mode_drops_the_light() {
    let (mut app, wid, session) = glass(Instant::now());
    probe::bytes(
        &app,
        session,
        b"\x1b]11;rgb:fa/f8/f0\x1b\\\x1b]10;rgb:20/20/20\x1b\\",
    );
    feed(&app, session, START, Inks::default());
    frame(&mut app, wid);
    let halos = light(&app, wid);
    assert!(!halos.is_empty());
    assert!(
        halos
            .iter()
            .all(|h| h.mode == aterm_core::render::HaloMode::Over),
        "source-over veils"
    );
    assert!(
        app.windows[&wid].input_scratch.glow_under.is_empty(),
        "nothing under the glyph"
    );
    // Each task's dot: a round tint, then its hollow's fill inside it, both
    // centred on its bullet's row.
    let strip = strip_rows(&app, wid);
    let (_, ch) = app.win_cell_size(wid);
    let top = app.win_pad_top(wid) + app.win_head(wid);
    for row in [FIRST, SECOND, WORKFLOW] {
        let band = top + (strip + row) * ch..top + (strip + row + 1) * ch;
        let radii: Vec<u16> = halos
            .iter()
            .filter(|h| band.contains(&usize::from(h.cy)) && usize::from(h.row) == strip + row)
            .map(|h| h.rx)
            .collect();
        assert!(
            radii.len() == 2 && radii[0] > radii[1],
            "row {row}: a tint, then the fill ({radii:?})"
        );
    }
    let ink = painted_ink(&app, wid);
    assert!(ink > 0);
    app.serious_mode = true;
    frame(&mut app, wid);
    assert!(light(&app, wid).is_empty(), "serious: no light");
    assert_eq!(painted_ink(&app, wid), ink, "and the same ink");
    crate::claude_card::set_test_now(None);
}

/// What one pass cost the panel.
#[derive(Debug, Default, PartialEq, Eq)]
struct Cost {
    allocations: usize,
    reads: usize,
    folds: usize,
    looks: usize,
}

fn cost(app: &mut App, wid: WindowId) -> Cost {
    let (plan, route) = app.active_visible_frame_layout(wid).expect("a layout");
    let _ = (
        crate::work_counts::take_panel_reads(),
        crate::work_counts::take_panel_folds(),
        crate::work_counts::take_panel_looks(),
    );
    let (_, allocations) =
        crate::work_counts::allocations_in(|| probe::pass_on(app, wid, &plan, route));
    Cost {
        allocations,
        reads: crate::work_counts::take_panel_reads(),
        folds: crate::work_counts::take_panel_folds(),
        looks: crate::work_counts::take_panel_looks(),
    }
}

/// THE PANEL AT REST COSTS NOTHING: frame after frame of an unchanged screen
/// — agents running, the bar on the glass — the pane's pass allocates
/// nothing, with the panel switched on or off, reads no panel, folds
/// nothing and makes no look; the glass the same. CONTROL: the clock
/// Claude repaints reads, folds and makes the look once.
#[test]
fn a_panel_at_rest_costs_nothing() {
    let t0 = Instant::now();
    let rest = |agents: bool| {
        let (mut app, wid, session) = glass(t0);
        app.config.claude_footer = Some(crate::app_config::ClaudeFooterConfig {
            agents: Some(agents),
            ..Default::default()
        });
        for _ in 0..3 {
            frame(&mut app, wid);
        }
        let lit = app.windows[&wid].input_scratch.ink.clone();
        assert_eq!(painted_ink(&app, wid) > 0, agents);
        let costs: Vec<Cost> = (0..30)
            .map(|i| {
                let c = cost(&mut app, wid);
                assert_eq!(app.windows[&wid].input_scratch.ink, lit, "frame {i}");
                c
            })
            .collect();
        (app, wid, session, costs)
    };
    let (_, _, _, bare) = rest(false);
    let (mut app, wid, session, lit) = rest(true);
    for (i, (lit, bare)) in lit.iter().zip(&bare).enumerate() {
        assert_eq!(
            *bare,
            Cost::default(),
            "frame {i}: the pane's pass without it"
        );
        assert_eq!(
            *lit,
            Cost::default(),
            "frame {i}: the panel's pass costs nothing"
        );
    }
    // CONTROL: a second later, Claude repaints the clocks.
    crate::claude_card::set_test_now(Some(t0 + Duration::from_secs(1)));
    feed(
        &app,
        session,
        Agents {
            first: (22, "6.3k"),
            second: (10, "1.1k"),
        },
        Inks::default(),
    );
    probe::refill(&mut app, wid);
    let moved = cost(&mut app, wid);
    assert_eq!(
        (moved.reads, moved.folds, moved.looks),
        (1, 1, 1),
        "{moved:?}"
    );
    crate::claude_card::set_test_now(None);
}

/// Whether the panel on show breathes, and since when.
fn breath_of(app: &App, wid: WindowId) -> (bool, Option<u64>) {
    app.windows[&wid]
        .claude_card
        .panels()
        .entries
        .iter()
        .find(|e| e.shown)
        .map(|e| (e.breathing, e.breath_since))
        .expect("a panel on show")
}

/// A STUCK DOT BREATHES, THEN RESTS: an agent whose clock runs on with no
/// work for 150 s turns grey and arms the breath's wake, a step a
/// `BREATH_STEP_MS`, the key moving with each; its light swells and falls.
/// After `BREATH_FOR_MS` it rests — no step wakes — until the window
/// regains focus. NEGATIVE CONTROL: a panel whose every task Claude marked
/// done arms nothing and keys constant.
#[test]
fn a_stuck_dot_arms_the_breath_and_a_settled_panel_arms_nothing() {
    let t0 = Instant::now();
    let (mut app, wid, session) = glass(t0);
    app.windows.get_mut(&wid).unwrap().band_on_screen_for_test = true;
    let plan = app.active_visible_leaf_plan(wid).unwrap();
    frame(&mut app, wid);
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let set_now = |t: Instant| crate::claude_card::set_test_now(Some(t));
    // 151 s on: the clocks ran on, the tokens did not move.
    let stuck_at = 151_000;
    set_now(at(stuck_at));
    let repaint = |app: &App, s: u64| {
        feed(
            app,
            session,
            Agents {
                first: (21 + s, "6.2k"),
                second: (9 + s, "1.1k"),
            },
            Inks::default(),
        );
    };
    repaint(&app, 151);
    frame(&mut app, wid);
    let (breathing, since) = breath_of(&app, wid);
    assert!(breathing, "a stuck dot breathes");
    let since = since.unwrap();
    let epoch = app.windows[&wid].claude_card.panels().epoch.unwrap();
    let deadline = app.claude_panel_deadline();
    assert_eq!(
        deadline,
        Some(epoch + Duration::from_millis(since + glass::BREATH_STEP_MS)),
        "the breath's next step"
    );
    let halo_peak = |app: &App| {
        let halos = light(app, wid);
        halos
            .iter()
            .filter(|h| usize::from(h.row) == strip_rows(app, wid) + FIRST)
            .map(|h| h.color)
            .max()
    };
    let floor = halo_peak(&app);
    let mut fp = app.claude_footer_fp(wid, &plan);
    let mut peaks = Vec::new();
    for step in 1..=8u64 {
        let t = epoch + Duration::from_millis(since + step * glass::BREATH_STEP_MS);
        set_now(t);
        assert_eq!(app.claude_panels_due(t), vec![wid], "step {step}");
        let now_fp = app.claude_footer_fp(wid, &plan);
        assert_ne!(now_fp, fp, "the key moves with the breath ({step})");
        probe::pass(&mut app, wid);
        fp = app.claude_footer_fp(wid, &plan);
        peaks.push(halo_peak(&app));
    }
    assert!(
        peaks[7] > floor,
        "it swells to half a period: {floor:?} {peaks:?}"
    );
    // Its course: Claude's clock ticks on, a second a frame, for a minute.
    for s in 1..=61u64 {
        set_now(epoch + Duration::from_millis(since + s * 1000));
        repaint(&app, 151 + s);
        frame(&mut app, wid);
    }
    let (breathing, _) = breath_of(&app, wid);
    assert!(!breathing, "rested after its course");
    let rest = epoch + Duration::from_millis(since + 61_000);
    assert!(
        app.claude_panel_deadline()
            .is_none_or(|d| d >= rest + Duration::from_millis(1000)),
        "no breath step wakes once rested"
    );
    // Focus regained: the breath again.
    app.rearm_claude_panels(wid, || rest);
    assert!(
        app.claude_panel_deadline()
            .is_some_and(|d| d <= rest + Duration::from_millis(glass::BREATH_STEP_MS))
    );

    // NEGATIVE CONTROL: every task done.
    let (mut app, wid, session) = glass(t0);
    app.windows.get_mut(&wid).unwrap().band_on_screen_for_test = true;
    let done = "  \u{25EF} sweep  \u{25B0}\u{25B0}\u{25B0}  3/3 \u{B7} 12m05s \u{B7} \u{2193} 88.1k tokens";
    let rows = probe::screen(&Live {
        status: DONE,
        draft: &[""],
        under: &[FOOTER, "", "  \u{23FA} main", done],
    });
    let mut out = String::from("\x1b[2J");
    for (i, row) in rows.iter().enumerate() {
        let row = if row == done {
            format!(
                "  {}\u{25EF}{}{}",
                sgr(GREEN),
                sgr(GREY),
                &row[2 + '\u{25EF}'.len_utf8()..]
            )
        } else {
            row.clone()
        };
        out.push_str(&format!("\x1b[{};1H{row}\x1b[0m", i + 1));
    }
    out.push_str(&format!("\x1b[{};3H", ROWS as usize - 5));
    probe::bytes(&app, session, out.as_bytes());
    let plan = app.active_visible_leaf_plan(wid).unwrap();
    frame(&mut app, wid);
    assert!(
        app.windows[&wid]
            .claude_card
            .panels()
            .entries
            .iter()
            .any(|e| e.shown),
        "CONTROL: the panel is on show"
    );
    assert_eq!(app.claude_panel_deadline(), None, "nothing to come");
    assert!(!app.claude_panels_live());
    let fp = app.claude_footer_fp(wid, &plan);
    for s in 1..=20u64 {
        let t = t0 + Duration::from_secs(s * 10);
        set_now(t);
        assert!(app.claude_panels_due(t).is_empty());
        assert_eq!(app.claude_footer_fp(wid, &plan), fp, "{s}");
    }
    crate::claude_card::set_test_now(None);
}

/// A FOCUS BLIP BREATHES NO RESTED DOT AGAIN (`claude_card::RETURN_MS`), on
/// a window whose launch card is not on show — the agents panel alone, so
/// the breath alone decides what a regain is. A stuck dot breathes its
/// course and rests; the window loses focus through the OS focus seam and
/// has it back 20 ms later: the loss noted the window went away with a
/// breath on show, so the gain is a blip and no breath step is asked (a
/// course replayed on a blip is `BREATH_FOR_MS` of steps, each a wake of
/// its own where no card's flow lends the grid slack). NEGATIVE CONTROL:
/// away 2 s, the gain is a person's return, and the dot breathes its course
/// again, its next step asked within a step.
#[test]
fn a_focus_blip_breathes_no_rested_dot_again() {
    let t0 = Instant::now();
    let (mut app, wid, session) = glass(t0);
    {
        let ws = app.windows.get_mut(&wid).unwrap();
        ws.band_on_screen_for_test = true;
        ws.focused = true;
    }
    frame(&mut app, wid);
    let set_now = |t: Instant| crate::claude_card::set_test_now(Some(t));
    let repaint = |app: &App, s: u64| {
        feed(
            app,
            session,
            Agents {
                first: (21 + s, "6.2k"),
                second: (9 + s, "1.1k"),
            },
            Inks::default(),
        );
    };
    // 151 s on the clocks ran on and the tokens did not: stuck, breathing.
    set_now(t0 + Duration::from_secs(151));
    repaint(&app, 151);
    frame(&mut app, wid);
    let (breathing, since) = breath_of(&app, wid);
    assert!(breathing, "CONTROL: a stuck dot breathes");
    let since = since.unwrap();
    let epoch = app.windows[&wid].claude_card.panels().epoch.unwrap();
    // Its course: Claude's clock ticks on, a second a frame, for a minute.
    for s in 1..=61u64 {
        set_now(epoch + Duration::from_millis(since + s * 1000));
        repaint(&app, 151 + s);
        frame(&mut app, wid);
    }
    assert!(!breath_of(&app, wid).0, "CONTROL: rested after its course");
    assert!(
        !app.windows[&wid].claude_card.any_shown(),
        "CONTROL: no card on show, the panel alone"
    );
    let rest = epoch + Duration::from_millis(since + 61_000);
    let resting = |app: &App| {
        app.claude_panel_deadline()
            .is_none_or(|d| d >= rest + Duration::from_millis(1000))
    };
    assert!(resting(&app), "CONTROL: no breath step wakes once rested");
    // THE BLIP: 20 ms away.
    let lost = rest + Duration::from_millis(50);
    set_now(lost);
    app.window_focus_event(wid, false);
    assert!(!app.windows[&wid].focused, "CONTROL: the window lost focus");
    set_now(lost + Duration::from_millis(20));
    app.window_focus_event(wid, true);
    assert!(app.windows[&wid].focused, "CONTROL: the window has it back");
    assert_eq!(
        breath_of(&app, wid),
        (false, Some(since)),
        "a blip breathes no rested dot again"
    );
    assert!(resting(&app), "and asks no breath step");
    // NEGATIVE CONTROL: away 2 s — a return, and the breath again.
    let away = lost + Duration::from_secs(1);
    set_now(away);
    app.window_focus_event(wid, false);
    let returned = away + Duration::from_secs(2);
    set_now(returned);
    app.window_focus_event(wid, true);
    let (breathing, again) = breath_of(&app, wid);
    assert!(
        breathing && again.is_some_and(|t| t > since + 61_000),
        "NEGATIVE CONTROL: a return breathes the rested dot again, a new course ({again:?})"
    );
    crate::claude_card::set_test_now(None);
}

/// Token counts a running agent's row shows, one a reading: each a change.
const TICKS: [&str; 31] = [
    "1.2k", "1.3k", "1.4k", "1.5k", "1.6k", "1.7k", "1.8k", "1.9k", "2.0k", "2.1k", "2.2k", "2.3k",
    "2.4k", "2.5k", "2.6k", "2.7k", "2.8k", "2.9k", "3.0k", "3.1k", "3.2k", "3.3k", "3.4k", "3.5k",
    "3.6k", "3.7k", "3.8k", "3.9k", "4.0k", "4.1k", "4.2k",
];

/// The pane rows whose dot is lit: a light centred on the row's band.
fn lit_rows(app: &App, wid: WindowId) -> Vec<usize> {
    let strip = strip_rows(app, wid);
    let (_, ch) = app.win_cell_size(wid);
    let top = app.win_pad_top(wid) + app.win_head(wid);
    let mut rows: Vec<usize> = light(app, wid)
        .iter()
        .filter_map(|h| (usize::from(h.cy).checked_sub(top)? / ch).checked_sub(strip))
        .collect();
    rows.sort_unstable();
    rows.dedup();
    rows
}

/// A SECOND STUCK AGENT BREATHES A NEW COURSE: the first agent (no work from
/// the start) turns stuck at 150 s, breathes its course and rests, and rests
/// on while the stuck rows stay the same; when the second agent (its work
/// stopped 90 s in) turns stuck too, the stuck rows gain one and the breath
/// starts a new course — its next step asked within a step — capped like
/// the first: it rests 60 s on. NEGATIVE CONTROL: the rest between, the same
/// row stuck frame after frame, starts none.
#[test]
fn a_second_stuck_row_breathes_a_new_course() {
    let t0 = Instant::now();
    let (mut app, wid, session) = glass(t0);
    {
        let ws = app.windows.get_mut(&wid).unwrap();
        ws.band_on_screen_for_test = true;
        ws.focused = true;
    }
    frame(&mut app, wid);
    let states = |app: &App| -> Vec<DotState> {
        app.windows[&wid]
            .claude_card
            .panels()
            .entries
            .iter()
            .find(|e| e.shown)
            .map(|e| e.track.dots().iter().map(|d| d.state).collect())
            .unwrap()
    };
    // Claude repaints every reading 3 s apart; the second agent's tokens move
    // until 90 s.
    let at = |app: &mut App, s: u64| {
        crate::claude_card::set_test_now(Some(t0 + Duration::from_secs(s)));
        let tokens = TICKS[usize::try_from(s / 3).unwrap().min(TICKS.len() - 1)];
        feed(
            app,
            session,
            Agents {
                first: (21 + s, "6.2k"),
                second: (9 + s, tokens),
            },
            Inks::default(),
        );
        frame(app, wid);
        breath_of(app, wid)
    };
    let mut first_course = None;
    let mut s = 3;
    while s <= 150 {
        let (breathing, since) = at(&mut app, s);
        if breathing {
            first_course = since;
        }
        s += 3;
    }
    let since = first_course.expect("the first stuck dot breathes");
    assert_eq!(states(&app)[0], DotState::Stuck);
    // Its course runs out; the same row stuck, the breath rests on.
    while s <= 237 {
        let (breathing, again) = at(&mut app, s);
        if s >= 213 {
            assert!(!breathing, "{s} s: rested");
            assert_eq!(again, Some(since), "{s} s: no new course");
            assert_ne!(
                states(&app)[1],
                DotState::Stuck,
                "{s} s: the second not yet"
            );
        }
        s += 3;
    }
    // The second agent turns stuck: a new course.
    let (breathing, again) = at(&mut app, 240);
    assert_eq!(&states(&app)[..2], &[DotState::Stuck, DotState::Stuck]);
    assert!(breathing, "a new stuck row breathes");
    assert!(again.is_some_and(|t| t > since), "{again:?} after {since}");
    let now = t0 + Duration::from_secs(240);
    assert!(
        app.claude_panel_deadline()
            .is_some_and(|d| d <= now + Duration::from_millis(glass::BREATH_STEP_MS)),
        "its next step asked"
    );
    assert!(lit_rows(&app, wid).contains(&FIRST) && lit_rows(&app, wid).contains(&SECOND));
    // The new course is capped like the first: it rests `BREATH_FOR_MS`
    // (a minute) on, the stuck rows unchanged.
    let course = again;
    let mut s = 243;
    while s <= 309 {
        let (breathing, again) = at(&mut app, s);
        assert_eq!(again, course, "{s} s: the same course");
        assert_eq!(
            breathing,
            (s - 240) * 1000 < glass::BREATH_FOR_MS,
            "{s} s: breathing for its course, then rested"
        );
        s += 3;
    }
    crate::claude_card::set_test_now(None);
}

/// A 1×1 image, as a placement on a cell carries it.
fn image() -> aterm_core::grid::extra::ImageRef {
    use aterm_core::grid::extra::{ImageData, ImageFormat, ImageRef, ImageScaling};
    ImageRef {
        image: Arc::new(ImageData {
            bytes: vec![1, 2, 3, 4],
            format: ImageFormat::RawRgba8 {
                width: 1,
                height: 1,
            },
            cols: 1,
            rows: 1,
            z_index: 0,
            band_lift_px: 0,
            cell_offset_px: (0, 0),
            scaling: ImageScaling::Fit,
            source_rect: None,
        }),
        cell_row: 0,
        cell_col: 0,
        kitty: None,
    }
}

/// What a case does to the frame before the panel's pass: `(app, window,
/// frame row of the first agent)`.
type Touch = fn(&mut App, WindowId, usize);

/// AS DRAWN WHERE THE CELLS ARE NOT PLAIN: a combining mark, an emoji
/// cluster or an image on the first agent's name keeps its name in Claude's
/// ink, its dot still inked and lit; an image on its bullet keeps the dot
/// Claude's and unlit, its name inked; a row BiDi reordered or one the chrome
/// owns keeps its whole row Claude's, unlit. The second agent's row is lit
/// in every case. NEGATIVE CONTROL: the untouched frame inks and lights the
/// first agent's dot and name.
#[test]
fn marks_images_bidi_and_chrome_rows_keep_claudes_look() {
    // The name's columns (`  ◯ general-purpose`) and one inside it.
    let (name, inside) = (4..19, 6);
    let cases: [(&str, Touch, bool, bool); 7] = [
        ("untouched", |_, _, _| {}, true, true),
        (
            "a combining mark on the name",
            |app, wid, r| {
                let s = &mut app.windows.get_mut(&wid).unwrap().input_scratch;
                s.combining
                    .resize_with(s.combining.len().max(r + 1), Vec::new);
                s.combining[r].push((6, vec!['\u{301}'].into_boxed_slice()));
            },
            true,
            false,
        ),
        (
            "an emoji cluster on the name",
            |app, wid, r| {
                let s = &mut app.windows.get_mut(&wid).unwrap().input_scratch;
                s.clusters
                    .resize_with(s.clusters.len().max(r + 1), Vec::new);
                s.clusters[r].push((6, "\u{1F44D}\u{1F3FD}".into()));
            },
            true,
            false,
        ),
        (
            "an image on the name",
            |app, wid, r| {
                let s = &mut app.windows.get_mut(&wid).unwrap().input_scratch;
                s.images.resize_with(s.images.len().max(r + 1), Vec::new);
                s.images[r].push((6, image()));
            },
            true,
            false,
        ),
        (
            "an image on the bullet",
            |app, wid, r| {
                let s = &mut app.windows.get_mut(&wid).unwrap().input_scratch;
                s.images.resize_with(s.images.len().max(r + 1), Vec::new);
                s.images[r].push((2, image()));
            },
            false,
            true,
        ),
        (
            "a row BiDi reordered",
            |app, wid, r| {
                let s = &mut app.windows.get_mut(&wid).unwrap().input_scratch;
                let cols = s.cells[r].len();
                let mut map: Vec<u16> = (0..cols).map(|c| u16::try_from(c).unwrap()).collect();
                map.swap(40, 41);
                s.visual_to_logical
                    .resize_with(s.visual_to_logical.len().max(r + 1), Vec::new);
                s.visual_to_logical[r] = map;
            },
            false,
            false,
        ),
        (
            "a row the chrome owns",
            |app, wid, _| {
                let rows = &mut app.windows.get_mut(&wid).unwrap().chrome_rows;
                rows.clear();
                rows.push(FIRST..FIRST + 1);
            },
            false,
            false,
        ),
    ];
    for (what, touch, dot, named) in cases {
        let (mut app, wid, _) = glass(Instant::now());
        probe::refill(&mut app, wid);
        let r = strip_rows(&app, wid) + FIRST;
        touch(&mut app, wid, r);
        probe::pass(&mut app, wid);
        let cols: Vec<usize> = ink_on(&app, wid, FIRST)
            .iter()
            .map(|(c, _)| usize::from(*c))
            .collect();
        assert_eq!(cols.contains(&2), dot, "{what}: the dot ({cols:?})");
        assert_eq!(
            name.clone().all(|c| cols.contains(&c)),
            named,
            "{what}: the name ({cols:?})"
        );
        if !named {
            assert!(
                !cols.contains(&inside),
                "{what}: no letter of it ({cols:?})"
            );
        }
        let lit = lit_rows(&app, wid);
        assert_eq!(lit.contains(&FIRST), dot, "{what}: its light ({lit:?})");
        assert!(
            lit.contains(&SECOND) && !ink_on(&app, wid, SECOND).is_empty(),
            "{what}: the second agent's row lit"
        );
    }
    crate::claude_card::set_test_now(None);
}

/// A SCROLLED-BACK VIEW KEEPS CLAUDE'S PANEL: with the view one row into the
/// scrollback, nothing of the panel is inked or lit. NEGATIVE CONTROL: at the
/// live tail, before and after, it is.
#[test]
fn a_scrolled_back_view_keeps_claudes_panel() {
    let (mut app, wid, session) = glass(Instant::now());
    // History above the screen, then the panel drawn again.
    probe::bytes(
        &app,
        session,
        format!("\x1b[{ROWS};1H{}", "history\r\n".repeat(60)).as_bytes(),
    );
    feed(&app, session, START, Inks::default());
    frame(&mut app, wid);
    assert!(
        painted_ink(&app, wid) > 0 && !light(&app, wid).is_empty(),
        "CONTROL: lit"
    );
    let term = app.pool.get(session).unwrap().term.clone();
    crate::term_lock(&term).scroll_display(1);
    assert!(crate::term_lock(&term).grid().display_offset() > 0);
    frame(&mut app, wid);
    assert_eq!(painted_ink(&app, wid), 0, "scrolled back: Claude's panel");
    assert!(lit_rows(&app, wid).is_empty(), "and no light");
    crate::term_lock(&term).scroll_display(-1);
    frame(&mut app, wid);
    assert!(painted_ink(&app, wid) > 0, "CONTROL: lit again at the tail");
    crate::claude_card::set_test_now(None);
}

/// IDLE TO ZERO: a panel whose agents stop drawing — the clocks and the
/// tokens still — asks for its dots' turns (halted, quiet's fade) and then
/// for nothing at all.
#[test]
fn a_still_panel_goes_idle_to_zero() {
    let t0 = Instant::now();
    let (mut app, wid, _) = glass(t0);
    app.windows.get_mut(&wid).unwrap().band_on_screen_for_test = true;
    // No crest: the window unfocused (its bar's ripple waits on focus).
    app.windows.get_mut(&wid).unwrap().focused = false;
    frame(&mut app, wid);
    let mut wakes = 0;
    while let Some(d) = app.claude_panel_deadline() {
        wakes += 1;
        assert!(wakes < 400, "the wakes end");
        crate::claude_card::set_test_now(Some(d));
        let due = app.claude_panels_due(d);
        if !due.is_empty() {
            assert_eq!(due, vec![wid]);
            probe::pass(&mut app, wid);
        }
    }
    assert!(wakes > 0, "CONTROL: the dots turned");
    assert!(!app.claude_panels_live());
    crate::claude_card::set_test_now(None);
}

/// A TURN THAT MOVES NO DOT ASKS NO FRAME (the cross-track review of
/// 2026-10-19): a still panel's looks fade on a tick (`LOOK_TICK_MS`) while
/// Claude draws nothing new; the wake that judges each tick retimes the
/// dots, and a frame is asked — and the repaint key moved — only where their
/// look moved. CONTROL: some ticks move a dot's look, and some move none.
#[test]
fn a_turn_that_moves_no_dot_asks_no_frame() {
    let t0 = Instant::now();
    let (mut app, wid, _) = glass(t0);
    app.windows.get_mut(&wid).unwrap().band_on_screen_for_test = true;
    app.windows.get_mut(&wid).unwrap().focused = false;
    let plan = app.active_visible_leaf_plan(wid).unwrap();
    frame(&mut app, wid);
    let dots = |app: &App| {
        app.windows[&wid]
            .claude_card
            .panels()
            .entries
            .iter()
            .find(|e| e.shown)
            .map(|e| e.track.fingerprint())
    };
    let (mut asked, mut quiet, mut wakes) = (0, 0, 0);
    while let Some(d) = app.claude_panel_deadline() {
        wakes += 1;
        assert!(wakes < 400, "the wakes end");
        crate::claude_card::set_test_now(Some(d));
        let key = app.claude_footer_fp(wid, &plan);
        let before = dots(&app);
        let due = app.claude_panels_due(d);
        let moved = dots(&app) != before;
        assert_eq!(
            !due.is_empty(),
            moved,
            "a frame asked only where a dot's look moved ({wakes})"
        );
        assert_eq!(
            app.claude_footer_fp(wid, &plan) != key,
            moved,
            "the key moves with the look ({wakes})"
        );
        if moved {
            asked += 1;
            probe::pass(&mut app, wid);
        } else {
            quiet += 1;
        }
    }
    assert!(
        asked > 0 && quiet > 0,
        "CONTROL: {asked} ticks moved a look, {quiet} none"
    );
    crate::claude_card::set_test_now(None);
}

/// THE BAR CRESTS WHILE AN AGENT RUNS, AND ONLY ITS FILL MOVES: on a focused
/// window with motion allowed, a frame a `FLOW_STEP_MS` is asked for and
/// keyed, the ripple's crest passes along the bar's filled cells, and every
/// other inked cell — the names, the bar's empty cells, the dots — stands
/// still. NEGATIVE CONTROL: unfocused, the bar rests and no crest step is
/// asked for.
#[test]
fn the_bar_crests_while_an_agent_runs_and_only_its_fill_moves() {
    let t0 = Instant::now();
    let (mut app, wid, session) = glass(t0);
    {
        let ws = app.windows.get_mut(&wid).unwrap();
        ws.band_on_screen_for_test = true;
        ws.focused = true;
    }
    let plan = app.active_visible_leaf_plan(wid).unwrap();
    frame(&mut app, wid);
    let strip = strip_rows(&app, wid);
    let bar = {
        let row: String = app.windows[&wid].input_scratch.cells[strip + WORKFLOW]
            .iter()
            .map(|c| c.ch)
            .collect();
        col_of(&row, "\u{25B0}")
    };
    let split = |app: &App| {
        let mut fill = Vec::new();
        let mut rest = Vec::new();
        for row in MAIN..=WORKFLOW {
            for (col, rgb) in ink_on(app, wid, row) {
                if row == WORKFLOW && (bar..bar + 9).contains(&usize::from(col)) {
                    fill.push(rgb);
                } else {
                    rest.push((row, col, rgb));
                }
            }
        }
        (fill, rest)
    };
    let (fill0, rest0) = split(&app);
    let epoch = app.windows[&wid].claude_card.panels().epoch.unwrap();
    let mut fp = app.claude_footer_fp(wid, &plan);
    let mut fills = vec![fill0];
    for step in 1..=20u64 {
        let t = epoch + Duration::from_millis(step * FLOW_STEP_MS);
        crate::claude_card::set_test_now(Some(t));
        assert_eq!(
            app.claude_panel_deadline(),
            Some(t),
            "the crest's next step ({step})"
        );
        assert_eq!(app.claude_panels_due(t), vec![wid]);
        assert_ne!(app.claude_footer_fp(wid, &plan), fp, "keyed ({step})");
        probe::pass(&mut app, wid);
        fp = app.claude_footer_fp(wid, &plan);
        let (fill, rest) = split(&app);
        assert_eq!(rest, rest0, "nothing else moves ({step})");
        fills.push(fill);
    }
    let distinct = fills
        .iter()
        .enumerate()
        .filter(|(i, f)| fills[..*i].iter().all(|g| g != *f))
        .count();
    assert!(distinct > 4, "the crest passes along the fill: {distinct}");
    // NEGATIVE CONTROL: the window unfocused.
    app.windows.get_mut(&wid).unwrap().focused = false;
    let t = epoch + Duration::from_millis(21 * FLOW_STEP_MS);
    crate::claude_card::set_test_now(Some(t));
    let _ = session;
    probe::pass(&mut app, wid);
    assert!(
        !app.windows[&wid].claude_card.panels().entries[0].crest,
        "unfocused: the bar rests"
    );
    assert!(
        app.claude_panel_deadline()
            .is_none_or(|d| d > t + Duration::from_millis(FLOW_STEP_MS)),
        "no crest step asked"
    );
    crate::claude_card::set_test_now(None);
}

/// The launch card of the facts' build ([`probe::facts`]), drawn over the
/// transcript's first rows: a blank row, the banner, the model, the working
/// directory in Claude's gray, a blank row.
fn card(app: &App, session: u64) {
    let rows = [
        String::new(),
        format!(
            " \u{2590}\u{259B}\u{2588}\u{2588}\u{2588}\u{259B}\u{2588}   {}2.1.283",
            aterm_phase::anchor_text("launch.banner")
        ),
        "\u{259D}\u{259C}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2580}  Opus 5.5 \u{b7} Claude Max"
            .to_string(),
        " \u{259D}\u{259D}   \u{259D}\u{259D}   \x1b[38;2;130;130;130m~/aterm".to_string(),
        String::new(),
    ];
    let mut out = String::from("\x1b7");
    for (i, row) in rows.iter().enumerate() {
        out.push_str(&format!("\x1b[{};1H\x1b[2K{row}\x1b[0m", i + 1));
    }
    out.push_str("\x1b8");
    probe::bytes(app, session, out.as_bytes());
}

/// The wakes a window asks for `n` frames on from `t`, each frame drawn at
/// the soonest: `(card's, panel's)` deadline, in ms since `epoch`.
fn wakes(
    app: &mut App,
    wid: WindowId,
    epoch: Instant,
    mut t: Instant,
    n: usize,
) -> Vec<(Option<u128>, Option<u128>)> {
    let ms = |d: Option<Instant>| d.map(|d| d.duration_since(epoch).as_millis());
    let mut out = Vec::new();
    for _ in 0..n {
        let card = app.claude_card_deadline(t);
        let panel = app.claude_panel_deadline();
        out.push((ms(card), ms(panel)));
        let Some(next) = [card, panel].into_iter().flatten().min() else {
            break;
        };
        t = next;
        crate::claude_card::set_test_now(Some(t));
        let _ = app.claude_cards_due(t);
        let _ = app.claude_panels_due(t);
        frame(app, wid);
    }
    out
}

/// A crest beside a stuck dot (`a_breath_beside_a_crest_steps_on_its_grid`):
/// a focused window, motion allowed, an agent running and one stuck at 151 s.
fn crest_and_breath(t0: Instant) -> (App, WindowId, Instant) {
    let (mut app, wid, session) = glass(t0);
    {
        let ws = app.windows.get_mut(&wid).unwrap();
        ws.band_on_screen_for_test = true;
        ws.focused = true;
    }
    frame(&mut app, wid);
    let now = t0 + Duration::from_millis(151_000);
    crate::claude_card::set_test_now(Some(now));
    feed(
        &app,
        session,
        Agents {
            first: (172, "6.9k"),
            second: (160, "1.1k"),
        },
        Inks::default(),
    );
    frame(&mut app, wid);
    (app, wid, now)
}

/// A BREATH BESIDE A CREST STEPS ON ITS GRID (the cross-track review of
/// 2026-10-19): a stuck dot's breath starts its course on the crest's grid
/// and steps with it — every wake a `FLOW_STEP_MS` after the last, each on
/// the crest's steps, one frame a step for both (two clocks, 125 ms beside
/// 128 ms, woke the window twice a step). CONTROL: both move — the crest's
/// and the breath's steps are on the key.
#[test]
fn a_breath_beside_a_crest_steps_on_its_grid() {
    let t0 = Instant::now();
    let (mut app, wid, now) = crest_and_breath(t0);
    let entry = app.windows[&wid]
        .claude_card
        .panels()
        .entries
        .iter()
        .find(|e| e.shown)
        .expect("a panel on show");
    assert!(entry.crest && entry.breathing, "CONTROL: both move");
    let epoch = app.windows[&wid].claude_card.panels().epoch.unwrap();
    let seen = wakes(&mut app, wid, epoch, now, 32);
    let panel: Vec<u128> = seen.iter().filter_map(|w| w.1).collect();
    // Every wake is a crest step's or a dot's turn (Claude's clock, a
    // second apart) — none is a breath's of its own.
    let step = u128::from(FLOW_STEP_MS);
    assert!(
        panel.iter().all(|w| w % step == 0 || w % 1000 == 0),
        "on the crest's grid: {panel:?}"
    );
    let grid: Vec<u128> = panel.iter().copied().filter(|w| w % step == 0).collect();
    assert!(grid.len() >= 16, "CONTROL: the crest steps on: {panel:?}");
    assert!(
        grid.windows(2).all(|w| w[1] - w[0] == step),
        "one wake a step: {panel:?}"
    );
    crate::claude_card::set_test_now(None);
}

/// A PRESENT IN THE SLACK DRAWS THE BREATH'S STEP (the real-window lab of
/// 2026-10-20: a stuck dot breathing beside a flowing card fired
/// `agents_panel` 243 times in 30 s, 552 frames where Claude's repaints drew
/// 301): with the card flowing on the window, its ripple 50 ms off the
/// window's clock, a frame composed inside a step's slack — where the card's
/// lock puts Claude's repaints, `FLARE_STEP_LEAD_MS` into each step — draws
/// the breath's step, so the panel's next wake is the next step's and the
/// window presents once a step for the card, the breath and Claude. The
/// breath's step changes where the card's does, at the step's start, not at
/// its wake. NEGATIVE CONTROL: a frame a millisecond before a step's start
/// leaves that step's wake owed. CONTROL: the stuck dots breathe and the
/// card flows throughout.
#[test]
fn a_present_in_the_slack_draws_the_breaths_step() {
    let t0 = Instant::now();
    let (mut app, wid, session) = glass(t0);
    {
        let ws = app.windows.get_mut(&wid).unwrap();
        ws.band_on_screen_for_test = true;
        ws.focused = true;
    }
    let host = supervise(&mut app, session);
    frame(&mut app, wid);
    crate::claude_card::set_test_now(Some(t0 + Duration::from_millis(50)));
    card(&app, session);
    frame(&mut app, wid);
    // 151 s on: Claude's clocks ran on and no token moved — both stuck, so
    // nothing crests and the breath is the panel's only motion.
    let now = t0 + Duration::from_millis(151_000);
    crate::claude_card::set_test_now(Some(now));
    feed(
        &app,
        session,
        Agents {
            first: (172, START.first.1),
            second: (160, START.second.1),
        },
        Inks::default(),
    );
    card(&app, session);
    frame(&mut app, wid);
    let slack = Duration::from_millis(FLOW_SLACK_MS);
    let lead = Duration::from_millis(aterm_effects::harness_card::FLARE_STEP_LEAD_MS);
    let mut t = now;
    for k in 0..16 {
        assert!(
            breath_of(&app, wid).0,
            "CONTROL: the stuck dots breathe ({k})"
        );
        let wake = app
            .claude_card_deadline(t)
            .expect("CONTROL: the card flows");
        assert!(wake > t, "CONTROL: the card's next step is ahead ({k})");
        let start = wake - slack;
        crate::claude_card::set_test_now(Some(start - Duration::from_millis(1)));
        frame(&mut app, wid);
        assert_eq!(
            app.claude_panel_deadline(),
            Some(wake),
            "NEGATIVE CONTROL: drawn before step {k} began, its wake is owed"
        );
        t = start + lead;
        crate::claude_card::set_test_now(Some(t));
        frame(&mut app, wid);
        let next = app.claude_panel_deadline();
        assert!(
            next.is_some_and(|d| d > wake),
            "step {k}, drawn in its slack, owes no wake of its own: {next:?} against {wake:?}"
        );
    }
    host.shutdown_and_join();
    crate::claude_card::set_test_now(None);
}

/// THE PANEL'S SHIMMER RIDES THE CARD'S CLOCK (the panel's design, step 7;
/// the cross-track review of 2026-10-19): with the harness card flowing on
/// the same window — drawn 50 ms after the panel's clock started, so its
/// ripple is 50 ms off the window's — the bar's crest steps on the card's
/// ripple: each crest step's frame is asked at the card's own step's
/// deadline, so the window wakes once a step for both. NEGATIVE CONTROL: with
/// no card the crest keeps the window's own grid.
#[test]
fn the_panel_shimmer_rides_the_cards_clock() {
    let t0 = Instant::now();
    let (mut app, wid, session) = glass(t0);
    {
        let ws = app.windows.get_mut(&wid).unwrap();
        ws.band_on_screen_for_test = true;
        ws.focused = true;
    }
    let host = supervise(&mut app, session);
    frame(&mut app, wid);
    let epoch = app.windows[&wid].claude_card.panels().epoch.unwrap();
    crate::claude_card::set_test_now(Some(t0 + Duration::from_millis(50)));
    card(&app, session);
    frame(&mut app, wid);
    assert!(
        app.windows[&wid].claude_card.any_shown(),
        "CONTROL: the card is on show"
    );
    let t = t0 + Duration::from_millis(aterm_effects::harness_card::EPISODE_MS + 1000);
    crate::claude_card::set_test_now(Some(t));
    // Claude's clock ran on and the tokens moved: the agents still run.
    feed(
        &app,
        session,
        Agents {
            first: (28, "6.9k"),
            second: (16, "1.4k"),
        },
        Inks::default(),
    );
    card(&app, session);
    frame(&mut app, wid);
    let seen = wakes(&mut app, wid, epoch, t, 12);
    // Each of the panel's wakes is the card's step, or a dot's turn on
    // Claude's clock (a second apart).
    assert!(
        seen.iter()
            .all(|w| w.0.is_some() && (w.0 == w.1 || w.1.is_some_and(|p| p % 1000 == 0))),
        "the crest's wake is the card's: {seen:?}"
    );
    assert!(
        seen.iter().filter(|w| w.0 == w.1).count() >= 10,
        "CONTROL: the crest steps: {seen:?}"
    );
    let card_steps: Vec<u128> = seen.iter().filter_map(|w| w.0).collect();
    assert!(
        card_steps
            .windows(2)
            .all(|w| w[1] - w[0] == u128::from(FLOW_STEP_MS)),
        "CONTROL: the card flows a step a wake: {card_steps:?}"
    );
    host.shutdown_and_join();
    // NEGATIVE CONTROL: no card — the window's own grid.
    let (mut app, wid, _) = glass(t0);
    {
        let ws = app.windows.get_mut(&wid).unwrap();
        ws.band_on_screen_for_test = true;
        ws.focused = true;
    }
    frame(&mut app, wid);
    let epoch = app.windows[&wid].claude_card.panels().epoch.unwrap();
    let seen = wakes(&mut app, wid, epoch, t0, 8);
    assert!(
        seen.iter()
            .all(|w| w.0.is_none() && w.1.is_some_and(|p| p % u128::from(FLOW_STEP_MS) == 0)),
        "NEGATIVE CONTROL: the window's grid: {seen:?}"
    );
    crate::claude_card::set_test_now(None);
}

/// MOTION WITHHELD, NO STEP ASKED: under OS Reduce Motion or serious mode, a
/// focused window whose panel has an agent running beside a stuck one and a
/// workflow bar neither crests the bar nor breathes the stuck dot — no frame
/// is asked a crest or breath step away, and the repaint key stands still
/// between the dots' turns. NEGATIVE CONTROL: with motion allowed the same
/// panel crests and breathes, its next step asked within one.
#[test]
fn reduced_motion_and_serious_mode_ask_no_step() {
    let t0 = Instant::now();
    for (what, reduce, serious) in [
        ("motion allowed", false, false),
        ("reduce motion", true, false),
        ("serious mode", false, true),
    ] {
        let (mut app, wid, session) = glass(t0);
        {
            let ws = app.windows.get_mut(&wid).unwrap();
            ws.band_on_screen_for_test = true;
            ws.focused = true;
        }
        app.system_reduce_motion = reduce;
        app.serious_mode = serious;
        let plan = app.active_visible_leaf_plan(wid).unwrap();
        frame(&mut app, wid);
        // 151 s on: the first agent's tokens moved (running), the second's
        // did not (stuck).
        let now = t0 + Duration::from_secs(151);
        crate::claude_card::set_test_now(Some(now));
        feed(
            &app,
            session,
            Agents {
                first: (172, "6.9k"),
                second: (160, "1.1k"),
            },
            Inks::default(),
        );
        frame(&mut app, wid);
        let entry = app.windows[&wid]
            .claude_card
            .panels()
            .entries
            .iter()
            .find(|e| e.shown)
            .expect("a panel on show");
        let states: Vec<DotState> = entry.track.dots().iter().map(|d| d.state).collect();
        assert!(
            states.contains(&DotState::Running) && states.contains(&DotState::Stuck),
            "{what}: {states:?}"
        );
        let moving = !(reduce || serious);
        assert_eq!(
            (entry.crest, entry.breathing),
            (moving, moving),
            "{what}: crest, breath"
        );
        let step = Duration::from_millis(FLOW_STEP_MS.max(glass::BREATH_STEP_MS));
        let deadline = app.claude_panel_deadline();
        if moving {
            assert!(
                deadline.is_some_and(|d| d <= now + step),
                "{what}: the next step asked ({deadline:?})"
            );
        } else {
            assert!(
                deadline.is_none_or(|d| d > now + step),
                "{what}: no step asked ({deadline:?})"
            );
            let fp = app.claude_footer_fp(wid, &plan);
            for k in 1..=4u32 {
                crate::claude_card::set_test_now(Some(now + step * k));
                assert_eq!(
                    app.claude_footer_fp(wid, &plan),
                    fp,
                    "{what}: keyed still ({k})"
                );
            }
        }
    }
    crate::claude_card::set_test_now(None);
}

/// WHILE CLAUDE'S PANEL HAS THE KEYS, IT STAYS LIT: Claude replaces the mode
/// row with its hint row (`  Enter to view · x to stop`, measured on 2.1.304),
/// points `❯ ` at the selected agent's row from column 0 and draws that row
/// in the default ink; the pointer is inked in the row's stripe beside the
/// name and the dot, and the dot is lit. NEGATIVE CONTROL: a pointer drawn
/// in a colour keeps it, the rest of the row still lit.
#[test]
fn the_panel_stays_lit_while_it_has_the_keys() {
    for pointer_ink in [None, Some(BLUE)] {
        let (mut app, wid, session) = glass(Instant::now());
        // The panel as `under` lays it, its mode row replaced by Claude's
        // hint row and its first agent's row selected.
        let mut under = under(START);
        under[0] = "  Enter to view \u{B7} x to stop".to_owned();
        under[3] = under[3].replacen("  ", "\u{276F} ", 1);
        let refs: Vec<&str> = under.iter().map(String::as_str).collect();
        let rows = probe::screen(&Live {
            status: DONE,
            draft: &[""],
            under: &refs,
        });
        let mut out = String::from("\x1b[2J\x1b[?25l");
        for (i, row) in rows.iter().enumerate() {
            let painted = if row.starts_with('\u{2500}') {
                format!("{}{row}", sgr([90, 90, 90]))
            } else if i == MAIN {
                format!("\x1b[1m{row}")
            } else if i == FIRST {
                // The selected row: Claude's default ink, its pointer in
                // `pointer_ink` where the control colours it.
                let (pointer, rest) = row.split_at('\u{276F}'.len_utf8());
                match pointer_ink {
                    Some(rgb) => format!("{}{pointer}\x1b[39m{rest}", sgr(rgb)),
                    None => row.clone(),
                }
            } else if (SECOND..=WORKFLOW).contains(&i) && !row.trim().is_empty() {
                format!("{}{row}", sgr(GREY))
            } else {
                row.clone()
            };
            out.push_str(&format!("\x1b[{};1H{painted}\x1b[0m", i + 1));
        }
        probe::bytes(&app, session, out.as_bytes());
        frame(&mut app, wid);
        let row: String = app.windows[&wid].input_scratch.cells[strip_rows(&app, wid) + FIRST]
            .iter()
            .map(|c| c.ch)
            .collect();
        assert!(
            row.starts_with("\u{276F} \u{25EF} general-purpose"),
            "{row:?}"
        );
        let ink = ink_on(&app, wid, FIRST);
        let cols: Vec<u16> = ink.iter().map(|(c, _)| *c).collect();
        let name = u16::try_from(col_of(&row, "general-purpose")).unwrap();
        assert!(
            cols.contains(&2) && cols.contains(&name),
            "the dot and the name: {cols:?}"
        );
        assert_eq!(
            cols.contains(&0),
            pointer_ink.is_none(),
            "the pointer inked where Claude drew it plain: {cols:?}"
        );
        if pointer_ink.is_none() {
            // In the row's stripe: the pointer and the name's first letter
            // share the row's hue.
            let at = |col: u16| ink.iter().find(|(c, _)| *c == col).unwrap().1;
            assert_eq!(at(0), at(name), "the pointer leads the name");
        }
        let halos = light(&app, wid);
        let strip = strip_rows(&app, wid);
        assert!(
            halos.iter().any(|h| usize::from(h.row) == strip + FIRST),
            "the selected agent's dot is lit"
        );
    }
    crate::claude_card::set_test_now(None);
}

/// The wall clock the flight tests start at (unix ms).
const W0: i64 = 1_792_512_000_000;

/// Publish `table` as the session's side's flights, as the resolver does.
fn publish_flights(app: &App, session: u64, table: aterm_agent::harness::flight::FlightTable) {
    let entry = app.pool.get(session).expect("session");
    let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
    timeline.set_claude_side(
        PGID,
        Some(Arc::new(
            crate::claude_footer::FactSide::at(None, None).with_flights(table),
        )),
    );
}

/// The first agent's file: `general-purpose`, `bling probe`, started 21 s
/// before `W0` (its row's clock at the start), saying `flight`.
fn bling_probe(
    flight: aterm_agent::harness::flight::FileFlight,
) -> aterm_agent::harness::flight::FlightTable {
    aterm_agent::harness::flight::FlightTable {
        flyers: vec![aterm_agent::harness::flight::Flyer::agent(
            "general-purpose",
            Some("bling probe"),
            W0 - 21_000,
            flight,
        )],
    }
}

/// Its one Bash call, written at `W0` with a 400 s timeout.
fn bling_bash() -> aterm_agent::harness::flight::FileFlight {
    aterm_agent::harness::flight::FileFlight::Tool(aterm_agent::harness::flight::Call {
        id: 7,
        tool: aterm_agent::harness::flight::ToolName::of("Bash"),
        at_ms: W0,
        budget_ms: 400_000,
    })
}

/// The panel's dots' states on show.
fn dot_states(app: &App, wid: WindowId) -> Vec<DotState> {
    app.windows[&wid]
        .claude_card
        .panels()
        .entries
        .iter()
        .find(|e| e.shown)
        .map(|e| e.track.dots().iter().map(|d| d.state).collect())
        .expect("a panel on show")
}

/// A LONG CALL IN FLIGHT READS AS ITS TOOL ON THE GLASS: the first agent's
/// tokens still for three minutes while its file says its Bash call is
/// inside its budget — in its tool, its dot fresh and lit, not grey; the
/// call answered with the screen unchanged, the dots take it at the next
/// frame (running). At rest with the table published the pass still costs
/// nothing. NEGATIVE CONTROL: the same screens with no table read stuck.
#[test]
fn a_long_call_in_flight_reads_as_its_tool_on_the_glass() {
    let t0 = Instant::now();
    let run = |flights: bool| {
        let (mut app, wid, session) = glass(t0);
        set_test_wall(Some(W0));
        if flights {
            publish_flights(&app, session, bling_probe(bling_bash()));
        }
        frame(&mut app, wid);
        let mut s = 3;
        while s <= 180 {
            crate::claude_card::set_test_now(Some(t0 + Duration::from_secs(s)));
            set_test_wall(Some(W0 + i64::try_from(s).unwrap() * 1000));
            feed(
                &app,
                session,
                Agents {
                    first: (21 + s, "6.2k"),
                    second: (9 + s, "1.1k"),
                },
                Inks::default(),
            );
            frame(&mut app, wid);
            s += 3;
        }
        (app, wid, session)
    };
    let (control, control_wid, _) = run(false);
    assert_eq!(
        dot_states(&control, control_wid)[0],
        DotState::Stuck,
        "control"
    );
    drop(control);
    let (mut app, wid, session) = run(true);
    assert_eq!(dot_states(&app, wid)[0], DotState::Tool);
    assert_eq!(
        dot_states(&app, wid)[1],
        DotState::Stuck,
        "its own file is not known"
    );
    let strip = strip_rows(&app, wid);
    assert!(
        light(&app, wid)
            .iter()
            .any(|h| usize::from(h.row) == strip + FIRST),
        "lit"
    );
    // At rest with the table published: nothing read, folded or made.
    for i in 0..5 {
        assert_eq!(cost(&mut app, wid), Cost::default(), "frame {i}");
    }
    // The call answered, the screen unchanged: the next frame takes it.
    publish_flights(
        &app,
        session,
        bling_probe(aterm_agent::harness::flight::FileFlight::Model),
    );
    frame(&mut app, wid);
    assert_eq!(
        dot_states(&app, wid)[0],
        DotState::Running,
        "answered: evidence"
    );
    set_test_wall(None);
    crate::claude_card::set_test_now(None);
}

/// THE TITLE SAYS WHAT IT WAITS ON: with Claude's own `❯` on the row in its
/// tool (the person points at it), the footer's title slot on the bottom
/// rule reads `in Bash 3m06s`; the glass only — the engine's text is
/// Claude's. NEGATIVE CONTROL: the same screens with no `❯` title
/// nothing, and neither does a `❯` on a row whose call was answered.
#[test]
fn the_title_says_what_a_pointed_row_waits_on() {
    let t0 = Instant::now();
    let run = |pointed: bool, answered: bool| {
        let (mut app, wid, session) = glass(t0);
        set_test_wall(Some(W0));
        publish_flights(&app, session, bling_probe(bling_bash()));
        let inks = Inks {
            first_pointed: pointed,
            ..Inks::default()
        };
        frame(&mut app, wid);
        let mut s = 3;
        while s <= 186 {
            crate::claude_card::set_test_now(Some(t0 + Duration::from_secs(s)));
            set_test_wall(Some(W0 + i64::try_from(s).unwrap() * 1000));
            if answered && s == 183 {
                publish_flights(
                    &app,
                    session,
                    bling_probe(aterm_agent::harness::flight::FileFlight::Model),
                );
            }
            feed(
                &app,
                session,
                Agents {
                    first: (21 + s, "6.2k"),
                    second: (9 + s, "1.1k"),
                },
                inks,
            );
            frame(&mut app, wid);
            s += 3;
        }
        let strip = strip_rows(&app, wid);
        let rule: String = app.windows[&wid].input_scratch.cells[strip + BOTTOM]
            .iter()
            .map(|c| c.ch)
            .collect();
        let engine = engine_text(&app, session)[BOTTOM].clone();
        set_test_wall(None);
        crate::claude_card::set_test_now(None);
        (rule, engine)
    };
    let (rule, engine) = run(true, false);
    assert!(rule.contains("in Bash 3m06s"), "{rule:?}");
    assert!(!engine.contains("in Bash"), "the glass only: {engine:?}");
    let (bare, _) = run(false, false);
    assert!(!bare.contains("Bash"), "no pointer: {bare:?}");
    let (answered, _) = run(true, true);
    assert!(!answered.contains("Bash"), "answered: {answered:?}");
}

/// A WORKFLOW ALONE WEARS A TASK ROW'S HUE: for a workflow alone Claude Code
/// draws no `⏺ main` row (measured on 2.1.304, `aterm-phase`'s
/// `claude-2.1.304-agents-panel-workflow-long-bash-8s-120col.txt`), and its
/// row takes the hue it wears under a main row — the first task row's, never
/// the main row's stripe (Cruise's red, beside Claude's failed red; the lab
/// read it there). NEGATIVE CONTROL: under a main row, the main row's own
/// first letter is in another hue.
#[test]
fn a_workflow_alone_wears_a_task_rows_hue() {
    let flow = "  \u{25EF} stall-probe  \u{25B0}\u{25B0}\u{25B0}\u{25B1}\u{25B1}\u{25B1}  1/2 \u{B7} 8s \u{B7} \u{2193} 12.7k tokens";
    let paint = |with_main: bool| {
        let (mut app, wid, session) = glass(Instant::now());
        let mut under = vec![FOOTER.to_owned(), String::new()];
        if with_main {
            under.push("  \u{23FA} main".to_owned());
        }
        under.push(flow.to_owned());
        under.resize(UNDER, String::new());
        let refs: Vec<&str> = under.iter().map(String::as_str).collect();
        let rows = probe::screen(&Live {
            status: DONE,
            draft: &[""],
            under: &refs,
        });
        let mut out = String::from("\x1b[2J");
        for (i, row) in rows.iter().enumerate() {
            let painted = if row.starts_with('\u{2500}') {
                format!("{}{row}", sgr([90, 90, 90]))
            } else if row.contains("\u{23FA} main") {
                format!("\x1b[1m{row}")
            } else if row.contains("stall-probe") {
                format!("{}{row}", sgr(GREY))
            } else {
                row.clone()
            };
            out.push_str(&format!("\x1b[{};1H{painted}\x1b[0m", i + 1));
        }
        out.push_str(&format!("\x1b[{};3H", BOTTOM));
        probe::bytes(&app, session, out.as_bytes());
        frame(&mut app, wid);
        let first_letter = |needle: &str| {
            let row = rows.iter().position(|r| r.contains(needle))?;
            let col = u16::try_from(col_of(&rows[row], needle)).ok()?;
            ink_on(&app, wid, row)
                .into_iter()
                .find(|(c, _)| *c == col)
                .map(|(_, rgb)| rgb)
        };
        let main = with_main.then(|| first_letter("main")).flatten();
        (first_letter("stall-probe"), main)
    };
    let (alone, none) = paint(false);
    let (under_main, main) = paint(true);
    assert_eq!(none, None);
    let (alone, under_main, main) = (
        alone.expect("the lone workflow's name inked"),
        under_main.expect("the workflow's name inked"),
        main.expect("main's name inked"),
    );
    assert_eq!(alone, under_main, "a task row's hue, with or without main");
    assert_ne!(main, under_main, "the main row's stripe is its own");
    crate::claude_card::set_test_now(None);
}
