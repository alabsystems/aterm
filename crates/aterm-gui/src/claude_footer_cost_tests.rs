// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What the footer's per-frame pass costs on one Claude Code pane — a real
//! engine, a real frame through the capture route's splices, the real key
//! and splice (`App::claude_footer_fp`, `App::splice_claude_footer`): at
//! rest nothing — no allocation, no reading, no composition; per keystroke
//! into the composer and per row Claude streams, the rows' rebuild and no
//! reading the changed rows cannot move — and every fact still on the glass
//! the frame its source changes. With the probe that measures it
//! ([`footer_pass_cost_probe`], run by name).

use super::*;

use std::time::Instant;

use aterm_agent::harness::footer::FactsOwner;
use aterm_agent::harness::session_usage::{ModelTokens, More, UsageFacts};

/// The pane: an idle Claude Code session well into a conversation.
const ROWS: u16 = 40;
const COLS: u16 = 120;

/// Bypass, the mode the owner expects: no light is drawn.
const BYPASS: &str = "  \u{23F5}\u{23F5} bypass permissions on (shift+tab to cycle)";
/// Plan: the mode light deviates and is drawn, `⏸ plan`.
const PLAN: &str = "  \u{23F8} plan mode on (shift+tab to cycle)";
/// The last turn's done row, the status row over the composer.
const DONE: &str = "\u{273B} Baked for 4s \u{b7} done 2:41 PM";

/// What a screen shows under the transcript: the status row, the draft's
/// rows in the composer and the rows under its bottom rule.
#[derive(Clone, Copy)]
struct Live<'a> {
    status: &'a str,
    draft: &'a [&'a str],
    under: &'a [&'a str],
}

const AT_REST: Live<'static> = Live {
    status: DONE,
    draft: &[""],
    under: &[BYPASS],
};

impl Live<'_> {
    /// The row the composer's bottom rule is on.
    fn bottom(&self) -> usize {
        usize::from(ROWS) - 1 - self.under.len()
    }
}

/// A Claude Code screen `ROWS` × `COLS` well into a conversation: the
/// transcript (Claude's `⏺` messages, continued at column two, a blank
/// between), the status row, a blank, the composer between its rules — its
/// caret row and any further draft rows — and the rows under it.
fn screen(live: &Live<'_>) -> Vec<String> {
    let (rows, cols) = (usize::from(ROWS), usize::from(COLS));
    let rule = "\u{2500}".repeat(cols);
    let mut s: Vec<String> = (0..rows - 4 - live.draft.len() - live.under.len())
        .map(|i| match i % 4 {
            0 => format!("\u{23FA} Message {i}: what Claude said about the change, as it says it."),
            3 => String::new(),
            _ => format!("  line {i} of it, continued at column two as Claude writes a message."),
        })
        .collect();
    s.extend([live.status.to_owned(), String::new(), rule.clone()]);
    for (k, row) in live.draft.iter().enumerate() {
        s.push(if k == 0 {
            format!("\u{276F} {row}")
        } else {
            format!("  {row}")
        });
    }
    s.push(rule);
    s.extend(live.under.iter().map(|row| (*row).to_owned()));
    assert_eq!(s.len(), rows);
    s
}

/// Paint `rows` into `session`'s terminal, the rules in Claude's gray, the
/// caret on the composer.
fn feed(app: &App, session: u64, rows: &[String]) {
    let mut out = String::from("\x1b[2J");
    for (i, row) in rows.iter().enumerate() {
        let sgr = if row.starts_with('\u{2500}') {
            "\x1b[38;2;90;90;90m"
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
    bytes(app, session, out.as_bytes());
}

fn bytes(app: &App, session: u64, bytes: &[u8]) {
    let term = app.pool.get(session).expect("session").term.clone();
    crate::term_lock(&term).process(bytes);
}

/// The facts a long-running session shows: model, effort, path, branch and
/// usage, its build and owner.
fn facts() -> FooterFacts {
    FooterFacts {
        model: Some("Opus 5.5".into()),
        effort: Some("xhigh".into()),
        path: Some("~/aterm".into()),
        branch: Some("main".into()),
        version: Some("2.1.283".into()),
        usage: usage(48_000_000),
        owner: Some(FactsOwner {
            floor: Some(0),
            session_id: "0b5e7c1d-9a1f-4c2e-8d1b-2f3a4b5c6d7e".into(),
        }),
        ..FooterFacts::default()
    }
}

fn usage(input: u64) -> Option<UsageFacts> {
    UsageFacts::of(
        (
            vec![ModelTokens {
                label: "opus".into(),
                input,
                output: 310_000,
            }],
            More::default(),
        ),
        None,
        None,
    )
}

fn publish(app: &App, session: u64, facts: FooterFacts) {
    let entry = app.pool.get(session).expect("session");
    let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
    timeline.note_foreground_group(4242);
    assert!(timeline.set_claude_footer(4242, None, Some(facts)));
}

/// The window: one Claude Code pane `ROWS` × `COLS` showing `live`, `facts`
/// published, two frames drawn (every buffer grown).
fn glass_with(live: &Live<'_>, facts: FooterFacts) -> (App, WindowId, u64) {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let session = app.focused_session_id(wid).expect("the front session");
    assert!(app.apply_term_resize(wid, ROWS, COLS));
    feed(&app, session, &screen(live));
    publish(&app, session, facts);
    for _ in 0..2 {
        refill(&mut app, wid);
        let _ = pass(&mut app, wid);
    }
    (app, wid, session)
}

fn probe_glass() -> (App, WindowId, u64) {
    glass_with(&AT_REST, facts())
}

/// Extract the frame and prepend the strip, as the capture route does —
/// what a frame whose grid moved does before the footer's pass.
fn refill(app: &mut App, wid: WindowId) {
    app.capture_words(wid, Instant::now());
    app.splice_tab_strip(wid);
}

/// The footer's pass: its repaint key and its splice. The key's and the
/// splice's work alone — the frame's extraction is [`refill`]'s, and the
/// layout the frame's own.
fn pass(app: &mut App, wid: WindowId) -> u64 {
    let (plan, route) = app.active_visible_frame_layout(wid).expect("a layout");
    pass_on(app, wid, &plan, route)
}

fn pass_on(
    app: &mut App,
    wid: WindowId,
    plan: &crate::tab_model::VisibleLeafPlan,
    route: VisibleContentRoute,
) -> u64 {
    let fp = app.claude_footer_fp(wid, plan);
    app.splice_claude_footer(wid, plan, route);
    fp
}

/// What one pass cost the footer, counter by counter.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Cost {
    allocations: usize,
    /// Rebuilds of the pane's text.
    rebuilds: usize,
    /// The pane's rows read for the footer and the lights.
    reads: usize,
    /// The composer's frame searched for again to keep a reading.
    searches: usize,
    /// The rule read off the frame for its coverable cells.
    masks: usize,
    /// Launch cards read for an open model or effort.
    cards: usize,
    /// Footer rules fitted and composed.
    composed: usize,
    /// Chrome band tones derived.
    tones: usize,
}

/// Run one pass on `wid` and say what it cost: the counters are taken
/// before it, so a cost is this pass's alone.
fn cost(app: &mut App, wid: WindowId) -> Cost {
    let (plan, route) = app.active_visible_frame_layout(wid).expect("a layout");
    let _ = (
        crate::work_counts::take_pane_text_rebuilds(),
        crate::work_counts::take_footer_pane_reads(),
        crate::work_counts::take_footer_frame_searches(),
        crate::work_counts::take_footer_rule_masks(),
        crate::work_counts::take_footer_card_reads(),
        crate::work_counts::take_footer_rules_composed(),
        crate::work_counts::take_band_derives(),
    );
    let (_, allocations) = crate::work_counts::allocations_in(|| pass_on(app, wid, &plan, route));
    Cost {
        allocations,
        rebuilds: crate::work_counts::take_pane_text_rebuilds(),
        reads: crate::work_counts::take_footer_pane_reads(),
        searches: crate::work_counts::take_footer_frame_searches(),
        masks: crate::work_counts::take_footer_rule_masks(),
        cards: crate::work_counts::take_footer_card_reads(),
        composed: crate::work_counts::take_footer_rules_composed(),
        tones: crate::work_counts::take_band_derives(),
    }
}

/// The pane's row `row` as it reads on `wid`'s glass (`read_glass`: the set
/// facts in the order they are drawn).
fn glass_row(app: &App, wid: WindowId, row: usize) -> String {
    let ws = &app.windows[&wid];
    let strip = ws.input_scratch.cells.len() - usize::from(ws.rows);
    read_glass(&ws.input_scratch.cells[strip + row])
}

/// A change to the pane's screen, the `i`th of its kind.
type Change = fn(&App, u64, usize);

/// A key typed into the composer: its echo at the caret.
fn keystroke(app: &App, session: u64, i: usize) {
    let caret = AT_REST.bottom();
    let ch = char::from(b'a' + (i % 26) as u8);
    let col = 3 + i % 40;
    bytes(
        app,
        session,
        format!("\x1b7\x1b[{caret};{col}H{ch}\x1b8").as_bytes(),
    );
}

/// A row Claude streams into the transcript, well above the status row.
fn streamed(app: &App, session: u64, i: usize) {
    let row = 5 + i % 20;
    bytes(
        app,
        session,
        format!(
            "\x1b7\x1b[{row};1H\u{23FA} streamed line {i}: the next words of the reply\x1b[K\x1b8"
        )
        .as_bytes(),
    );
}

/// A turn running: the status row's spinner ticks (and its seconds).
fn spinner(app: &App, session: u64, i: usize) {
    let status = AT_REST.bottom() - 3;
    let glyph = ['\u{2722}', '\u{2733}', '\u{2736}', '\u{273B}'][i % 4];
    bytes(
        app,
        session,
        format!(
            "\x1b7\x1b[{status};1H{glyph} Thinking\u{2026} ({}s \u{b7} esc to interrupt)\x1b[K\x1b8",
            i / 10
        )
        .as_bytes(),
    );
}

/// THE FOOTER AT REST COSTS NOTHING: a frame on a Claude Code pane whose
/// screen and facts did not move — model, effort, path, branch and usage
/// shown — allocates nothing in the footer's pass, frame after frame: no
/// copy of the facts, no reading of the pane, no read of its rule, no fit,
/// no composition, no band tone derived; the glass the same.
///
/// NEGATIVE CONTROLS: a fact published anew (the usage's tokens, then the
/// branch) composes the rule once, allocates for its copy and is on the
/// glass that frame, the pane not read again — then rests at nothing; and
/// the first frame of a fresh window reads the pane and composes its rule
/// (the counters are wired to the work).
#[test]
fn a_frame_at_rest_costs_the_footer_nothing() {
    let (mut app, wid, session) = probe_glass();
    let rule = AT_REST.bottom();
    let at_rest = glass_row(&app, wid, rule);
    assert!(
        at_rest.contains("Opus 5.5 xhigh") && at_rest.contains("main"),
        "the facts are on the rule: {at_rest:?}"
    );
    for i in 0..30 {
        assert_eq!(cost(&mut app, wid), Cost::default(), "frame {i}");
        assert_eq!(
            glass_row(&app, wid, rule),
            at_rest,
            "frame {i}: the same glass"
        );
    }
    // CONTROL: the usage moves, then the branch.
    let more = FooterFacts {
        usage: usage(49_000_000),
        ..facts()
    };
    let branch = FooterFacts {
        branch: Some("perf/rest".into()),
        ..more.clone()
    };
    let key = pass(&mut app, wid);
    assert_eq!(pass(&mut app, wid), key, "the repaint key stands at rest");
    let mut keys = vec![key];
    for (what, facts) in [("49M", more), ("perf/rest", branch)] {
        publish(&app, session, facts);
        let (plan, _) = app.active_visible_frame_layout(wid).expect("a layout");
        let key = app.claude_footer_fp(wid, &plan);
        assert!(
            !keys.contains(&key),
            "CONTROL {what}: the repaint key moved"
        );
        keys.push(key);
        let moved = cost(&mut app, wid);
        assert!(
            moved.composed == 1 && moved.allocations > 0 && moved.reads == 0,
            "CONTROL {what}: composed anew, the pane not read again ({moved:?})"
        );
        assert!(
            glass_row(&app, wid, rule).contains(what),
            "CONTROL {what}: on the glass: {:?}",
            glass_row(&app, wid, rule)
        );
        assert_eq!(
            cost(&mut app, wid),
            Cost::default(),
            "{what}: at rest again"
        );
        assert_eq!(
            pass(&mut app, wid),
            key,
            "{what}: the key it moved to stands"
        );
    }
    // CONTROL: a first frame reads and composes.
    let mut fresh = App::headless_for_test();
    let fresh_wid = WindowId(0);
    let fresh_session = fresh.focused_session_id(fresh_wid).expect("a session");
    assert!(fresh.apply_term_resize(fresh_wid, ROWS, COLS));
    feed(&fresh, fresh_session, &screen(&AT_REST));
    publish(&fresh, fresh_session, facts());
    refill(&mut fresh, fresh_wid);
    let first = cost(&mut fresh, fresh_wid);
    assert!(
        first.rebuilds == 1 && first.reads == 1 && first.masks == 1 && first.composed == 1,
        "CONTROL: the first frame reads and composes ({first:?})"
    );
}

/// The window's harness host, for the harness's chips to be drawn: one that
/// supervises nothing, shut down when the guard drops (a failing test's
/// included).
struct Hosted(crate::harness_host::HostHandle);

impl Drop for Hosted {
    fn drop(&mut self) {
        self.0.shutdown_and_join();
    }
}

fn hosted(app: &mut App) -> Hosted {
    let host = crate::harness_host::HostHandle::for_supervised_test(
        Vec::new(),
        std::sync::Arc::new(|_| crate::harness_host::BodyEnd::Stopped),
    );
    app.harness = Some(host.clone());
    Hosted(host)
}

/// A HOVERED CHIP COSTS ITS TITLE ALONE: with the pointer resting on a
/// harness chip, a frame whose screen and facts did not move builds the
/// chip's title — whole and short: two allocations, each sized once, v1's
/// count — and does nothing else the footer counts: no reading, no fit, no
/// composition (the rule's cells are the last frame's). The Autopilot
/// chip's title, made from the face table's parts, costs what `⚙`'s static
/// one does.
///
/// CONTROLS: the chips on the rule at rest cost nothing; the pointer gone,
/// nothing again.
#[test]
fn a_hovered_chip_costs_its_title_alone() {
    use crate::claude_lights::Chip;
    let (mut app, wid, session) = probe_glass();
    let _host = hosted(&mut app);
    for _ in 0..2 {
        let _ = pass(&mut app, wid);
    }
    let rule = AT_REST.bottom();
    assert!(
        glass_row(&app, wid, rule).contains('\u{221e}'),
        "CONTROL: Autopilot's chip on the rule: {:?}",
        glass_row(&app, wid, rule)
    );
    assert_eq!(
        cost(&mut app, wid),
        Cost::default(),
        "CONTROL: chips at rest"
    );
    for chip in [Chip::Autopilot, Chip::Settings] {
        app.hover_claude_chip(wid, Some((session, chip)));
        for _ in 0..2 {
            let _ = pass(&mut app, wid);
        }
        for i in 0..12 {
            assert_eq!(
                cost(&mut app, wid),
                Cost {
                    allocations: 2,
                    ..Cost::default()
                },
                "{chip:?} hovered, frame {i}"
            );
        }
    }
    app.hover_claude_chip(wid, None);
    for _ in 0..2 {
        let _ = pass(&mut app, wid);
    }
    assert_eq!(
        cost(&mut app, wid),
        Cost::default(),
        "CONTROL: the pointer gone"
    );
}

/// The costs of `passes` frames that each rebuild the pane's text, `change`
/// before each refill — after a warm cycle of each change (the streamed
/// rows', the draft's), which grows the rows' text buffers it writes.
fn rebuilt(change: Change, passes: usize) -> Vec<Cost> {
    const WARM: usize = 48;
    let (mut app, wid, session) = probe_glass();
    for i in 0..WARM {
        change(&app, session, i);
        refill(&mut app, wid);
        let _ = cost(&mut app, wid);
    }
    (WARM..WARM + passes)
        .map(|i| {
            change(&app, session, i);
            refill(&mut app, wid);
            cost(&mut app, wid)
        })
        .collect()
}

/// UNCHANGED ROWS COST NO READ: a rebuild of the pane's text whose changed
/// rows cannot move what the footer and the lights read — every reading is
/// of the composer's frame and the rows under its bottom rule
/// (`aterm_phase::phase::FrameReach`) — keeps the last reading. A row Claude
/// streams into the transcript and a spinner's tick on the status row, both
/// above every row the frame's search reads: no search, no reading, no
/// composition, no allocation. A keystroke's echo on the caret row, which
/// the search does read: the frame searched for again (the same frame found),
/// and still no reading, no composition, no allocation. Every pass rebuilt
/// the text (the change reached the footer) and read its new scratch's rule
/// for the cells it may cover.
///
/// NEGATIVE CONTROLS: [`each_reading_follows_its_rows`] — a change under the
/// rule, or one that moves it, is read anew the frame it lands.
#[test]
fn unchanged_rows_cost_no_read() {
    let kept = Cost {
        rebuilds: 1,
        masks: 1,
        ..Cost::default()
    };
    let changes: [(&str, Change, usize); 3] = [
        ("streamed row", streamed, 0),
        ("spinner tick", spinner, 0),
        ("keystroke", keystroke, 1),
    ];
    for (what, change, searches) in changes {
        for (i, cost) in rebuilt(change, 12).into_iter().enumerate() {
            assert_eq!(cost, Cost { searches, ..kept }, "{what} {i}");
        }
    }
}

/// EACH READING FOLLOWS ITS ROWS: every reader of the pane's rows reads anew
/// the frame its source rows change, and the glass shows it — one change
/// per reader:
///
/// * THE RULE (`read_rule_under`: the composer's bottom rule): a row opening
///   under the mode row moves the composer up a row; the facts are written
///   on the new rule alone, none on the row the old one stood on;
/// * THE LIGHTS (`lights::reading_under`: the pill under the rule): the mode
///   row says plan, and the mode light is drawn, `⏸ plan`; back to bypass,
///   and it goes;
/// * THE LAUNCH CARD (`footer::launch_card`, while the facts leave the model
///   open): the card names a model, and the footer shows it; a new card
///   names another, and the footer shows that — then, at rest, the reading
///   is kept and nothing is read.
///
/// CONTROL: the frame before each change shows the reading before it.
#[test]
fn each_reading_follows_its_rows() {
    // The rule.
    let (mut app, wid, session) = probe_glass();
    let facts_on = |app: &App| -> Vec<usize> {
        (0..usize::from(ROWS))
            .filter(|&r| glass_row(app, wid, r).contains("Opus 5.5 xhigh"))
            .collect()
    };
    assert_eq!(facts_on(&app), [AT_REST.bottom()], "CONTROL: on the rule");
    let up = Live {
        under: &[BYPASS, ""],
        ..AT_REST
    };
    feed(&app, session, &screen(&up));
    refill(&mut app, wid);
    let moved = cost(&mut app, wid);
    assert_eq!(moved.reads, 1, "the rule's rows read anew: {moved:?}");
    assert_eq!(
        facts_on(&app),
        [up.bottom()],
        "the facts on the new rule alone"
    );
    assert_ne!(up.bottom(), AT_REST.bottom());
    // A box over the composer (Claude's permission dialog): no composer, no
    // rule, no facts; the composer back, the facts back.
    let mut boxed = screen(&AT_REST);
    let from = AT_REST.bottom() - 6;
    boxed[from] = "\u{256D}".to_owned() + &"\u{2500}".repeat(60) + "\u{256E}";
    boxed[from + 1] = "\u{2502} Do you want to make this edit to footer.rs?".to_owned();
    boxed[from + 2] = "\u{2502} \u{276F} 1. Yes".to_owned();
    boxed[from + 3] = "\u{2502}   2. No, and tell Claude what to do differently".to_owned();
    boxed[from + 4] = "\u{2570}".to_owned() + &"\u{2500}".repeat(60) + "\u{256F}";
    for row in &mut boxed[from + 5..] {
        row.clear();
    }
    for (what, rows, on) in [
        ("a box", boxed, &[][..]),
        (
            "the composer back",
            screen(&AT_REST),
            &[AT_REST.bottom()][..],
        ),
    ] {
        feed(&app, session, &rows);
        refill(&mut app, wid);
        let read = cost(&mut app, wid);
        assert_eq!(read.reads, 1, "{what}: read anew: {read:?}");
        assert_eq!(facts_on(&app), on, "{what}");
    }
    // The lights.
    let (mut app, wid, session) = probe_glass();
    let rule = AT_REST.bottom();
    assert!(
        !glass_row(&app, wid, rule).contains("plan"),
        "CONTROL: bypass draws no light"
    );
    for (mode, drawn) in [(PLAN, true), (BYPASS, false)] {
        feed(
            &app,
            session,
            &screen(&Live {
                under: &[mode],
                ..AT_REST
            }),
        );
        refill(&mut app, wid);
        let read = cost(&mut app, wid);
        assert_eq!(read.reads, 1, "{mode}: the pill read anew: {read:?}");
        assert_eq!(
            glass_row(&app, wid, rule).contains("\u{23F8} plan"),
            drawn,
            "{mode}: {:?}",
            glass_row(&app, wid, rule)
        );
    }
    // The launch card, while the model is open.
    let banner = aterm_phase::anchor_text("launch.banner");
    let card = |model: &str| -> Vec<String> {
        let mut rows = screen(&AT_REST);
        rows[1] =
            format!(" \u{2590}\u{259B}\u{2588}\u{2588}\u{2588}\u{259B}\u{2588}   {banner}2.1.283");
        rows[2] = format!(
            "\u{259D}\u{259C}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2580}  {model} \u{b7} Claude Max"
        );
        rows[3] = " \u{259D}\u{259D}   \u{259D}\u{259D}   ~/aterm".to_owned();
        for row in &mut rows[4..AT_REST.bottom() - 4] {
            row.clear();
        }
        rows
    };
    let open = FooterFacts {
        model: None,
        effort: None,
        model_open: true,
        ..facts()
    };
    let (mut app, wid, session) = glass_with(&AT_REST, open);
    assert!(
        !glass_row(&app, wid, rule).contains("Opus"),
        "CONTROL: no card, no model: {:?}",
        glass_row(&app, wid, rule)
    );
    for model in ["Opus 5.5", "Sonnet 4.5"] {
        feed(&app, session, &card(model));
        refill(&mut app, wid);
        let read = cost(&mut app, wid);
        assert_eq!(read.cards, 1, "{model}: the card read anew: {read:?}");
        assert!(
            glass_row(&app, wid, rule).contains(model),
            "{model}: {:?}",
            glass_row(&app, wid, rule)
        );
        assert_eq!(
            cost(&mut app, wid),
            Cost::default(),
            "{model}: the card's reading kept, at rest"
        );
    }
}

/// `same_text` reads a row exactly as its chars: one byte or four, a wide
/// char, an empty row, a text a char longer or shorter, a char a byte off.
/// CONTROL: the chars' own comparison, on every pair.
#[test]
fn a_row_compares_as_its_chars() {
    let cells = |text: &str| -> Vec<RenderCell> {
        text.chars()
            .map(|ch| RenderCell {
                ch,
                ..RenderCell::default()
            })
            .collect()
    };
    let texts = [
        "",
        "a",
        "ab",
        "abc",
        "ab ",
        "\u{276F} draft",
        "\u{276F} drafu",
        "\u{2500}\u{2500}",
        "\u{2500}\u{2501}",
        "\u{4E2D} ",
        "\u{4E2D}",
        "\u{1F600}x",
        "\u{1F601}x",
        "a\u{e9}",
        "a\u{e8}",
    ];
    for text in texts {
        for row in texts {
            let row_cells = cells(row);
            assert_eq!(
                same_text(text, Some(&row_cells)),
                text == row,
                "{text:?} as {row:?}"
            );
        }
        assert_eq!(same_text(text, None), text.is_empty(), "{text:?}, no row");
    }
}

/// THE PROBE (run by name, optimized: `targo --unverified test --config
/// 'profile.probe.inherits="dev"' --config 'profile.probe.opt-level=3'
/// --config 'profile.probe.overflow-checks=false' --profile probe -p
/// aterm-gui --lib footer_pass_cost_probe -- --ignored --nocapture`): for
/// each kind of frame, the footer's pass's allocations (the test binary's
/// counting allocator, this thread's) and its time, p50/p90/p99 µs over
/// many passes, the frame's extraction outside the timing, and the work it
/// counted.
#[test]
#[ignore = "a measurement, run by name"]
fn footer_pass_cost_probe() {
    const PASSES: usize = 4000;
    // The last: at rest with a harness host and the pointer on Autopilot's
    // chip, whose title a hovered frame builds.
    let kinds: [(&str, Option<Change>, bool); 5] = [
        ("at rest", None, false),
        ("keystroke", Some(keystroke), false),
        ("streamed row", Some(streamed), false),
        ("spinner tick", Some(spinner), false),
        ("hovered chip", None, true),
    ];
    for (what, step, hovered) in kinds {
        let (mut app, wid, session) = probe_glass();
        let _host = hovered.then(|| {
            let host = hosted(&mut app);
            app.hover_claude_chip(wid, Some((session, crate::claude_lights::Chip::Autopilot)));
            host
        });
        let frame = |app: &mut App, i: usize| {
            if let Some(step) = step {
                step(app, session, i);
                refill(app, wid);
            }
        };
        // Warm: every buffer the change grows, grown.
        for i in 0..64 {
            frame(&mut app, i);
            let _ = pass(&mut app, wid);
        }
        let mut work = Cost::default();
        let mut allocations = 0;
        let mut micros = Vec::with_capacity(PASSES);
        for i in 64..64 + 2 * PASSES {
            frame(&mut app, i);
            if i % 2 == 0 {
                let c = cost(&mut app, wid);
                allocations = allocations.max(c.allocations);
                work.rebuilds += c.rebuilds;
                work.reads += c.reads;
                work.searches += c.searches;
                work.masks += c.masks;
                work.cards += c.cards;
                work.composed += c.composed;
                work.tones += c.tones;
            } else {
                let (plan, route) = app.active_visible_frame_layout(wid).expect("a layout");
                let t = Instant::now();
                let _ = std::hint::black_box(pass_on(&mut app, wid, &plan, route));
                micros.push(t.elapsed().as_secs_f64() * 1e6);
            }
        }
        micros.sort_by(f64::total_cmp);
        let at = |q: f64| micros[((micros.len() - 1) as f64 * q) as usize];
        println!(
            "PROBE {what:>12}: allocations/pass max {allocations} | µs/pass p50 {:.2} p90 {:.2} p99 {:.2} | work per {PASSES} passes: {work:?}",
            at(0.5),
            at(0.9),
            at(0.99),
        );
    }
}
