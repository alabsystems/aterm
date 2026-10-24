// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Agents panel rendering, gates, and interaction tests.

use super::*;

use std::sync::Arc;
use std::time::{Duration, Instant};

use aterm_agent::supervise::Autopilot;
use aterm_window::event::{ElementState, MouseButton};

use crate::claude_footer::probe::{self, COLS, DONE, Live, ROWS};

const PGID: i32 = 4242;
const GREY: [u8; 3] = [153, 153, 153];
const GREEN: [u8; 3] = [78, 186, 101];
const BLUE: [u8; 3] = [51, 153, 255];
const FOOTER: &str = "  \u{23F8} manual mode on \u{B7} ? for shortcuts \u{B7} \u{2193} to manage";

const UNDER: usize = 7;
const BOTTOM: usize = ROWS as usize - 1 - UNDER;
const MAIN: usize = BOTTOM + 3;
const FIRST: usize = BOTTOM + 4;
const SECOND: usize = BOTTOM + 5;
const WORKFLOW: usize = BOTTOM + 7;

fn elapsed(s: u64) -> String {
    if s < 60 {
        format!("{s}s")
    } else {
        format!("{}m {}s", s / 60, s % 60)
    }
}

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
    out.push_str(&format!("\x1b[{};3H", BOTTOM));
    probe::bytes(app, session, out.as_bytes());
}

fn claude(app: &App, session: u64) {
    probe::drawer(app, session, PGID, "claude", "2.1.283");
    let entry = app.pool.get(session).expect("session");
    let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
    timeline.note_foreground_group(PGID);
    timeline.set_program(PGID, Some("claude".into()));
    assert!(timeline.set_claude_footer(PGID, None, Some(probe::facts())));
}

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

fn frame(app: &mut App, wid: WindowId) -> u64 {
    probe::refill(app, wid);
    probe::pass(app, wid)
}

fn strip_rows(app: &App, wid: WindowId) -> usize {
    app.windows[&wid].input_scratch.cells.len() - usize::from(app.windows[&wid].rows)
}

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

fn light(app: &App, wid: WindowId) -> Vec<RainHalo> {
    let p = &app.windows[&wid].claude_card;
    let entry = p.panels().entries.iter().find(|e| e.shown);
    entry.map_or(Vec::new(), |e| e.halo.clone())
}

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
    let second = ink_on(&app, wid, SECOND);
    let workflow = ink_on(&app, wid, WORKFLOW);
    let lead = |ink: &[(u16, [u8; 3])], col: usize| {
        ink.iter().find(|(c, _)| usize::from(*c) == col).unwrap().1
    };
    assert_ne!(lead(&main, 4), lead(&first, 4));
    assert_ne!(lead(&first, 4), lead(&second, 4));
    let wrow = text(WORKFLOW);
    let bar = col_of(&wrow, "\u{25B0}");
    let cols: Vec<usize> = workflow.iter().map(|(c, _)| usize::from(*c)).collect();
    assert!(
        (bar..bar + 12).all(|c| cols.contains(&c)),
        "the bar: {cols:?}"
    );
    assert!(cols.contains(&2) && cols.contains(&4));
    assert!(!cols.iter().any(|&c| c >= bar + 12), "not its progress");
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

    probe::bytes(
        &app,
        session,
        format!(
            "\x1b[{};3H{}⏺\x1b[0m\x1b[{};3H",
            FIRST + 1,
            sgr(GREEN),
            BOTTOM
        )
        .as_bytes(),
    );
    frame(&mut app, wid);
    assert!(
        ink_on(&app, wid, FIRST).is_empty(),
        "Claude's colored viewed-agent bullet keeps its settled status and blue name"
    );
    assert!(
        !light(&app, wid)
            .iter()
            .any(|h| usize::from(h.row) == strip + FIRST),
        "the settled viewed agent has no running halo"
    );

    let rule = "─".repeat(usize::from(COLS));
    let agy_rows = vec![
        rule.clone(),
        "> ".into(),
        rule.clone(),
        "  ● Agent(self: Probe)  Working · 1s".into(),
        rule,
        "? for shortcuts".into(),
    ];
    let mut bytes = String::from("\x1b[2J");
    for (row, text) in agy_rows.iter().enumerate() {
        let painted = if row == 3 {
            format!(
                "  {}● {}Agent(self: Probe){}  Working · 1s",
                sgr(GREEN),
                sgr(BLUE),
                sgr(GREY)
            )
        } else {
            text.clone()
        };
        bytes.push_str(&format!("\x1b[{};1H{painted}\x1b[0m", row + 1));
    }
    probe::bytes(&app, session, bytes.as_bytes());
    probe::refill(&mut app, wid);
    let scratch = &app.windows[&wid].input_scratch;
    let read = read_panel(
        session,
        &agy_rows,
        2,
        scratch,
        PaneGeom {
            strip,
            row_off: 0,
            col_off: 0,
            rows: usize::from(ROWS),
            cols: usize::from(COLS),
        },
        scratch.implicit_blank,
    )
    .expect("actual ANSI cells of the AGY agent panel");
    assert_eq!(read.panel.rows().len(), 1);
    assert_eq!(read.tints[0], Tint::Plain);
    assert!(read.ink[0].bullet.is_some() && read.ink[0].name.is_some());

    feed(&app, session, START, Inks::default());
    probe::bytes(&app, session, format!("\x1b[{};40H", FIRST + 1).as_bytes());
    frame(&mut app, wid);
    assert!(ink_on(&app, wid, FIRST).is_empty(), "the cursor's row");
    assert!(!ink_on(&app, wid, SECOND).is_empty(), "the others lit");
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

#[test]
fn a_stuck_dot_arms_the_breath_and_a_settled_panel_arms_nothing() {
    let t0 = Instant::now();
    let (mut app, wid, session) = glass(t0);
    app.windows.get_mut(&wid).unwrap().band_on_screen_for_test = true;
    let plan = app.active_visible_leaf_plan(wid).unwrap();
    frame(&mut app, wid);
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let set_now = |t: Instant| crate::claude_card::set_test_now(Some(t));
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
    app.rearm_claude_panels(wid, || rest);
    assert!(
        app.claude_panel_deadline()
            .is_some_and(|d| d <= rest + Duration::from_millis(glass::BREATH_STEP_MS))
    );

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
    set_now(t0 + Duration::from_secs(151));
    repaint(&app, 151);
    frame(&mut app, wid);
    let (breathing, since) = breath_of(&app, wid);
    assert!(breathing, "CONTROL: a stuck dot breathes");
    let since = since.unwrap();
    let epoch = app.windows[&wid].claude_card.panels().epoch.unwrap();
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

const TICKS: [&str; 31] = [
    "1.2k", "1.3k", "1.4k", "1.5k", "1.6k", "1.7k", "1.8k", "1.9k", "2.0k", "2.1k", "2.2k", "2.3k",
    "2.4k", "2.5k", "2.6k", "2.7k", "2.8k", "2.9k", "3.0k", "3.1k", "3.2k", "3.3k", "3.4k", "3.5k",
    "3.6k", "3.7k", "3.8k", "3.9k", "4.0k", "4.1k", "4.2k",
];

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

type Touch = fn(&mut App, WindowId, usize);

#[test]
fn marks_images_bidi_and_chrome_rows_keep_claudes_look() {
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

#[test]
fn a_scrolled_back_view_keeps_claudes_panel() {
    let (mut app, wid, session) = glass(Instant::now());
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

#[test]
fn a_still_panel_goes_idle_to_zero() {
    let t0 = Instant::now();
    let (mut app, wid, _) = glass(t0);
    app.windows.get_mut(&wid).unwrap().band_on_screen_for_test = true;
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

fn card(app: &App, session: u64) {
    let rows = [
        String::new(),
        format!(
            " \u{2590}\u{259B}\u{2588}\u{2588}\u{2588}\u{259B}\u{2588}   {}2.1.283",
            aterm_phase::anchor!("launch.banner")
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

#[test]
fn the_panel_stays_lit_while_it_has_the_keys() {
    for pointer_ink in [None, Some(BLUE)] {
        let (mut app, wid, session) = glass(Instant::now());
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

const W0: i64 = 1_792_512_000_000;

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

fn bling_bash() -> aterm_agent::harness::flight::FileFlight {
    aterm_agent::harness::flight::FileFlight::Tool(aterm_agent::harness::flight::Call {
        id: 7,
        tool: aterm_agent::harness::flight::ToolName::of("Bash"),
        at_ms: W0,
        budget_ms: 400_000,
    })
}

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
    for i in 0..5 {
        assert_eq!(cost(&mut app, wid), Cost::default(), "frame {i}");
    }
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
