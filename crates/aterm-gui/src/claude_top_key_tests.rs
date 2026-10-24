// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for clicking the Claude Code top-rule effort key and ghost target.

use aterm_core::terminal::{RenderCell, Script, UnderlineStyle};
use aterm_window::event::{ElementState, MouseButton};

use super::*;
use crate::claude_lights::FactHit;
#[cfg(target_os = "macos")]
use crate::claude_lights::Target;
use crate::claude_slash::KeyLatch;

fn ghost_text() -> String {
    format!(" {KEY} off ")
}

fn claude_with(app: &App, session: u64, model: &str, version: &str, top: Option<bool>) {
    let entry = app.pool.get(session).expect("session");
    let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
    timeline.note_foreground_group(PGID);
    timeline.set_claude_footer(
        PGID,
        None,
        Some(FooterFacts {
            model: Some(model.into()),
            effort: Some("xhigh".into()),
            path: Some("~/aterm".into()),
            branch: Some("main".into()),
            version: Some(version.into()),
            top_effort: top,
            ..FooterFacts::default()
        }),
    );
    timeline.set_agent_reader_for_test(true);
}

fn top_row(app: &App, wid: WindowId) -> usize {
    footer_row(app, wid) - 2
}

fn top_cells(app: &App, wid: WindowId) -> Vec<RenderCell> {
    app.windows[&wid].input_scratch.cells[top_row(app, wid)].clone()
}

fn top_text(app: &App, wid: WindowId) -> String {
    top_cells(app, wid).iter().map(|c| c.ch).collect()
}

fn key_hit(app: &App, wid: WindowId) -> Option<FactHit> {
    app.windows[&wid]
        .claude_lights
        .fact_hits
        .iter()
        .find(|h| h.fact == Fact::TopEffort)
        .copied()
}

fn click_at(app: &mut App, wid: WindowId, cell: (usize, usize)) {
    let (x, y) = px_of(app, wid, cell);
    app.on_cursor_moved(wid, x, y);
    app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
    app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
}

fn wrote(app: &App, wid: WindowId, row: usize, cols: (usize, usize)) -> bool {
    app.windows[&wid]
        .claude_footer_undo
        .painted_spans()
        .any(|(at, col, len)| at == row && col < cols.1 && col + len > cols.0)
}

fn through_enter_after_probe(
    app: &mut App,
    session: u64,
    reader: &mut std::os::unix::net::UnixStream,
    slash: Slash,
) -> Instant {
    echo(app, session, CARET, "/");
    let at = Instant::now();
    app.advance_slash_presses(at);
    assert_eq!(pty(reader, rest(slash).len()), rest(slash));
    echo(app, session, CARET, &slash.text());
    app.advance_slash_presses(at);
    app.advance_slash_presses(at + QUIET);
    assert_eq!(pty(reader, 1), b"\r", "the Enter");
    at
}

#[test]
fn the_ghost_is_drawn_only_on_evidence_of_off() {
    let (mut app, wid, session, _reader) = idle();
    frame(&mut app, wid);
    let w = cols(&app, session);
    assert!(!top_text(&app, wid).contains(&ghost_text()), "no evidence");
    assert_eq!(key_hit(&app, wid), None, "and no target");
    claude_with(&app, session, "Opus 5.5", "2.1.304", Some(false));
    frame(&mut app, wid);
    let text = top_text(&app, wid);
    let chars: Vec<char> = text.chars().collect();
    let ghost: String = chars[w - 16..w - 1].iter().collect();
    assert_eq!(ghost, ghost_text(), "in Claude's slot: {text:?}");
    assert_eq!(chars[w - 1], '\u{2500}', "Claude's last `\u{2500}`");
    let hit = key_hit(&app, wid).expect("the ghost is a target");
    assert_eq!(hit.cols, (w - 15, w - 2), "its glyphs");
    let row = top_row(&app, wid);
    let rule = top_cells(&app, wid)[w - 1];
    let greyed =
        crate::claude_footer::greyed_ink(rule.fg, app.windows[&wid].input_scratch.implicit_blank);
    assert_ne!(greyed, rule.fg, "CONTROL: a rule in the pane's own ink");
    for cell in &top_cells(&app, wid)[w - 15..w - 2] {
        assert_eq!(
            (cell.fg, cell.bold, cell.script),
            (greyed, false, Script::Normal),
            "greyed, not bold, full size"
        );
    }
    answered(&app, session, TOP, true);
    frame(&mut app, wid);
    let text = top_text(&app, wid);
    assert!(!text.contains(&ghost_text()), "no ghost over the tag");
    let hit = key_hit(&app, wid).expect("the tag is a target");
    assert_eq!(hit.cols, (w - 11, w - 2), "the tag's key");
    assert_eq!(
        text.chars()
            .skip(hit.cols.0)
            .take(KEY.len())
            .collect::<String>(),
        KEY
    );
    assert!(
        !wrote(&app, wid, row, (hit.cols.0 - 1, w)),
        "Claude's cells unwritten"
    );
    assert_eq!(
        app.claude_facts.get(&session).and_then(|f| f.top_seen),
        Some(PGID),
        "the tag seen for the process"
    );
    claude_with(&app, session, "Opus 5.5", "2.1.304", None);
    answered(&app, session, TOP, false);
    frame(&mut app, wid);
    assert!(
        top_text(&app, wid).contains(&ghost_text()),
        "seen: the ghost"
    );
    claude_with(&app, session, "Opus 5.5", "2.1.302", Some(false));
    frame(&mut app, wid);
    assert!(
        !top_text(&app, wid).contains(&ghost_text()),
        "2.1.302: none"
    );
    assert_eq!(key_hit(&app, wid), None);
    claude_with(&app, session, "Opus 5.5", "2.1.304", Some(false));
    app.claude_facts.entry(session).or_default().latch = Some(KeyLatch {
        pgid: PGID,
        model: Some("Opus 5.5".into()),
        named: None,
        answer: aterm_agent::harness::footer::TopAnswer::NeedsWorkflows,
    });
    frame(&mut app, wid);
    assert!(
        !top_text(&app, wid).contains(&ghost_text()),
        "latched: none"
    );
    assert_eq!(key_hit(&app, wid), None);
    claude_with(&app, session, "Sonnet 5.5", "2.1.304", Some(false));
    frame(&mut app, wid);
    assert!(
        top_text(&app, wid).contains(&ghost_text()),
        "another model: back"
    );
}

#[test]
fn a_click_on_the_ghost_types_key_on_and_reads_it_back() {
    let (mut app, wid, session, mut reader) = idle();
    claude_with(&app, session, "Opus 5.5", "2.1.304", Some(false));
    frame(&mut app, wid);
    let hit = key_hit(&app, wid).expect("the ghost");
    let row = hit.frame_row;
    click_at(&mut app, wid, (row, hit.cols.0 - 1));
    assert!(pty(&mut reader, 0).is_empty(), "CONTROL: nothing typed");
    assert_eq!(stage(&app, session), None, "CONTROL: no press");
    let human = human_seq(&app, session);
    frame(&mut app, wid);
    let at_rest = top_cells(&app, wid)[hit.cols.0 + 1];
    assert_eq!(at_rest.underline, UnderlineStyle::None, "at rest: plain");
    let (x, y) = px_of(&app, wid, (row, hit.cols.0 + 1));
    app.on_cursor_moved(wid, x, y);
    frame(&mut app, wid);
    let lit = top_cells(&app, wid)[hit.cols.0 + 1];
    assert_eq!(lit.underline, UnderlineStyle::Single, "hovered: underlined");
    click_at(&mut app, wid, (row, hit.cols.0 + 1));
    assert_eq!(pty(&mut reader, 1), b"/", "the probe alone");
    assert_eq!(human_seq(&app, session), human + 1, "the person's write");
    assert_eq!(
        said(&app, wid, session).as_deref(),
        Some(format!("turning {KEY} on\u{2026}").as_str())
    );
    frame(&mut app, wid);
    let pending = top_text(&app, wid);
    assert!(
        pending.contains(&format!("\u{25d0}{}", ghost_text())),
        "◐ before the ghost: {pending:?}"
    );
    let slash = Slash::TopEffort(true);
    let at = through_enter_after_probe(&mut app, session, &mut reader, slash);
    assert_eq!(human_seq(&app, session), human + 1, "aterm's own writes");
    answered(&app, session, TOP, true);
    app.advance_slash_presses(at + QUIET);
    assert_eq!(ended(&app, session), Some(SlashEnd::Done));
    assert_eq!(
        said(&app, wid, session).as_deref(),
        Some(format!("\u{2713} {KEY} on (this session)").as_str())
    );
    assert_eq!(app.claude_facts[&session].top_seen, Some(PGID));
    frame(&mut app, wid);
    assert!(
        !top_text(&app, wid).contains(&ghost_text()),
        "the ghost gone"
    );
    let tag = key_hit(&app, wid).expect("Claude's tag the target");
    assert_eq!(tag.cols.1, cols(&app, session) - 2);
    assert!(pty(&mut reader, 0).is_empty(), "nothing after the answer");
}

#[test]
fn a_band_over_the_bottom_rule_leaves_the_top_rules_key() {
    let (mut app, wid, session, _reader) = idle();
    claude_with(&app, session, "Opus 5.5", "2.1.304", Some(false));
    frame(&mut app, wid);
    assert!(
        top_text(&app, wid).contains(&ghost_text()),
        "CONTROL: the ghost"
    );
    assert!(key_hit(&app, wid).is_some(), "CONTROL: its target");
    let prepared = app.prepare_terminal_capture_grid_with_cursor_fx_and_plan_outcome(
        wid,
        crate::app_render::ComposedCursorFxClock::Advance(std::time::Instant::now()),
    );
    let crate::app_render::CapturePreparation::Ready((grid, plan)) = prepared else {
        panic!("the headless capture must produce a frame");
    };
    app.splice_tab_strip(wid);
    let band = TOP + 1..TOP + 4;
    app.windows.get_mut(&wid).unwrap().chrome_rows = std::iter::once(band).collect();
    let strip = {
        let ws = &app.windows[&wid];
        ws.input_scratch.cells.len() - usize::from(ws.rows)
    };
    let under = |app: &App| {
        app.windows[&wid].input_scratch.cells[strip + TOP + 1..strip + TOP + 4].to_vec()
    };
    let before = under(&app);
    app.splice_claude_footer(
        wid,
        &plan,
        crate::VisibleContentRoute::Terminal {
            composed: grid.composed,
        },
    );
    assert!(
        top_text(&app, wid).contains(&ghost_text()),
        "the ghost stays: {:?}",
        top_text(&app, wid)
    );
    assert!(key_hit(&app, wid).is_some(), "and its target");
    assert_eq!(under(&app), before, "nothing under the band is written");
}

#[test]
fn a_double_click_on_the_key_opens_no_menu_while_aterm_types() {
    for draft in [false, true] {
        let (mut app, wid, session, mut reader) = idle();
        claude_with(&app, session, "Opus 5.5", "2.1.304", Some(false));
        draw_claude(&app, session, BYPASS);
        if draft {
            echo(&app, session, CARET, "draft");
        }
        frame(&mut app, wid);
        assert!(
            !app.windows[&wid].claude_lights.shows_block(session),
            "CONTROL: no block"
        );
        let hit = key_hit(&app, wid).expect("the ghost");
        let cell = (hit.frame_row, hit.cols.0 + 1);
        click_at(&mut app, wid, cell);
        if draft {
            assert!(pty(&mut reader, 0).is_empty(), "CONTROL: nothing typed");
            assert!(app.autopilot_menu.is_some(), "CONTROL: the menu opens");
            continue;
        }
        assert_eq!(pty(&mut reader, 1), b"/", "the probe");
        click_at(&mut app, wid, cell);
        assert!(app.autopilot_menu.is_none(), "no menu while aterm types");
        assert!(pty(&mut reader, 0).is_empty(), "nothing more");
        assert_eq!(stage(&app, session), Some(Stage::Probed));
    }
}

/// A CLICK ON CLAUDE'S TAG TYPES THE KEY OFF AND NEVER WRITES ITS CELLS:
/// what the click turns the key to is the ENGINE's — off, its tag drawn —
/// and through the press the tag's cells and its leading blank are never in
/// the footer's write, only the `◐` in the `─` before them; Claude's tag
/// gone ends it with its notice, and the ghost stands in its slot (the
/// process showed the key).
#[cfg(target_os = "macos")]
#[test]
fn a_click_on_claudes_tag_types_key_off_and_never_writes_its_cells() {
    let (mut app, wid, session, mut reader) = idle();
    answered(&app, session, TOP, true);
    frame(&mut app, wid);
    let tag = key_hit(&app, wid).expect("the tag");
    let row = tag.frame_row;
    let before = top_cells(&app, wid)[tag.cols.0 - 1..].to_vec();
    click_at(&mut app, wid, (row, tag.cols.0));
    assert_eq!(pty(&mut reader, 1), b"/");
    frame(&mut app, wid);
    assert_eq!(
        top_cells(&app, wid)[tag.cols.0 - 2].ch,
        '\u{25d0}',
        "◐ before it"
    );
    assert_eq!(
        top_cells(&app, wid)[tag.cols.0 - 1..],
        before[..],
        "the tag as drawn"
    );
    assert!(!wrote(
        &app,
        wid,
        row,
        (tag.cols.0 - 1, cols(&app, session))
    ));
    let slash = Slash::TopEffort(false);
    let at = through_enter_after_probe(&mut app, session, &mut reader, slash);
    frame(&mut app, wid);
    assert_eq!(top_cells(&app, wid)[tag.cols.0 - 1..], before[..], "still");
    answered(&app, session, TOP, false);
    app.advance_slash_presses(at + QUIET);
    assert_eq!(ended(&app, session), Some(SlashEnd::Done));
    assert_eq!(
        said(&app, wid, session).as_deref(),
        Some(format!("\u{2713} {KEY} off (this session)").as_str())
    );
    frame(&mut app, wid);
    assert!(
        top_text(&app, wid).contains(&ghost_text()),
        "the ghost in its slot"
    );
    let mut walked = Vec::new();
    for _ in 0..10 {
        chord(&mut app, wid);
        frame(&mut app, wid);
        let (_, at) = app.windows[&wid]
            .claude_lights
            .selected
            .expect("a selection");
        if walked.contains(&at) {
            break;
        }
        walked.push(at);
    }
    assert_eq!(
        walked.last(),
        Some(&Target::Fact(Fact::TopEffort)),
        "{walked:?}"
    );
    assert!(
        walked.contains(&Target::Fact(Fact::Branch)),
        "after the bottom rule's facts: {walked:?}"
    );
    while app.windows[&wid].claude_lights.selected != Some((session, Target::Fact(Fact::TopEffort)))
    {
        chord(&mut app, wid);
        frame(&mut app, wid);
    }
    assert!(
        top_text(&app, wid).contains(&format!("\u{25b8}{}", ghost_text())),
        "▸ selected"
    );
    use aterm_window::keyboard::{KeyCode, NamedKey};
    tap(&mut app, wid, NamedKey::Enter, KeyCode::Enter, "\r");
    assert_eq!(pty(&mut reader, 1), b"/", "Return is the click: on again");
    assert_eq!(
        app.claude_facts[&session].press.as_ref().map(|p| p.slash),
        Some(Slash::TopEffort(true))
    );
}

#[test]
fn a_refused_key_latches_the_ghost_for_that_model() {
    let (mut app, wid, session, mut reader) = idle();
    claude_with(&app, session, "Opus 5.5", "2.1.304", Some(false));
    frame(&mut app, wid);
    let hit = key_hit(&app, wid).expect("the ghost");
    click_at(&mut app, wid, (hit.frame_row, hit.cols.0));
    assert_eq!(pty(&mut reader, 1), b"/");
    let slash = Slash::TopEffort(true);
    let at = through_enter_after_probe(&mut app, session, &mut reader, slash);
    feed(
        &app,
        session,
        format!(
            "\x1b[17;1H\x1b[2K\u{276F} {}\x1b[18;1H\x1b[2K  \u{23BF}  {} isn't available on \
             claude-sonnet-4-5. Valid options are: low, medium, high\x1b[19;1H\x1b[2K\
             \x1b[20;1H\x1b[2K",
            slash.text(),
            key_capitalised()
        )
        .as_bytes(),
    );
    answered(&app, session, TOP, false);
    app.advance_slash_presses(at + QUIET);
    assert_eq!(
        ended(&app, session),
        Some(SlashEnd::Stopped(SlashRefusal::Unavailable))
    );
    frame(&mut app, wid);
    assert!(!top_text(&app, wid).contains(&ghost_text()), "latched");
    assert_eq!(key_hit(&app, wid), None, "and no target");
    claude_with(&app, session, "Sonnet 5.5", "2.1.304", Some(false));
    frame(&mut app, wid);
    assert!(top_text(&app, wid).contains(&ghost_text()), "a new model");
    assert!(key_hit(&app, wid).is_some());
}

fn held(app: &mut App, session: u64) -> Hosted {
    let hosted = host(app);
    let ctx = app.pool.get(session).expect("session").ctx.clone();
    crate::session_timeline::claim_supervisor(
        &ctx,
        &crate::harness_host::holder(),
        None,
        None,
        crate::metrics::now_us(),
    )
    .expect("the claim");
    hosted
}

#[test]
fn the_ghost_shows_with_the_harness_off_and_writes_only_its_cells() {
    use aterm_agent::supervise::Autopilot;
    {
        let (mut app, wid, session, _reader) = idle();
        let _host = held(&mut app, session);
        claude_with(&app, session, "Opus 5.5", "2.1.304", Some(false));
        frame(&mut app, wid);
        let row = top_row(&app, wid);
        assert!(
            wrote(&app, wid, row, (0, 2)),
            "CONTROL: the border tints the run"
        );
    }
    let (mut app, wid, session, mut reader) = idle();
    claude_with(&app, session, "Opus 5.5", "2.1.304", Some(false));
    frame(&mut app, wid);
    let w = cols(&app, session);
    let row = top_row(&app, wid);
    assert!(app.set_session_autopilot(
        session,
        Some(Autopilot::Off),
        crate::autopilot::Surface::Chip
    ));
    frame(&mut app, wid);
    let hit = key_hit(&app, wid).expect("the ghost, bare");
    assert!(top_text(&app, wid).contains(&ghost_text()));
    assert!(!wrote(&app, wid, row, (0, 2)), "nothing left of the lockup");
    assert!(wrote(&app, wid, row, (2, 13)), "the lockup's cells");
    let text: Vec<char> = top_text(&app, wid).chars().collect();
    assert!(
        text[13..w - 16].iter().all(|&c| c == '\u{2500}'),
        "between the lockup and the ghost, Claude's rule: {:?}",
        text.iter().collect::<String>()
    );
    assert!(
        !wrote(&app, wid, row, (w - 1, w)),
        "nor Claude's last `\u{2500}`"
    );
    assert!(wrote(&app, wid, row, (w - 16, w - 1)), "the ghost's cells");
    let draft = SlashRefusal::Draft.says("", None);
    echo(&app, session, CARET, "hello");
    frame(&mut app, wid);
    assert!(
        app.windows[&wid].claude_lights.shows_block(session),
        "a block"
    );
    click_at(&mut app, wid, (hit.frame_row, hit.cols.0));
    assert!(pty(&mut reader, 0).is_empty(), "nothing typed");
    assert_eq!(said(&app, wid, session), Some(draft.clone()));
    assert!(app.autopilot_menu.is_none(), "the title slot says it");
    draw_claude(&app, session, BYPASS);
    echo(&app, session, CARET, "hello");
    frame(&mut app, wid);
    assert!(
        !app.windows[&wid].claude_lights.shows_block(session),
        "no block"
    );
    let hit = key_hit(&app, wid).expect("the ghost");
    click_at(&mut app, wid, (hit.frame_row, hit.cols.0));
    assert!(pty(&mut reader, 0).is_empty(), "nothing typed");
    let menu = app.autopilot_menu.as_ref().expect("the effort menu");
    assert_eq!(
        menu.anchor,
        (hit.frame_row, hit.cols.0),
        "hung from the key"
    );
    assert!(menu_notes(&app).contains(&draft), "{:?}", menu_notes(&app));
    assert!(
        app.autopilot_menu
            .as_ref()
            .is_some_and(|m| m.rows.iter().all(|row| !matches!(
                row,
                MenuRow::Item { act, enabled: true, .. } if act.key().contains("/type/")
            )))
    );
}

/// THE KEY'S OWN WORDS AND MENU (design §10.6.14): its title says what a
/// click turns it to — hovered beside a block of chips, in the block's
/// title slot — and a reader names it `<Key>: on|off`; selected, `↓` opens
/// the effort's menu hung from the key, offering the toggle the engine's
/// top rule calls for, and it stays while the key does.
#[cfg(target_os = "macos")]
#[test]
fn the_keys_title_and_menu_are_its_own() {
    use aterm_window::keyboard::{KeyCode, NamedKey};
    let words = crate::claude_facts::top_title(false);
    assert_eq!(
        words,
        (
            format!("{KEY} off \u{b7} click turns it on for this session"),
            "click: on".to_owned()
        )
    );
    assert_eq!(
        crate::claude_facts::top_title(true).0,
        format!("{KEY} on \u{b7} click turns it off for this session")
    );
    assert_eq!(
        crate::claude_facts::top_spoken(false).0,
        format!("{}: off", key_capitalised())
    );
    assert_eq!(
        crate::claude_facts::top_spoken(true).0,
        format!("{}: on", key_capitalised())
    );
    let (mut app, wid, session, mut reader) = idle();
    claude_with(&app, session, "Opus 5.5", "2.1.304", Some(false));
    frame(&mut app, wid);
    let hit = key_hit(&app, wid).expect("the ghost");
    let footer = footer_row(&app, wid);
    let said_title = |app: &App| {
        let text = row_text(app, wid, footer);
        text.contains(&words.0) || text.contains(&words.1)
    };
    assert!(!said_title(&app), "CONTROL: no title at rest");
    let (x, y) = px_of(&app, wid, (hit.frame_row, hit.cols.0));
    app.on_cursor_moved(wid, x, y);
    frame(&mut app, wid);
    assert!(said_title(&app), "{:?}", row_text(&app, wid, footer));
    let (x, y) = px_of(&app, wid, (footer, 1));
    app.on_cursor_moved(wid, x, y);
    frame(&mut app, wid);
    assert!(!said_title(&app), "CONTROL: the pointer off it");
    while app.windows[&wid].claude_lights.selected != Some((session, Target::Fact(Fact::TopEffort)))
    {
        chord(&mut app, wid);
        frame(&mut app, wid);
    }
    tap(&mut app, wid, NamedKey::ArrowDown, KeyCode::ArrowDown, "");
    let menu = app.autopilot_menu.as_ref().expect("the menu");
    assert_eq!(menu.anchor, (hit.frame_row, hit.cols.0));
    assert!(menu_offers(&app, "fact-menu/top-effort/type/key-on"));
    assert!(menu_offers(&app, "/type/effort"), "the slider");
    frame(&mut app, wid);
    assert!(app.autopilot_menu.is_some(), "it stays while the key does");
    tap(&mut app, wid, NamedKey::Escape, KeyCode::Escape, "\u{1b}");
    assert!(app.autopilot_menu.is_none());
    assert!(pty(&mut reader, 0).is_empty(), "nothing typed");
}

/// A SCREEN READER'S PRESS ON THE KEY IS ITS CLICK, and its node is named
/// `<Key>: off` while the ghost stands.
#[cfg(a11y_tree)]
#[test]
fn a_readers_press_on_the_key_is_its_click() {
    use crate::a11y_tree::HarnessChip;
    use aterm_a11y::model::Toggled;
    let (mut app, wid, session, mut reader) = idle();
    frame(&mut app, wid);
    assert!(
        !app.claude_chips_a11y(wid)
            .iter()
            .any(|n| n.chip == HarnessChip::TopEffort),
        "CONTROL: no node"
    );
    app.press_claude_chip_for_reader(wid, session, HarnessChip::TopEffort);
    assert!(pty(&mut reader, 0).is_empty(), "CONTROL: nothing typed");
    claude_with(&app, session, "Opus 5.5", "2.1.304", Some(false));
    frame(&mut app, wid);
    let node = app
        .claude_chips_a11y(wid)
        .into_iter()
        .find(|n| n.chip == HarnessChip::TopEffort)
        .expect("the key's node");
    assert_eq!(node.label, format!("{}: off", key_capitalised()));
    assert_eq!(
        node.toggled,
        Some(Toggled::False),
        "a two-state key is a switch, its state off (review 13)"
    );
    let model = app
        .claude_chips_a11y(wid)
        .into_iter()
        .find(|n| n.chip == HarnessChip::Model)
        .expect("CONTROL: the model's node");
    assert_eq!(model.toggled, None, "CONTROL: a fact's button has no state");
    app.press_claude_chip_for_reader(wid, session, HarnessChip::TopEffort);
    assert_eq!(pty(&mut reader, 1), b"/", "the click's probe");
}

/// THE KEY'S NODE STATES ITS STATE BOTH WAYS (review 13): with Claude's own
/// tag on the top rule the key is on, its node `<Key>: on` and toggled on.
/// NEGATIVE CONTROL: the tag gone and the transcript saying off, the ghost's
/// node is `<Key>: off`, toggled off.
#[cfg(a11y_tree)]
#[test]
fn the_keys_node_is_on_while_claudes_tag_stands() {
    use crate::a11y_tree::HarnessChip;
    use aterm_a11y::model::Toggled;
    let (mut app, wid, session, _reader) = idle();
    let key = |app: &App| {
        app.claude_chips_a11y(wid)
            .into_iter()
            .find(|n| n.chip == HarnessChip::TopEffort)
            .map(|n| (n.label, n.toggled))
    };
    answered(&app, session, TOP, true);
    frame(&mut app, wid);
    assert_eq!(
        key(&app),
        Some((format!("{}: on", key_capitalised()), Some(Toggled::True))),
        "the tag drawn"
    );
    answered(&app, session, TOP, false);
    claude_with(&app, session, "Opus 5.5", "2.1.304", Some(false));
    frame(&mut app, wid);
    assert_eq!(
        key(&app),
        Some((format!("{}: off", key_capitalised()), Some(Toggled::False))),
        "NEGATIVE CONTROL: the tag gone, the ghost"
    );
}

#[test]
fn the_keys_mark_reads_on_a_tab_theme_the_instance_did_not_choose() {
    use aterm_effects::color_math::contrast_ratio;
    let (mut app, wid, session, mut reader) = idle();
    let dracula = aterm_types::scheme::builtin("Dracula").expect("a built-in");
    app.theme = aterm_render::Theme::from(&dracula);
    app.set_session_theme(
        session,
        Some(crate::session_theme::SessionTheme {
            selector: "Default Light".into(),
            origin: crate::session_theme::SessionThemeOrigin::Ctl,
        }),
    );
    answered(&app, session, TOP, true);
    frame(&mut app, wid);
    let tag = key_hit(&app, wid).expect("the tag");
    click_at(&mut app, wid, (tag.frame_row, tag.cols.0));
    assert_eq!(pty(&mut reader, 1), b"/");
    frame(&mut app, wid);
    let mark = top_cells(&app, wid)[tag.cols.0 - 2];
    assert_eq!(mark.ch, '\u{25d0}', "◐ before the tag");
    let paper = aterm_types::scheme::builtin("Default Light")
        .expect("a built-in")
        .background;
    let ground = [paper.r, paper.g, paper.b];
    assert_eq!(mark.bg, ground, "the tab's own paper is the rule's ground");
    let pack = |c: [u8; 3]| u32::from_be_bytes([0, c[0], c[1], c[2]]);
    let reads = |a: [u8; 3], b: [u8; 3]| contrast_ratio(pack(a), pack(b));
    let accent = crate::chrome_band::band_colors(app.theme).accent;
    assert!(
        reads(accent, ground) < 1.5,
        "CONTROL: the instance's accent reads {:.2} on the tab's paper",
        reads(accent, ground)
    );
    assert!(
        reads(mark.fg, ground) >= aterm_effects::cli_marks::NAME_CONTRAST,
        "the mark reads {:.2} on the tab's paper",
        reads(mark.fg, ground)
    );
}
