// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE TOP RULE'S KEY, CLICKED (design §10.6.14): Claude Code's own
//! top-effort tag on its composer's top rule, and aterm's greyed ghost of it
//! while the key is off — drawn only on evidence the key exists for the
//! process, never over Claude's ink — each a target whose left click types
//! Claude's own `/effort <key> on|off` through the facts' one typing path
//! (`crate::claude_slash`), read back from the engine. On the slash tests'
//! rig: a real engine, the session's PTY sink, the person clock.

use aterm_core::terminal::{RenderCell, Script, UnderlineStyle};
use aterm_window::event::{ElementState, MouseButton};

use super::*;
use crate::claude_lights::FactHit;
#[cfg(target_os = "macos")]
use crate::claude_lights::Target;
use crate::claude_slash::KeyLatch;

/// The ghost's text: ` KEY off `.
fn ghost_text() -> String {
    format!(" {KEY} off ")
}

/// [`claude_on`] with the build `version` and the key as the transcript
/// last stated it (`top`: the resolver's `FooterFacts::top_effort`).
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

/// The frame row of the composer's top rule [`draw_claude`] draws.
fn top_row(app: &App, wid: WindowId) -> usize {
    footer_row(app, wid) - 2
}

/// The top rule's cells as this frame's glass holds them.
fn top_cells(app: &App, wid: WindowId) -> Vec<RenderCell> {
    app.windows[&wid].input_scratch.cells[top_row(app, wid)].clone()
}

/// The top rule as this frame's glass shows it, char by char.
fn top_text(app: &App, wid: WindowId) -> String {
    top_cells(app, wid).iter().map(|c| c.ch).collect()
}

/// The key's target this frame, where the glass shows it.
fn key_hit(app: &App, wid: WindowId) -> Option<FactHit> {
    app.windows[&wid]
        .claude_lights
        .fact_hits
        .iter()
        .find(|h| h.fact == Fact::TopEffort)
        .copied()
}

/// A left click on frame cell `cell`, the pointer moved there first.
fn click_at(app: &mut App, wid: WindowId, cell: (usize, usize)) {
    let (x, y) = px_of(app, wid, cell);
    app.on_cursor_moved(wid, x, y);
    app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
    app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
}

/// Whether this frame's footer write touched any of frame row `row`'s
/// columns `cols`.
fn wrote(app: &App, wid: WindowId, row: usize, cols: (usize, usize)) -> bool {
    app.windows[&wid]
        .claude_footer_undo
        .painted_spans()
        .any(|(at, col, len)| at == row && col < cols.1 && col + len > cols.0)
}

/// The rest of a press of `slash` once its probe is on the PTY: the probe's
/// echo, the rest, its echo, the quiet window and the Enter.
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

/// THE GHOST IS DRAWN ONLY ON EVIDENCE THE KEY IS OFF (design §10.6.14): Claude
/// Code 2.1.304 with no word of the key draws nothing of aterm's on its top
/// rule's right end and offers no target; the transcript's statement of the
/// key makes ` KEY off ` stand in Claude's slot — its glyphs a target, the
/// `─` after it Claude's — and so does Claude's own tag seen earlier for the
/// process. While Claude draws its tag, its cells are the engine's, byte for
/// byte, never written, and they are the target; no ghost is drawn.
///
/// NEGATIVE CONTROLS: Claude Code 2.1.302 (no session toggle to type), and
/// the key latched off by Claude's refusal for the process and its model,
/// draw nothing and offer nothing — another model lets the latch go.
#[test]
fn the_ghost_is_drawn_only_on_evidence_of_off() {
    let (mut app, wid, session, _reader) = idle();
    frame(&mut app, wid);
    let w = cols(&app, session);
    assert!(!top_text(&app, wid).contains(&ghost_text()), "no evidence");
    assert_eq!(key_hit(&app, wid), None, "and no target");
    // The transcript stated it: off.
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
    for cell in &top_cells(&app, wid)[w - 15..w - 2] {
        assert_eq!(
            (cell.fg, cell.bold, cell.script),
            (rule.fg, false, Script::Normal),
            "Claude's rule gray, not bold, full size"
        );
    }
    // Claude's tag live: its own cells, never written, the target.
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
    // The tag gone, no statement: the tag seen is the evidence.
    claude_with(&app, session, "Opus 5.5", "2.1.304", None);
    answered(&app, session, TOP, false);
    frame(&mut app, wid);
    assert!(
        top_text(&app, wid).contains(&ghost_text()),
        "seen: the ghost"
    );
    // CONTROL: an older build.
    claude_with(&app, session, "Opus 5.5", "2.1.302", Some(false));
    frame(&mut app, wid);
    assert!(
        !top_text(&app, wid).contains(&ghost_text()),
        "2.1.302: none"
    );
    assert_eq!(key_hit(&app, wid), None);
    // CONTROL: latched off for the process and its model.
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

/// A CLICK ON THE GHOST TYPES THE KEY ON AND READS IT BACK: the left click
/// writes the probe `/` alone as the person's, the ghost marked `◐` in the
/// `─` before it while the press runs; the rest and aterm's Enter follow
/// the echo; Claude's tag on the top rule ends it — the ghost gone, Claude's
/// tag the target, its notice in the title slot, the process noted as
/// having shown the key. Hovered, the ghost is lit (lifted, underlined).
/// NEGATIVE CONTROL: a click one cell left of the ghost's glyphs types
/// nothing.
#[test]
fn a_click_on_the_ghost_types_key_on_and_reads_it_back() {
    let (mut app, wid, session, mut reader) = idle();
    claude_with(&app, session, "Opus 5.5", "2.1.304", Some(false));
    frame(&mut app, wid);
    let hit = key_hit(&app, wid).expect("the ghost");
    let row = hit.frame_row;
    // CONTROL: the ghost's leading blank is no target.
    click_at(&mut app, wid, (row, hit.cols.0 - 1));
    assert!(pty(&mut reader, 0).is_empty(), "CONTROL: nothing typed");
    assert_eq!(stage(&app, session), None, "CONTROL: no press");
    let human = human_seq(&app, session);
    // Hovered: lit (underlined; lifted toward the pane's ink, which this
    // rule's gray already is).
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

/// A BAND OVER THE BOTTOM RULE LEAVES THE TOP RULE'S KEY (the cross-track
/// review of 2026-10-19): a find bar floated to the pane's bottom claims the
/// composer's caret row, its bottom rule and the mode row — no bottom rule
/// is on the glass to write — while the top rule stands clear of it: its
/// ghost and its target stay, and nothing under the band is written.
/// CONTROL: the frame before, with no band, shows them.
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
    // One band, the find bar's rows: a list of one range, not the rows.
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

/// A DOUBLE-CLICK ON THE KEY, NO BLOCK OF CHIPS SHOWN (no harness host):
/// the first click begins the press; the second, refused as one already in
/// flight, opens no menu over the pane while aterm types, and types nothing
/// more. NEGATIVE CONTROL: a refusal of another kind there (a draft in the
/// composer) opens the effort menu hung from the key, under its note.
#[test]
fn a_double_click_on_the_key_opens_no_menu_while_aterm_types() {
    for draft in [false, true] {
        let (mut app, wid, session, mut reader) = idle();
        claude_with(&app, session, "Opus 5.5", "2.1.304", Some(false));
        // Bypass draws no chip: no block.
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
/// process showed the key). NEGATIVE CONTROL: a keyboard walk selects the
/// key last, after the bottom rule's facts, and Return on it is the same
/// click.
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
    // CONTROL: the keyboard's walk ends on the key; Return is its click.
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

/// A REFUSED KEY LATCHES THE GHOST FOR THAT MODEL: Claude's measured
/// `isn't available on <model>` answer to a click on the ghost stops the
/// press naming the model, and the ghost is drawn no more — no target —
/// for the process and its model. NEGATIVE CONTROL: another model in the
/// footer lets the latch go, and the ghost is back.
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
    // CONTROL: another model.
    claude_with(&app, session, "Sonnet 5.5", "2.1.304", Some(false));
    frame(&mut app, wid);
    assert!(top_text(&app, wid).contains(&ghost_text()), "a new model");
    assert!(key_hit(&app, wid).is_some());
}

/// This window's harness host holding `session`: its top rule wears the
/// harness's border and its switch. The guard shuts the host down.
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

/// THE GHOST SHOWS WITH THE HARNESS OFF AND WRITES ONLY ITS CELLS: the
/// session's Autopilot Off in a window with no host leaves the top rule
/// Claude's — no tint, no switch — but for the lockup two cells in (it shows
/// in every harness state) and the ghost, whose cells end the write: the
/// rule between them is Claude's own `─`, copied as it stands, its lead and
/// its last `─` never written. A refused click says why in the lights' title slot
/// where a block of chips is drawn, and where none is (bypass: no chip)
/// opens the effort menu hung from the ghost, the refusal its note and its
/// typed rows disabled. NEGATIVE CONTROL: in a window whose host holds the
/// session, the harness's border on, the same rule's write covers the run.
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
    // A refused click (a draft in the composer) beside a block of chips:
    // said in its title slot.
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
    // No block (bypass draws no chip): the effort menu, hung from the key.
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
/// top rule calls for, and it stays while the key does. NEGATIVE CONTROL:
/// the pointer off the key, its title is gone.
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
    // Selected, `↓`: the effort's menu, hung from the key.
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
/// `<Key>: off` while the ghost stands. NEGATIVE CONTROL: with no ghost and
/// no tag there is no node, and the press types nothing.
#[cfg(a11y_tree)]
#[test]
fn a_readers_press_on_the_key_is_its_click() {
    use crate::a11y_tree::HarnessChip;
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
    app.press_claude_chip_for_reader(wid, session, HarnessChip::TopEffort);
    assert_eq!(pty(&mut reader, 1), b"/", "the click's probe");
}

/// THE KEY'S MARK READS ON A TAB'S OWN THEME (design §8; `session_theme`):
/// the `◐` a press of the key stands under, in the `─` left of Claude's tag,
/// is the instance theme's accent, and a tab drawn in a theme of its own
/// keeps the instance's (the window's chrome follows the instance) — so on
/// an instance in Dracula a Claude Code tab in Default Light drew Dracula's
/// light accent on Default Light's paper at 1.0:1. The accent is floored
/// along its hue on the rule's own ground, as on Codex's slab, so the mark
/// reads at 4.5:1 there.
///
/// NEGATIVE CONTROL: the instance's accent as chosen reads under 1.5:1 on
/// the tab's ground.
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
