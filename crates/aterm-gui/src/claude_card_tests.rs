// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The harness card on the glass: a real engine, a real frame through the
//! capture route's splices, the real splice — and the layout's rules on
//! hand-made cells.

use super::*;

use std::sync::Arc;
use std::time::Duration;

use aterm_agent::harness::footer::FooterFacts;
use aterm_agent::supervise::Autopilot;
use aterm_effects::harness_card::EPISODE_MS;

/// The build the screens' card names, and the process the facts are for.
const VERSION: &str = "2.1.283";
const PGID: i32 = 4242;

/// The model row of the screens' card: short, so the kitty has room.
const MODEL: &str = "Opus 5.5 \u{b7} Claude Max";

/// [`MODEL`] with its last glyph wide: as many chars (so the card's place
/// reads the same), a cell longer on the glass.
const MODEL_WIDE_END: &str = "Opus 5.5 \u{b7} Claude Ma\u{4E2D}";

/// The card's working directory is drawn in this gray, as Claude draws it.
const GRAY: [u8; 3] = [130, 130, 130];

/// The vendor's banner text (anchor `launch.banner`), from the one table.
fn banner() -> &'static str {
    aterm_phase::anchor_text("launch.banner")
}

/// A Claude Code screen `rows` × `cols`: a blank row, the wide launch card
/// (its text at column 11, its build [`VERSION`] or `version`), blank rows,
/// then the composer between its rules and the mode row at the bottom.
fn card_screen(rows: usize, cols: usize, model: &str, version: &str) -> Vec<String> {
    let rule = "\u{2500}".repeat(cols);
    let mut screen = vec![
        String::new(),
        format!(
            " \u{2590}\u{259B}\u{2588}\u{2588}\u{2588}\u{259B}\u{2588}   {}{version}",
            banner()
        ),
        format!(
            "\u{259D}\u{259C}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2580}  {model}"
        ),
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

/// Paint `rows` into `session`'s terminal: the working directory row in
/// [`GRAY`], the rest in the default ink, the caret on the composer.
fn feed(app: &App, session: u64, rows: &[String]) {
    let mut out = String::from("\x1b[2J");
    for (i, row) in rows.iter().enumerate() {
        let sgr = if row.ends_with("~/aterm") {
            format!("\x1b[38;2;{};{};{}m", GRAY[0], GRAY[1], GRAY[2])
        } else {
            String::new()
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

/// The footer facts of a Claude Code process of build `version`, in front.
fn publish(app: &App, session: u64, version: &str) {
    let entry = app.pool.get(session).expect("session");
    let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
    timeline.note_foreground_group(PGID);
    timeline.set_claude_footer(
        PGID,
        None,
        Some(FooterFacts {
            model: Some("Opus 5.5".into()),
            version: Some(version.into()),
            ..FooterFacts::default()
        }),
    );
}

/// This window's host, holding `session`'s live supervisor claim: the
/// session is SUPERVISED (`App::session_autopilot`). The caller shuts the
/// host down.
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

/// One frame through the capture route's order (`App::render_image`'s
/// terminal arm): extract with the cursor effects, publish the word streams,
/// prepend the strip, then the footer's splice.
fn frame(app: &mut App, wid: WindowId) {
    app.capture_words(wid, Instant::now());
    let (plan, route) = app.active_visible_frame_layout(wid).expect("a layout");
    app.splice_tab_strip(wid);
    app.splice_claude_footer(wid, &plan, route);
}

/// A headless window `cols` wide and 24 rows tall, its front session, its
/// card on the screen and the facts of its process published.
fn glass(cols: u16) -> (App, WindowId, u64) {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let session = app.focused_session_id(wid).expect("the front session");
    if cols != 80 {
        assert!(app.apply_term_resize(wid, 24, cols));
    }
    feed(
        &app,
        session,
        &card_screen(24, usize::from(cols), MODEL, VERSION),
    );
    publish(&app, session, VERSION);
    (app, wid, session)
}

/// The chrome rows above the terminal grid on `wid`'s frame.
fn strip_rows(app: &App, wid: WindowId) -> usize {
    app.windows[&wid].input_scratch.cells.len() - usize::from(app.windows[&wid].rows)
}

/// The text of frame row `row`, columns `cols`.
fn glass_text(app: &App, wid: WindowId, row: usize, cols: std::ops::Range<usize>) -> String {
    app.windows[&wid].input_scratch.cells[row]
        .iter()
        .skip(cols.start)
        .take(cols.len())
        .map(|c| c.ch)
        .collect()
}

/// The card's record of what it put in the streams.
fn painted(app: &App, wid: WindowId) -> &CardPainted {
    &app.windows[&wid].claude_card.painted
}

/// A painted card's two light streams, `(under, over)`, as they stand.
fn card_streams(app: &App, wid: WindowId) -> (Vec<GlowQuad>, Vec<GlowQuad>) {
    let p = painted(app, wid);
    (p.under.clone(), p.over.clone())
}

/// The kitty's left edge in a card's streams: its opaque quads' leftmost x.
fn kitty_x((_, over): &(Vec<GlowQuad>, Vec<GlowQuad>)) -> Option<u16> {
    over.iter().filter(|q| q.alpha == 255).map(|q| q.x).min()
}

/// The span the trail's quads cover in a card's streams: its `under` quads
/// clear of the border's stroke (which holds the stream's outermost
/// pixels), left to right.
fn trail_span((under, _): &(Vec<GlowQuad>, Vec<GlowQuad>)) -> Option<(u32, u32)> {
    let end = |q: &GlowQuad| {
        (
            u32::from(q.x) + u32::from(q.w),
            u32::from(q.y) + u32::from(q.h),
        )
    };
    let x0 = under.iter().map(|q| u32::from(q.x)).min()?;
    let y0 = under.iter().map(|q| u32::from(q.y)).min()?;
    let x1 = under.iter().map(|q| end(q).0).max()?;
    let y1 = under.iter().map(|q| end(q).1).max()?;
    let inside = under
        .iter()
        .filter(|q| u32::from(q.x) > x0 && u32::from(q.y) > y0 && end(q).0 < x1 && end(q).1 < y1);
    let a = inside.clone().map(|q| u32::from(q.x)).min()?;
    let b = inside.map(|q| end(q).0).max()?;
    Some((a, b))
}

/// The line the card writes for `view` in a pane `cols` wide with its star at
/// column `at`: the widest of [`line_texts`] that ends inside the border.
fn expected_line(view: AutopilotView, at: usize, cols: usize) -> String {
    line_texts(view, LineEnd::Mode)
        .iter()
        .find(|t| at + LINE_LEAD + text_cells(t) < cols)
        .expect("a line fits")
        .clone()
}

/// THE WRAPPED CARD (§3): Cruise under this host's live claim — the border
/// and the trail in `glow_under` (window pixels, additive on a dark ground,
/// every quad on one of the box's frame rows), the kitty and the stars in
/// `nova_add` (the kitty opaque art), and the line `✦ enhanced by aterm
/// harness <version> · autopilot ∞ cruise` written on the glass under the
/// card at its text column, in the footer's value colour, inked, behind a
/// drawn `Star4`.
#[test]
fn the_wrapped_card_wears_its_border_kitty_stars_and_line() {
    let (mut app, wid, session) = glass(120);
    let host = supervise(&mut app, session);
    assert_eq!(
        app.session_autopilot(session).1,
        AutopilotView::Cruise,
        "the default Autopilot"
    );
    frame(&mut app, wid);
    let strip = strip_rows(&app, wid);
    // The card's banner is pane row 1: the box is rows 1..=4 (the line's).
    let (top, text_col) = (1usize, 11usize);
    let p = painted(&app, wid);
    assert!(!p.under.is_empty(), "the border and the trail");
    assert!(!p.over.is_empty(), "the kitty and the stars");
    let scratch = &app.windows[&wid].input_scratch;
    for q in &p.under {
        assert!(scratch.glow_under.contains(q), "in the frame's stream");
        assert_eq!(q.alpha, 0, "additive light on a dark ground");
        let row = usize::from(q.row);
        assert!(
            (strip + top..strip + top + 4).contains(&row),
            "row {row}: one of the box's frame rows"
        );
    }
    for q in &p.over {
        assert!(scratch.nova_add.contains(q), "in the frame's stream");
    }
    assert!(
        p.over.iter().any(|q| q.alpha == u8::MAX),
        "the kitty: opaque art"
    );
    // Window pixels under the ink, grid-interior pixels for the nova stream:
    // the same frame row starts `grid_top` lower in the first.
    let (_, ch) = app.win_cell_size(wid);
    let grid_top = app.win_pad_top(wid) + app.win_head(wid);
    let y_of = |q: &GlowQuad| usize::from(q.y);
    assert!(
        p.under
            .iter()
            .all(|q| y_of(q) >= grid_top + usize::from(q.row) * ch)
    );
    assert!(
        p.over
            .iter()
            .all(|q| y_of(q) >= usize::from(q.row) * ch && y_of(q) < (usize::from(q.row) + 1) * ch)
    );
    // The line.
    let line_row = strip + top + 3;
    let want = expected_line(AutopilotView::Cruise, text_col, 120);
    assert!(
        want.ends_with("\u{b7} autopilot \u{221e} cruise"),
        "the whole line fits at 120 columns: {want:?}"
    );
    let start = text_col + LINE_LEAD;
    assert_eq!(
        glass_text(&app, wid, line_row, start..start + want.chars().count()),
        want
    );
    assert_eq!(
        glass_text(&app, wid, line_row, text_col..start),
        "  ",
        "the star's cell and the gap stay blank cells"
    );
    let scratch = &app.windows[&wid].input_scratch;
    let blank = scratch.implicit_blank;
    let e = &scratch.cells[line_row][start];
    assert_eq!(e.ch, 'e');
    assert_eq!(
        e.fg,
        crate::claude_footer::value_fg(GRAY, blank.fg),
        "the footer's value colour under the ink"
    );
    let inked: Vec<u16> = scratch
        .ink
        .iter()
        .filter(|c| usize::from(c.row) == line_row)
        .map(|c| c.col)
        .collect();
    assert_eq!(
        inked,
        (start..start + want.chars().count())
            .map(|c| c as u16)
            .collect::<Vec<_>>(),
        "every letter inked"
    );
    assert!(
        scratch
            .ink
            .windows(2)
            .all(|w| (w[0].row, w[0].col) < (w[1].row, w[1].col)),
        "the ink stays sorted and unique"
    );
    let star = scratch
        .word_decorations
        .iter()
        .find(|d| usize::from(d.row) == line_row)
        .expect("the star");
    assert_eq!(
        (usize::from(star.col), star.glyph, star.blend),
        (text_col, DecoGlyph::Star4, DecoBlend::Add)
    );
    host.shutdown_and_join();
}

/// THE WORKSPACE CARD (Claude Code 2.1.302 on `--effort workspace`, the
/// measured screen): the mascot holds its wand `╱✦` between its blocks and
/// the banner, and the card's text sits at column 13 — it wears the border,
/// the kitty and the line all the same, the line under the card at ITS text
/// column, the vendor's glyphs untouched. CONTROL (mutation-checked): a
/// reader that takes the mascot's columns as block glyphs alone leaves this
/// card bare, as the Together run of 2026-10-16 saw it (defect D1: Claude's
/// tab showed only the vendor's banner for the whole run).
#[test]
fn the_workspace_card_wears_the_card_at_its_own_text_column() {
    let text =
        include_str!("../../aterm-phase/src/fixtures/claude-2.1.302-launch-workspace-120col.txt");
    let screen: Vec<String> = text.lines().skip(1).map(str::to_owned).collect();
    let wand =
        " \u{2590}\u{259B}\u{2588}\u{2588}\u{2588}\u{259B}\u{2588}\u{2584}\u{2571}\u{2726}  ";
    assert_eq!(
        screen[0],
        format!("{wand}{}2.1.302", banner()),
        "the measured banner row"
    );
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let session = app.focused_session_id(wid).expect("the front session");
    assert!(app.apply_term_resize(wid, 24, 120));
    feed(&app, session, &screen);
    publish(&app, session, "2.1.302");
    let host = supervise(&mut app, session);
    frame(&mut app, wid);
    let strip = strip_rows(&app, wid);
    let (top, text_col) = (0usize, 13usize);
    let p = painted(&app, wid);
    assert!(!p.under.is_empty(), "the border and the trail");
    assert!(
        p.over.iter().any(|q| q.alpha == u8::MAX),
        "the kitty: opaque art"
    );
    let line_row = strip + top + 3;
    let want = expected_line(AutopilotView::Cruise, text_col, 120);
    let start = text_col + LINE_LEAD;
    assert_eq!(
        glass_text(&app, wid, line_row, start..start + want.chars().count()),
        want,
        "the line under the card at its text column"
    );
    assert_eq!(
        glass_text(&app, wid, strip + top, 0..screen[0].chars().count()),
        screen[0],
        "the vendor's banner row, the wand in it, untouched"
    );
    host.shutdown_and_join();
}

/// A frame of Claude Code 2.1.302's fullscreen entrance, the measured JUMP
/// (`aterm-phase/src/fixtures/claude-2.1.302-launch-workspace-fullscreen-entrance-jump-*`,
/// one launch, its screen read every ~50 ms, 144x40): the mascot standing,
/// a row low in its dust (twice), both arms up, and at rest with the wand.
fn jump(pose: &str) -> Vec<String> {
    let text = match pose {
        "1-stand" => include_str!(
            "../../aterm-phase/src/fixtures/claude-2.1.302-launch-workspace-fullscreen-entrance-jump-1-stand-144col.txt"
        ),
        "2-dust" => include_str!(
            "../../aterm-phase/src/fixtures/claude-2.1.302-launch-workspace-fullscreen-entrance-jump-2-dust-144col.txt"
        ),
        "3-dust" => include_str!(
            "../../aterm-phase/src/fixtures/claude-2.1.302-launch-workspace-fullscreen-entrance-jump-3-dust-144col.txt"
        ),
        "4-arms-up" => include_str!(
            "../../aterm-phase/src/fixtures/claude-2.1.302-launch-workspace-fullscreen-entrance-jump-4-arms-up-144col.txt"
        ),
        "5-rest" => include_str!(
            "../../aterm-phase/src/fixtures/claude-2.1.302-launch-workspace-fullscreen-entrance-jump-5-rest-144col.txt"
        ),
        other => panic!("no such frame: {other}"),
    };
    text.lines().skip(1).map(str::to_owned).collect()
}

/// The jump's frames in the order the screen drew them.
const JUMP: [&str; 10] = [
    "1-stand",
    "2-dust",
    "3-dust",
    "4-arms-up",
    "1-stand",
    "2-dust",
    "3-dust",
    "4-arms-up",
    "1-stand",
    "5-rest",
];

/// A headless window 144x40 showing the entrance's frame `pose`, the facts
/// of a 2.1.302 process published and its session supervised under Cruise.
fn entrance_glass(pose: &str) -> (App, WindowId, u64, crate::harness_host::HostHandle) {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let session = app.focused_session_id(wid).expect("the front session");
    assert!(app.apply_term_resize(wid, 40, 144));
    feed(&app, session, &jump(pose));
    publish(&app, session, "2.1.302");
    let host = supervise(&mut app, session);
    (app, wid, session, host)
}

/// Where the card decorated on `wid`'s last frame sits, `None` when no card
/// is on the glass.
fn decorated(app: &App, wid: WindowId) -> Option<CardPlace> {
    let on = app.windows[&wid]
        .claude_card
        .entries
        .iter()
        .any(|e| e.shown)
        && !painted(app, wid).under.is_empty();
    on.then(|| laid_key(app, wid).place)
}

/// THE CARD STAYS ON THE GLASS THROUGH THE VENDOR'S ENTRANCE. Claude Code
/// 2.1.302's fullscreen renderer plays its mascot's entrance once per build
/// (the measured jump: stand, dust, dust, arms up, twice; stand; the wand at
/// rest), and the card found on the standing frame is decorated on every
/// frame after it — at its place through the dust and the raised arms, which
/// the reader places nothing on, held by its words — and at the rest's place
/// once the wand is up. A frame after each that changed nothing reads no
/// card: a held reading is kept like any other (`CardReading::holds`).
/// CONTROLS: the dust and arms-up frames alone, on a fresh window, draw no
/// card. Mutation-checked: a `read_card` that never holds leaves the six
/// frames of dust and arms bare, the blink main shows in the lab.
#[test]
fn the_card_stays_on_the_glass_through_the_entrance() {
    let (mut app, wid, session, host) = entrance_glass("1-stand");
    let mut shown = Vec::new();
    for pose in JUMP {
        feed(&app, session, &jump(pose));
        frame(&mut app, wid);
        let place = decorated(&app, wid);
        shown.push(place);
        let _ = crate::work_counts::take_card_reads();
        frame(&mut app, wid);
        assert_eq!(
            (crate::work_counts::take_card_reads(), decorated(&app, wid)),
            (0, place),
            "{pose}: the frame after, nothing changed"
        );
    }
    let stand = aterm_phase::launch_card_place(&jump("1-stand")).expect("standing");
    let rest = aterm_phase::launch_card_place(&jump("5-rest")).expect("at rest");
    assert_eq!((stand.top, stand.mascot_end, rest.mascot_end), (1, 9, 11));
    let mut want = vec![Some(stand); 9];
    want.push(Some(rest));
    assert_eq!(shown, want, "the card on every frame of the entrance");
    host.shutdown_and_join();
    for pose in ["2-dust", "3-dust", "4-arms-up"] {
        let (mut app, wid, _, host) = entrance_glass(pose);
        frame(&mut app, wid);
        assert_eq!(decorated(&app, wid), None, "CONTROL: {pose} alone");
        host.shutdown_and_join();
    }
}

/// A HELD CARD GOES ON THE FRAME ITS BANNER LEAVES, as the window holds it.
/// Held through the raised arms, it goes when the screen is then cleared:
/// the held reading is keyed to the card's own row, so a change there reads
/// anew. Only the LAST reading's card is held: with the card switched off
/// for two frames — the screen cleared, then the raised arms — the card that
/// went unseen is not held again when the card is back on. And a resize lets
/// it go: its place was read on the old grid. CONTROLS: the standing frame
/// decorates and the arms hold it in each run, and with the card switched
/// off for the arms alone the card is held when it is back on.
/// Mutation-checked: a held reading keyed to the reader's own banner (none,
/// for the raised arms) keeps the card on the cleared screen; an earlier
/// reading's card held as the last one's keeps it on the arms after the
/// unseen clear; a pane's text kept across a resize keeps it on the resized
/// arms.
#[test]
fn a_held_card_goes_on_the_frame_its_banner_leaves() {
    let blank = vec![String::new(); 40];
    let switch = |app: &mut App, on: bool| {
        app.config.claude_footer = Some(crate::app_config::ClaudeFooterConfig {
            card: Some(on),
            ..Default::default()
        });
    };
    let (mut app, wid, session, host) = entrance_glass("1-stand");
    frame(&mut app, wid);
    let stand = decorated(&app, wid).expect("CONTROL: the standing card");
    feed(&app, session, &jump("4-arms-up"));
    frame(&mut app, wid);
    assert_eq!(decorated(&app, wid), Some(stand), "CONTROL: held");
    feed(&app, session, &blank);
    frame(&mut app, wid);
    assert_eq!(decorated(&app, wid), None, "cleared after the hold");
    host.shutdown_and_join();

    for (screens, want) in [
        (vec![jump("4-arms-up")], Some(stand)),
        (vec![blank, jump("4-arms-up")], None),
    ] {
        let (mut app, wid, session, host) = entrance_glass("1-stand");
        frame(&mut app, wid);
        assert_eq!(
            decorated(&app, wid),
            Some(stand),
            "CONTROL: the standing card"
        );
        switch(&mut app, false);
        for rows in &screens {
            feed(&app, session, rows);
            frame(&mut app, wid);
            assert_eq!(decorated(&app, wid), None, "switched off");
        }
        switch(&mut app, true);
        frame(&mut app, wid);
        assert_eq!(
            decorated(&app, wid),
            want,
            "back on after {} unseen frames",
            screens.len()
        );
        host.shutdown_and_join();
    }

    let (mut app, wid, session, host) = entrance_glass("1-stand");
    frame(&mut app, wid);
    assert_eq!(
        decorated(&app, wid),
        Some(stand),
        "CONTROL: the standing card"
    );
    assert!(app.apply_term_resize(wid, 40, 120));
    feed(&app, session, &jump("4-arms-up"));
    frame(&mut app, wid);
    assert_eq!(decorated(&app, wid), None, "the arms after a resize");
    host.shutdown_and_join();
}

/// LAND wears the sunset (red → gold → rose: no light bluer than it is red)
/// and says so on its line. CONTROL: Cruise's rainbow has blue in it.
#[test]
fn land_wears_the_sunset_and_says_land() {
    let (mut app, wid, session) = glass(120);
    let host = supervise(&mut app, session);
    let blue = |q: &GlowQuad| (q.color & 0xff) > ((q.color >> 16) & 0xff);
    frame(&mut app, wid);
    assert!(
        painted(&app, wid).under.iter().any(blue),
        "Cruise: the spectrum"
    );
    assert!(app.set_session_autopilot(session, Some(Autopilot::Land)));
    frame(&mut app, wid);
    let p = painted(&app, wid);
    assert!(!p.under.is_empty());
    assert!(
        !p.under.iter().any(blue),
        "Land: a sunset, never bluer than red"
    );
    let strip = strip_rows(&app, wid);
    let want = expected_line(AutopilotView::Land, 11, 120);
    assert!(want.ends_with("\u{b7} autopilot \u{2913} land"), "{want:?}");
    assert_eq!(
        glass_text(&app, wid, strip + 4, 13..13 + want.chars().count()),
        want
    );
    host.shutdown_and_join();
}

/// A TASK'S END ON THE CARD'S LINE (the Autopilot design's §9.4.1, §10.2.1):
/// under Land and Cruise, the task's face told `landed` ends the line `·
/// autopilot ✓ landed` and `done` ends it `· autopilot ✓ done` — the same
/// words under both; spent, theirs, open and unknown keep the mode's own
/// (`⤓ land`, `∞ cruise`). The words are made once per pair (a frame formats
/// nothing), the narrower rungs say no mode, and a face's move re-lays the
/// line and ARMS NO EPISODE. NEGATIVE CONTROL: Off, and no harness, say no
/// end whatever is told.
#[test]
fn a_tasks_end_is_said_on_the_cards_line_under_land_and_cruise() {
    use aterm_agent::harness::acts::TaskFace;
    for view in AutopilotView::ALL {
        let faces = std::iter::once(None).chain(TaskFace::ALL.into_iter().map(Some));
        for task in faces {
            let want = match (view, task) {
                (AutopilotView::Land | AutopilotView::Cruise, Some(TaskFace::Landed)) => {
                    LineEnd::Landed
                }
                (AutopilotView::Land | AutopilotView::Cruise, Some(TaskFace::Done)) => {
                    LineEnd::Done
                }
                _ => LineEnd::Mode,
            };
            assert_eq!(LineEnd::of(view, task), want, "{view:?} {task:?}");
        }
    }
    for (end, tail) in [
        (LineEnd::Mode, "\u{2913} land"),
        (LineEnd::Landed, "\u{2713} landed"),
        (LineEnd::Done, "\u{2713} done"),
    ] {
        let texts = line_texts(AutopilotView::Land, end);
        assert!(
            texts[0].ends_with(&format!("\u{b7} autopilot {tail}")),
            "{texts:?}"
        );
        assert!(
            std::ptr::eq(texts, line_texts(AutopilotView::Land, end)),
            "made once"
        );
        assert_eq!(
            texts[1..],
            line_texts(AutopilotView::Land, LineEnd::Mode)[1..],
            "the narrower rungs"
        );
        if end == LineEnd::Mode {
            let cruise = line_texts(AutopilotView::Cruise, end);
            assert!(
                cruise[0].ends_with("\u{b7} autopilot \u{221e} cruise"),
                "{cruise:?}"
            );
        } else {
            assert_eq!(
                line_texts(AutopilotView::Cruise, end),
                texts,
                "Cruise says the end in Land's words"
            );
        }
        for view in [AutopilotView::Off, AutopilotView::MasterOff] {
            assert_eq!(
                line_texts(view, end),
                line_texts(view, LineEnd::Mode),
                "NEGATIVE CONTROL: {view:?} says no end"
            );
        }
    }
    let fits = |end: LineEnd| {
        line_texts(AutopilotView::Land, end)
            .iter()
            .find(|t| 11 + LINE_LEAD + text_cells(t) < 120)
            .expect("a line fits")
            .clone()
    };
    for word in [Autopilot::Land, Autopilot::Cruise] {
        let (mut app, wid, session) = glass(120);
        let host = supervise(&mut app, session);
        app.windows.get_mut(&wid).unwrap().band_on_screen_for_test = true;
        let sid = app
            .pool
            .get(session)
            .unwrap()
            .ctx
            .self_id
            .as_str()
            .to_string();
        assert!(app.set_session_autopilot(session, Some(word)));
        let mut t = Instant::now();
        set_now(Some(t));
        frame(&mut app, wid);
        t += Duration::from_millis(EPISODE_MS + 1000);
        set_now(Some(t));
        frame(&mut app, wid);
        assert_eq!(app.claude_card_deadline(t), None, "settled");
        let strip = strip_rows(&app, wid);
        for (face, end) in [
            (Some(TaskFace::Landed), LineEnd::Landed),
            (Some(TaskFace::Done), LineEnd::Done),
            (Some(TaskFace::Spent), LineEnd::Mode),
            (Some(TaskFace::Theirs), LineEnd::Mode),
            (Some(TaskFace::Open), LineEnd::Mode),
            (Some(TaskFace::Landed), LineEnd::Landed),
            (None, LineEnd::Mode),
        ] {
            app.apply_harness_fact(&sid, crate::HarnessFact::Task(face));
            frame(&mut app, wid);
            let want = match (word, end) {
                (Autopilot::Land, _) | (_, LineEnd::Landed | LineEnd::Done) => fits(end),
                _ => expected_line(AutopilotView::Cruise, 11, 120),
            };
            assert_eq!(
                glass_text(&app, wid, strip + 4, 13..13 + want.chars().count()),
                want,
                "{word:?}: {face:?}"
            );
            assert_eq!(
                app.claude_card_deadline(t),
                None,
                "{word:?}: {face:?}: a face's move arms no episode"
            );
        }
        set_now(None);
        host.shutdown_and_join();
    }
}

/// A timer's face, `18:00` on the day it was set or (`other_day`) on a
/// Saturday.
fn six_pm(other_day: bool) -> crate::claude_lights::LandAtGlass {
    crate::claude_lights::LandAtGlass {
        clock: crate::claude_lights::Clock::of_minute(18 * 60),
        other_day: other_day.then_some(crate::claude_lights::Weekday::Sat),
    }
}

/// THE LINE FOLLOWS AN ARMED TIMER (§10.6.1): under Cruise with a timer armed
/// the line's widest words end `· autopilot ∞ cruise ⤓18:00` — `⤓Sat 18:00`
/// for a timer on another day, as the title writes it — then narrow to
/// Cruise's own words, then as before. The words are made once per face of
/// the timer (a frame formats nothing). CONTROL: no timer, Land, and a
/// landing's end keep their own words (`LineEnd::timed`).
#[test]
fn the_cards_line_follows_an_armed_timer() {
    let cruise = line_texts(AutopilotView::Cruise, LineEnd::Mode);
    for (other_day, tail) in [(false, "\u{2913}18:00"), (true, "\u{2913}Sat 18:00")] {
        let at = six_pm(other_day);
        let end = LineEnd::Mode.timed(AutopilotView::Cruise, Some(at));
        assert_eq!(end, LineEnd::CruiseUntil(at));
        let texts = line_texts(AutopilotView::Cruise, end);
        assert_eq!(
            texts[0],
            format!("{} {tail}", cruise[0]),
            "the timer's time after Cruise's words"
        );
        assert!(
            texts[0].ends_with(&format!("\u{b7} autopilot \u{221e} cruise {tail}")),
            "{texts:?}"
        );
        assert_eq!(texts[1..], cruise[..], "then Cruise's own rungs");
        assert!(
            std::ptr::eq(texts, line_texts(AutopilotView::Cruise, end)),
            "made once"
        );
        assert_eq!(
            line_texts(AutopilotView::Land, end),
            line_texts(AutopilotView::Land, LineEnd::Mode),
            "the timer is Cruise's"
        );
    }
    assert_ne!(
        line_texts(AutopilotView::Cruise, LineEnd::CruiseUntil(six_pm(false)))[0],
        line_texts(AutopilotView::Cruise, LineEnd::CruiseUntil(six_pm(true)))[0],
        "another face, other words"
    );
    let at = Some(six_pm(false));
    assert_eq!(
        LineEnd::Mode.timed(AutopilotView::Cruise, None),
        LineEnd::Mode,
        "CONTROL: no timer"
    );
    for (end, view) in [
        (LineEnd::Mode, AutopilotView::Land),
        (LineEnd::Landed, AutopilotView::Land),
        (LineEnd::Done, AutopilotView::Land),
        (LineEnd::Mode, AutopilotView::Off),
    ] {
        assert_eq!(end.timed(view, at), end, "CONTROL: {view:?} {end:?}");
    }
}

/// THE TIMER ON THE GLASS: a settled Cruise card whose session arms a timer
/// for 18:00 says `⤓18:00` at the end of its line on the next frame — laid
/// out once, no episode armed — and every frame after it replays the
/// settled frame and its line as they are, laying out nothing and inking
/// nothing; the timer gone, the line says Cruise's own words again.
/// CONTROL: the same card with no timer never says a time.
#[test]
fn a_cruise_cards_line_says_its_armed_timer() {
    let (mut app, wid, session) = glass(120);
    let host = supervise(&mut app, session);
    app.windows.get_mut(&wid).unwrap().band_on_screen_for_test = true;
    let mut t = Instant::now();
    set_now(Some(t));
    frame(&mut app, wid);
    t += Duration::from_millis(EPISODE_MS + 1000);
    set_now(Some(t));
    frame(&mut app, wid);
    assert_eq!(app.claude_card_deadline(t), None, "settled");
    let line_row = strip_rows(&app, wid) + 4;
    let start = 11 + LINE_LEAD;
    let read = |app: &App, words: &str| {
        glass_text(app, wid, line_row, start..start + words.chars().count())
    };
    let plain = expected_line(AutopilotView::Cruise, 11, 120);
    assert_eq!(read(&app, &plain), plain, "CONTROL: no timer, no time");
    assert!(!glass_text(&app, wid, line_row, 0..120).contains('\u{2913}'));
    // Armed for 18:00 UTC, set at noon.
    let day_ms = 20_000 * 86_400_000_i64;
    let wall = day_ms + 12 * 3_600_000;
    let utc = |_: i64| Some(0);
    assert!(
        app.autopilot_timers
            .arm(session, day_ms + 18 * 3_600_000, t, wall, &utc, None)
    );
    let timed = format!("{plain} \u{2913}18:00");
    assert!(
        11 + LINE_LEAD + text_cells(&timed) < 120,
        "the timed line fits at 120 columns: {timed:?}"
    );
    let _ = crate::work_counts::take_card_layouts();
    frame(&mut app, wid);
    assert_eq!(read(&app, &timed), timed, "the timer's time on the line");
    assert_eq!(crate::work_counts::take_card_layouts(), 1, "laid out once");
    assert_eq!(
        app.claude_card_deadline(t),
        None,
        "a timer's move arms no episode"
    );
    let under = painted(&app, wid).under.clone();
    let row: Vec<RenderCell> = app.windows[&wid].input_scratch.cells[line_row].clone();
    let _ = crate::work_counts::take_card_line_inks();
    let _ = crate::work_counts::take_card_ticks();
    for i in 0..20u64 {
        set_now(Some(t + Duration::from_millis(1 + i * 7)));
        frame(&mut app, wid);
        assert_eq!(painted(&app, wid).under, under, "constant bytes, frame {i}");
        assert_eq!(
            app.windows[&wid].input_scratch.cells[line_row], row,
            "frame {i}"
        );
    }
    assert_eq!(
        crate::work_counts::take_card_layouts(),
        0,
        "nothing laid out"
    );
    assert_eq!(
        crate::work_counts::take_card_line_inks(),
        0,
        "nothing inked"
    );
    assert_eq!(
        crate::work_counts::take_card_ticks(),
        0,
        "the settled frame, replayed"
    );
    // Cancelled: Cruise's own words again.
    assert!(app.autopilot_timers.forget(session));
    frame(&mut app, wid);
    assert_eq!(read(&app, &plain), plain, "the timer gone");
    assert!(!glass_text(&app, wid, line_row, 0..120).contains('\u{2913}'));
    set_now(None);
    host.shutdown_and_join();
}

/// One negative control: its name, and what it does to the wrapped card's
/// app and session.
type NegativeCase = (&'static str, fn(&mut App, u64));

/// NOTHING is drawn — no light, no line — for Off, a session this host does
/// not hold, `[claude_footer] card = false`, a card of another build than the
/// process in front, a scrolled-back frame, no host and the master switch
/// off. CONTROL: the wrapped card on the same screen draws.
#[test]
fn off_unsupervised_switched_off_another_build_or_scrolled_back_draw_nothing() {
    let bare = |app: &App, wid: WindowId| {
        let strip = strip_rows(app, wid);
        painted(app, wid).is_empty()
            && !glass_text(app, wid, strip + 4, 0..120).contains("aterm harness")
            && !app.windows[&wid]
                .claude_card
                .entries
                .iter()
                .any(|e| e.shown)
    };
    let cases: [NegativeCase; 7] = [
        ("off", |app, session| {
            assert!(app.set_session_autopilot(session, Some(Autopilot::Off)));
        }),
        ("unsupervised", |app, session| {
            let ctx = app.pool.get(session).unwrap().ctx.clone();
            crate::session_timeline::release_supervisor(&ctx, None, crate::metrics::now_us());
        }),
        ("switched off", |app, _| {
            app.config.claude_footer = Some(crate::app_config::ClaudeFooterConfig {
                card: Some(false),
                ..Default::default()
            });
        }),
        ("another build", |app, session| {
            publish(app, session, "2.1.282");
        }),
        ("scrolled back", |app, session| {
            let term = app.pool.get(session).unwrap().term.clone();
            let mut term = crate::term_lock(&term);
            term.process(b"\x1b[24;1H");
            for _ in 0..3 {
                term.process(b"\r\n");
            }
            term.scroll_display(2);
            assert!(term.grid().display_offset() > 0);
        }),
        ("no host", |app, _| app.harness = None),
        ("master switch off", |app, _| {
            app.harness_policy = crate::harness_policy_cell::HarnessPolicyCell::new(
                crate::app_config::HarnessPolicy::read("[harness]\nenabled = false\n"),
            );
        }),
    ];
    for (what, case) in cases {
        let (mut app, wid, session) = glass(120);
        let host = supervise(&mut app, session);
        frame(&mut app, wid);
        assert!(!bare(&app, wid), "{what}: CONTROL, the wrapped card draws");
        case(&mut app, session);
        frame(&mut app, wid);
        assert!(bare(&app, wid), "{what}: the vendor's card bare");
        host.shutdown_and_join();
    }
}

/// The terminal's own text, as `text`/`screen` read it: a fresh extraction
/// of the engine's grid, one string per row.
fn engine_text(app: &App, session: u64) -> Vec<String> {
    let mut fresh = RenderInput::empty();
    let term = app.pool.get(session).unwrap().term.clone();
    crate::term_lock(&term).cell_frame_into(&mut fresh, 24, 120);
    fresh
        .cells
        .iter()
        .map(|row| row.iter().map(|c| c.ch).collect::<String>())
        .collect()
}

/// GLASS ONLY: what the terminal holds — the text `text`/`screen` read —
/// never changes, and the frame's undo and strip leave the scratch exactly
/// as the vendor's frame: every cell, and the effect streams without the
/// card's light.
#[test]
fn the_grid_and_the_vendors_frame_are_untouched() {
    let (mut app, wid, session) = glass(120);
    let before = engine_text(&app, session);
    frame(&mut app, wid);
    let vendor = {
        let ws = app.windows.get_mut(&wid).unwrap();
        assert!(crate::claude_footer::undo(
            &mut ws.claude_footer_undo,
            &mut ws.input_scratch
        ));
        ws.input_scratch.cells.clone()
    };
    let host = supervise(&mut app, session);
    frame(&mut app, wid);
    assert!(!painted(&app, wid).is_empty());
    assert_ne!(app.windows[&wid].input_scratch.cells, vendor, "the line");
    assert_eq!(engine_text(&app, session), before, "the terminal's text");
    let ws = app.windows.get_mut(&wid).unwrap();
    let (under, over) = (
        ws.claude_card.painted.under.clone(),
        ws.claude_card.painted.over.clone(),
    );
    assert!(crate::claude_footer::undo(
        &mut ws.claude_footer_undo,
        &mut ws.input_scratch
    ));
    strip(&mut ws.claude_card, &mut ws.input_scratch);
    assert_eq!(ws.input_scratch.cells, vendor, "every cell the vendor's");
    assert!(
        under
            .iter()
            .all(|q| !ws.input_scratch.glow_under.contains(q))
    );
    assert!(over.iter().all(|q| !ws.input_scratch.nova_add.contains(q)));
    assert!(
        ws.input_scratch
            .word_decorations
            .iter()
            .all(|d| d.glyph != DecoGlyph::Star4 || usize::from(d.col) != 11)
    );
    host.shutdown_and_join();
}

/// C6: A CAPTURE'S RE-SPLICE never doubles the light. A capture re-splices a
/// scratch whose effect streams still hold the last frame's light
/// (`cell_frame_into` never writes them): the splice takes its own back
/// first. Splicing the same scratch again, and running the whole capture
/// again, leave every stream as long as once.
#[test]
fn a_capture_resplice_never_doubles_the_light() {
    let (mut app, wid, session) = glass(120);
    let host = supervise(&mut app, session);
    let t0 = Instant::now();
    TEST_NOW.with(|now| now.set(Some(t0)));
    let lens = |app: &App| {
        let s = &app.windows[&wid].input_scratch;
        (
            s.glow_under.len(),
            s.nova_add.len(),
            s.ink.len(),
            s.word_decorations.len(),
        )
    };
    frame(&mut app, wid);
    let once = lens(&app);
    assert!(!painted(&app, wid).is_empty());
    let (plan, route) = app.active_visible_frame_layout(wid).unwrap();
    for _ in 0..3 {
        app.splice_claude_footer(wid, &plan, route);
        assert_eq!(lens(&app), once, "the same scratch, spliced again");
    }
    frame(&mut app, wid);
    assert_eq!(lens(&app), once, "the whole capture, run again");
    let p = painted(&app, wid);
    let scratch = &app.windows[&wid].input_scratch;
    for q in &p.under {
        assert_eq!(
            scratch.glow_under.iter().filter(|o| *o == q).count(),
            1,
            "each quad once"
        );
    }
    let strip = strip_rows(&app, wid);
    assert_eq!(
        glass_text(&app, wid, strip + 4, 0..120)
            .matches("aterm harness")
            .count(),
        1
    );
    TEST_NOW.with(|now| now.set(None));
    host.shutdown_and_join();
}

/// EPISODES, THEN STILLNESS (§3): a card first decorated plays its episode — a
/// frame ahead each step, the repaint key moving every frame — then asks once
/// for its settled frame, and from there its key is constant and no wake is
/// armed, over a thousand idle frames. Focus regained plays it again, the
/// kitty in the seat the glass showed and the trail whole (`harness_card`'s
/// `Ride`, review of slice 5b): no snap back to the trail's start.
/// NEGATIVE CONTROL: the first episode's first frame rides (the kitty left of
/// its seat, no trail yet).
#[test]
fn the_card_settles_to_a_constant_key_and_arms_no_wake() {
    let (mut app, wid, session) = glass(120);
    let host = supervise(&mut app, session);
    app.windows.get_mut(&wid).unwrap().band_on_screen_for_test = true;
    let plan = app.active_visible_leaf_plan(wid).unwrap();
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let set_now = |t: Instant| TEST_NOW.with(|now| now.set(Some(t)));
    set_now(t0);
    assert_eq!(app.claude_card_deadline(t0), None, "nothing on show yet");
    frame(&mut app, wid);
    let riding = card_streams(&app, wid);
    let mut last_fp = app.claude_footer_fp(wid, &plan);
    let mut t = 0;
    while t + FRAME_MS < EPISODE_MS {
        let deadline = app.claude_card_deadline(at(t));
        assert_eq!(deadline, Some(at(t + FRAME_MS)), "a frame ahead at {t} ms");
        t += FRAME_MS;
        set_now(at(t));
        assert_eq!(app.claude_cards_due(at(t)), vec![wid], "{t} ms");
        let fp = app.claude_footer_fp(wid, &plan);
        assert_ne!(
            fp, last_fp,
            "the key moves while the episode plays ({t} ms)"
        );
        frame(&mut app, wid);
        last_fp = app.claude_footer_fp(wid, &plan);
    }
    // The episode runs out: one ask for the settled frame, once.
    let end = at(EPISODE_MS + 1);
    set_now(end);
    assert_eq!(app.claude_card_deadline(end), Some(end));
    assert_ne!(
        app.claude_footer_fp(wid, &plan),
        last_fp,
        "the settled frame is owed"
    );
    assert_eq!(app.claude_cards_due(end), vec![wid]);
    assert_eq!(
        app.claude_card_deadline(end),
        None,
        "asked once: no wake spins on a frame that never comes"
    );
    frame(&mut app, wid);
    let settled = app.claude_footer_fp(wid, &plan);
    let quads = painted(&app, wid).under.clone();
    let at_rest = card_streams(&app, wid);
    for i in 0..1000u64 {
        let t = end + Duration::from_millis(1 + i * 7);
        set_now(t);
        assert_eq!(app.claude_footer_fp(wid, &plan), settled, "idle frame {i}");
        assert_eq!(app.claude_card_deadline(t), None, "idle frame {i}");
        assert!(app.claude_cards_due(t).is_empty(), "idle frame {i}");
    }
    for _ in 0..20 {
        frame(&mut app, wid);
        assert_eq!(painted(&app, wid).under, quads, "constant bytes");
        assert_eq!(app.claude_footer_fp(wid, &plan), settled);
    }
    // Focus regained: the episode again, its first frame wanted at once
    // (the last frame drawn is long past).
    let back = end + Duration::from_secs(60);
    set_now(back);
    app.rearm_claude_cards(wid, || back);
    assert!(app.claude_card_deadline(back).is_some_and(|d| d <= back));
    assert_ne!(app.claude_footer_fp(wid, &plan), settled);
    assert_eq!(app.claude_cards_due(back), vec![wid]);
    frame(&mut app, wid);
    assert_eq!(
        app.claude_card_deadline(back),
        Some(back + Duration::from_millis(FRAME_MS)),
        "drawn: the next frame a frame on"
    );
    let replayed = card_streams(&app, wid);
    assert_eq!(
        kitty_x(&replayed),
        kitty_x(&at_rest),
        "the kitty stays seated"
    );
    assert_eq!(
        trail_span(&replayed),
        trail_span(&at_rest),
        "the trail whole"
    );
    assert!(kitty_x(&at_rest).is_some() && trail_span(&at_rest).is_some());
    // CONTROL: the first episode rode in.
    assert!(
        kitty_x(&riding) < kitty_x(&at_rest),
        "the first episode rides"
    );
    assert_ne!(trail_span(&riding), trail_span(&at_rest));
    // NEGATIVE CONTROL: an unfocused window draws the settled frame and arms
    // nothing.
    app.windows.get_mut(&wid).unwrap().focused = false;
    frame(&mut app, wid);
    assert_eq!(app.claude_card_deadline(back), None);
    assert_eq!(painted(&app, wid).under, quads, "the settled frame");
    TEST_NOW.with(|now| now.set(None));
    host.shutdown_and_join();
}

/// LIGHT THEMES (§3: "additive light is invisible on white"): every quad of
/// the card is source-over ink (`alpha > 0`), and its star is stamped over.
#[test]
fn a_light_theme_card_is_ink() {
    let (mut app, wid, session) = glass(120);
    {
        let term = app.pool.get(session).unwrap().term.clone();
        crate::term_lock(&term).process(b"\x1b]11;rgb:ff/ff/ff\x1b\\\x1b]10;rgb:20/20/20\x1b\\");
    }
    let host = supervise(&mut app, session);
    frame(&mut app, wid);
    let p = painted(&app, wid);
    assert!(!p.under.is_empty() && !p.over.is_empty());
    assert!(p.under.iter().chain(&p.over).all(|q| q.alpha > 0));
    assert!(p.decos.iter().all(|d| d.blend == DecoBlend::Over));
    host.shutdown_and_join();
}

/// A NARROW PANE gives up the kitty and its trail first (they need 8 trail
/// cells and four of stars and border inside the pane), keeping the border
/// and the line, which steps down to fit. CONTROL: one column wider, the
/// kitty rides.
#[test]
fn a_narrow_pane_drops_the_kitty_and_keeps_the_border_and_line() {
    let model = "Opus 5.5 with xhigh effort \u{b7} Claude Max";
    let probe = App::headless_for_test();
    let (cw, ch) = probe.win_cell_size(WindowId(0));
    let kc = usize::from(kitty_cols(u16_of(cw), u16_of(ch)));
    assert!(kc > 0, "the headless cell holds the art ({cw}x{ch})");
    // The trail starts two cells past the model row's end.
    let start = 11 + model.chars().count() + 2;
    let narrow = start + TRAIL_MIN + kc + KITTY_MARGIN;
    for (cols, kitty) in [(narrow, false), (narrow + 1, true)] {
        let (mut app, wid, session) = glass(u16_of(cols));
        feed(&app, session, &card_screen(24, cols, model, VERSION));
        let host = supervise(&mut app, session);
        frame(&mut app, wid);
        let p = painted(&app, wid);
        assert!(!p.under.is_empty(), "{cols}: the border");
        assert_eq!(!p.over.is_empty(), kitty, "{cols}: the kitty and its stars");
        let strip = strip_rows(&app, wid);
        let want = expected_line(AutopilotView::Cruise, 11, cols);
        assert_eq!(
            glass_text(&app, wid, strip + 4, 13..13 + want.chars().count()),
            want,
            "{cols}: the line"
        );
        host.shutdown_and_join();
    }
}

/// A SPLIT: the card goes on its own pane only — every quad inside that
/// pane's pixels, the line inside its columns — and the sibling's cells are
/// untouched.
#[test]
fn a_split_pane_decorates_its_own_card_only() {
    let (mut app, wid, session) = glass(160);
    let _other = app.split_active_stub_tab(wid);
    let (pane_rows, pane_cols) = {
        let term = crate::term_lock(&app.pool.get(session).unwrap().term);
        (usize::from(term.rows()), usize::from(term.cols()))
    };
    assert!(pane_cols < 100, "a half-width pane: {pane_cols}");
    feed(
        &app,
        session,
        &card_screen(pane_rows, pane_cols, MODEL, VERSION),
    );
    frame(&mut app, wid);
    let vendor = app.windows[&wid].input_scratch.cells.clone();
    let host = supervise(&mut app, session);
    frame(&mut app, wid);
    let p = painted(&app, wid);
    assert!(!p.under.is_empty());
    let (cw, _) = app.win_cell_size(wid);
    let pad = app.win_pad(wid);
    let right = pad + pane_cols * cw;
    assert!(
        p.under
            .iter()
            .all(|q| usize::from(q.x) + usize::from(q.w) <= right)
    );
    assert!(
        p.over
            .iter()
            .all(|q| usize::from(q.x) + usize::from(q.w) <= pane_cols * cw)
    );
    let glass = &app.windows[&wid].input_scratch.cells;
    for (r, (now, was)) in glass.iter().zip(&vendor).enumerate() {
        assert_eq!(
            now.get(pane_cols..),
            was.get(pane_cols..),
            "row {r}: the sibling's cells"
        );
    }
    assert!(
        glass.iter().any(|row| row
            .iter()
            .take(pane_cols)
            .map(|c| c.ch)
            .collect::<String>()
            .contains("aterm harness")),
        "the line, inside the pane"
    );
    host.shutdown_and_join();
}

/// A row of cells for the layout tests: `text` one char a cell, in `blank`'s
/// style, `cols` wide.
fn cells_of(text: &str, blank: RenderCell, cols: usize) -> Vec<RenderCell> {
    let mut row: Vec<RenderCell> = text.chars().map(|ch| RenderCell { ch, ..blank }).collect();
    row.resize(cols, blank);
    row
}

/// A scratch of `rows` (no strip), each `cols` wide.
fn scratch_of(rows: &[String], cols: usize) -> RenderInput {
    let mut s = RenderInput::empty();
    s.rows = rows.len();
    s.cols = cols;
    s.cells = rows
        .iter()
        .map(|r| cells_of(r, RenderCell::default(), cols))
        .collect();
    s
}

/// The rows of [`card_screen`] above the composer, the card's banner at row 1.
fn card_rows(cols: usize, model: &str) -> Vec<String> {
    card_screen(12, cols, model, VERSION)[..8].to_vec()
}

/// THE LAYOUT'S RULES, on hand-made cells: the line under the card at its
/// text column (four boxed rows); a busy row under the card puts it on the
/// banner row two cells past the banner (three boxed rows) and the kitty
/// after it; a narrow pane narrows the words, then drops them; a wide last
/// glyph ends a row one cell later (`CardPlace::ends`). The kitty stands
/// three rows tall where the working directory's row is blank from the
/// trail's start to the kitty's margin — a glyph anywhere in that span, at
/// either end of it, keeps it two rows tall, one past it does not (the row
/// is read no further) — and the read is one of the layout's noted tests
/// ([`Seen`]): a glyph put in the span after it fails them.
#[test]
fn the_layout_follows_the_designs_rules() {
    let texts = vec!["w".repeat(40), "v".repeat(20)];
    let lay = |rows: &[String], cols: usize, kc: usize| {
        let s = scratch_of(rows, cols);
        let place = aterm_phase::launch_card_place(rows).expect("a card");
        let pane = PaneGeom {
            strip: 0,
            row_off: 0,
            col_off: 0,
            rows: rows.len(),
            cols,
        };
        let cells = PaneCells {
            scratch: &s,
            pane,
            blank: RenderCell::default(),
        };
        layout(&cells, &place, kc, &texts, &mut Seen::default())
    };
    let model_end = 11 + MODEL.chars().count();
    // Room for everything: the line under the card, the kitty on the trail.
    let rows = card_rows(120, MODEL);
    let l = lay(&rows, 120, 6).expect("laid out");
    assert_eq!(l.rows, 4);
    assert_eq!(
        l.line,
        Some(LinePlace {
            row: 4,
            col: 11,
            text: &texts[0],
            cells: 40
        })
    );
    let start = model_end + 2;
    assert_eq!(l.trail, Some((start, start + TRAIL_MAX)));
    assert_eq!(l.kitty_col, Some(start + TRAIL_MAX));
    assert_eq!(
        l.cols,
        (start + TRAIL_MAX + 6 + KITTY_MARGIN).max(11 + 2 + 40)
    );
    assert!(l.cols <= 119);
    // The tall kitty: `~/aterm` ends long before the trail starts.
    assert_eq!(l.kitty_rows, 3, "a short path: three rows tall");
    let margin_end = start + TRAIL_MAX + 6 + KITTY_MARGIN;
    let path_row = |at: usize| {
        let mut r = rows.clone();
        let head: String = r[3].chars().collect();
        let pad = at - head.chars().count();
        r[3] = format!("{head}{}x", " ".repeat(pad));
        r
    };
    for (at, tall) in [
        (start - 1, 3),
        (start, 2),
        (start + TRAIL_MAX / 2, 2),
        (start + TRAIL_MAX + 3, 2),
        (margin_end - 1, 2),
        (margin_end, 3),
    ] {
        let l = lay(&path_row(at), 120, 6).expect("laid out");
        assert!(l.kitty_col.is_some(), "a glyph at {at}: the kitty stays");
        assert_eq!(l.kitty_rows, tall, "a glyph on the path row at {at}");
    }
    {
        let s = scratch_of(&rows, 120);
        let place = aterm_phase::launch_card_place(&rows).expect("a card");
        let pane = PaneGeom {
            strip: 0,
            row_off: 0,
            col_off: 0,
            rows: rows.len(),
            cols: 120,
        };
        let cells = PaneCells {
            scratch: &s,
            pane,
            blank: RenderCell::default(),
        };
        let mut seen = Seen::default();
        let l = layout(&cells, &place, 6, &texts, &mut seen).expect("laid out");
        assert_eq!(l.kitty_rows, 3);
        assert!(
            seen.0.contains(&Probe::Blanks {
                row: 3,
                from: start,
                to: margin_end,
                len: margin_end - start
            }),
            "the path row's read, noted: {:?}",
            seen.0
        );
        assert!(seen.hold(&cells), "the same cells hold");
        let moved = scratch_of(&path_row(margin_end - 1), 120);
        let cells = PaneCells {
            scratch: &moved,
            ..cells
        };
        assert!(
            !seen.hold(&cells),
            "CONTROL: a glyph in the span fails them"
        );
        let past = scratch_of(&path_row(margin_end), 120);
        let cells = PaneCells {
            scratch: &past,
            ..cells
        };
        assert!(seen.hold(&cells), "one past the span: never read");
    }
    // Text under the card: the line goes on the banner row, the box keeps
    // three rows and the kitty rides after the line.
    let mut busy = rows.clone();
    busy[4] = "           a message under the card".to_string();
    let l = lay(&busy, 120, 6).expect("laid out");
    assert_eq!(l.rows, 3);
    let banner_end = 11 + banner().chars().count() + VERSION.len();
    let line = l.line.clone().expect("on the banner row");
    assert_eq!((line.row, line.col), (1, banner_end + 2));
    let after = banner_end + 2 + LINE_LEAD + 40 + 2;
    assert_eq!(l.trail.map(|t| t.0), Some(after));
    // Narrow: the shorter words, then none; no room for the kitty either.
    let l = lay(&card_rows(40, MODEL), 40, 6).expect("laid out");
    assert_eq!(l.line.as_ref().map(|l| l.cells), Some(20));
    assert_eq!((l.kitty_col, l.trail), (None, None));
    assert!(l.cols <= 39);
    let l = lay(&card_rows(30, MODEL), 30, 6).expect("laid out");
    assert_eq!((l.line, l.rows), (None, 3));
    // No kitty art at this cell size: the trail goes with it.
    let l = lay(&rows, 120, 0).expect("laid out");
    assert_eq!((l.kitty_col, l.trail, l.kitty_rows), (None, None, 2));
    // A wide last glyph on the working directory row ends it a cell later.
    let mut wide = rows.clone();
    wide[3] =
        " \u{259D}\u{259D}   \u{259D}\u{259D}   ~/src/github.com/someone/\u{6771}".to_string();
    let mut s = scratch_of(&wide, 120);
    // The engine's cells: the wide glyph at 36, then its continuation.
    s.cells[3].insert(
        37,
        RenderCell {
            wide: true,
            ..RenderCell::default()
        },
    );
    s.cells[3].truncate(120);
    let place = aterm_phase::launch_card_place(&wide).expect("a card");
    assert_eq!(place.ends[2], 37);
    let pane = PaneGeom {
        strip: 0,
        row_off: 0,
        col_off: 0,
        rows: wide.len(),
        cols: 120,
    };
    let cells = PaneCells {
        scratch: &s,
        pane,
        blank: RenderCell::default(),
    };
    let short = vec!["x".to_string()];
    let l = layout(&cells, &place, 0, &short, &mut Seen::default()).expect("laid out");
    assert_eq!(l.cols, 38, "the working directory's wide glyph, whole");
    assert!(38 > banner_end.max(model_end).max(11 + LINE_LEAD + 1));
}

/// The pane width a corpus capture was taken at: its name's `-<n>col`, else
/// its provenance line's `(<cols>x<rows>`, else its widest row.
fn capture_cols(name: &str, text: &str, rows: &[String]) -> usize {
    let digits_before = |s: &str, at: usize| {
        let head = &s[..at];
        let from = head
            .rfind(|c: char| !c.is_ascii_digit())
            .map_or(0, |i| i + 1);
        head[from..].parse::<usize>().ok()
    };
    let by_name = name.rfind("col.txt").and_then(|at| digits_before(name, at));
    let by_header = text.lines().next().and_then(|line| {
        let open = line.find(" (")? + 2;
        let tail = &line[open..];
        let x = tail.find('x')?;
        tail[..x].parse::<usize>().ok()
    });
    by_name
        .or(by_header)
        .unwrap_or_else(|| rows.iter().map(|r| text_cells(r)).max().unwrap_or(80))
}

/// THE TALL KITTY OVER THE CORPUS (§10.6.3): every launch card in
/// `aterm-phase`'s fixtures that the reader places (about 150, Claude Code
/// 2.1.302's workspace card among them), laid out at its capture's width,
/// stands the kitty three rows tall exactly where the card's working
/// directory row is blank from the trail's start to the kitty's margin —
/// judged here from the row's text — and two rows tall elsewhere. CONTROL:
/// each card again with its path run on past the trail's start stands the
/// kitty two rows tall, its place and its trail unchanged.
#[test]
fn every_recorded_card_stands_the_kitty_tall_only_beside_a_blank_path_row() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../aterm-phase/src/fixtures");
    let mut paths: Vec<_> = std::fs::read_dir(&dir)
        .expect("the fixtures")
        .map(|e| e.expect("a fixture").path())
        .collect();
    paths.sort();
    let kc = usize::from(kitty_cols(9, 18));
    let texts = line_texts(AutopilotView::Cruise, LineEnd::Mode);
    let lay = |rows: &[String], cols: usize| {
        let s = scratch_of(rows, cols);
        let place = aterm_phase::launch_card_place(rows)?;
        let pane = PaneGeom {
            strip: 0,
            row_off: 0,
            col_off: 0,
            rows: rows.len(),
            cols,
        };
        let cells = PaneCells {
            scratch: &s,
            pane,
            blank: RenderCell::default(),
        };
        layout(&cells, &place, kc, texts, &mut Seen::default())
            .map(|l| (place, l.trail, l.kitty_col, l.kitty_rows))
    };
    let (mut tall, mut short, mut bare, mut workspace) = (0, 0, 0, 0);
    for path in paths {
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let rows = aterm_phase::prompt::fixtures::screen(&text);
        let cols = capture_cols(&name, &text, &rows);
        let Some((place, trail, kitty_col, rows_tall)) = lay(&rows, cols) else {
            continue;
        };
        let (Some((start, _)), Some(kitty_col)) = (trail, kitty_col) else {
            assert_eq!(rows_tall, 2, "{name}: no kitty");
            bare += 1;
            continue;
        };
        let path_row = place.top + 2;
        let end = kitty_col + kc + KITTY_MARGIN;
        let blank = rows[path_row]
            .chars()
            .chain(std::iter::repeat(' '))
            .skip(start)
            .take(end - start)
            .all(|c| c == ' ');
        assert_eq!(
            rows_tall,
            if blank { 3 } else { 2 },
            "{name}: {:?}",
            rows[path_row]
        );
        if blank {
            tall += 1;
        } else {
            short += 1;
        }
        if name.contains("-workspace-") {
            assert_eq!(rows_tall, 3, "{name}: the workspace card's short path");
            workspace += 1;
        }
        // CONTROL: the path runs on to the kitty's column.
        let mut long = rows.clone();
        let head: String = long[path_row].trim_end().to_string();
        let run = kitty_col.saturating_sub(text_cells(&head)).max(1);
        long[path_row] = format!(
            "{head}{}",
            "/deep".repeat(run).chars().take(run).collect::<String>()
        );
        let (_, trail_long, kitty_long, rows_long) =
            lay(&long, cols).unwrap_or_else(|| panic!("{name}: the long path's card"));
        assert_eq!((trail_long, kitty_long), (trail, Some(kitty_col)), "{name}");
        assert_eq!(
            rows_long, 2,
            "{name}: CONTROL: a long path keeps it two rows tall"
        );
    }
    assert!(
        tall + short + bare >= 140 && tall >= 100 && workspace >= 1,
        "the corpus must exercise the layout: {tall} tall, {short} short, {bare} bare, \
         {workspace} workspace"
    );
}

/// THE TALL KITTY ON THE GLASS: beside the card's short path the scene says
/// three rows and the kitty's opaque art is the tall art — 21 of its square
/// pixels tall, its head where the two-row kitty's is, so at this cell size
/// it reaches past the two-row art's feet; a path run on past the trail's
/// start keeps the two-row art, 14 pixels tall. CONTROL: the two cards
/// differ in the path row alone.
#[test]
fn the_kitty_stands_three_rows_tall_beside_a_short_path() {
    for (long_path, rows_tall, art_h) in [(false, 3u8, 21), (true, 2, 14)] {
        let (mut app, wid, session) = glass(120);
        if long_path {
            let mut screen = card_screen(24, 120, MODEL, VERSION);
            screen[3] = format!(
                "{}/a/working/directory/that/runs/on/past/the/trail",
                screen[3]
            );
            feed(&app, session, &screen);
        }
        let host = supervise(&mut app, session);
        frame(&mut app, wid);
        let strip = strip_rows(&app, wid);
        let (_, ch) = app.win_cell_size(wid);
        let p = (ch - (ch / 14).max(1)) / 7;
        assert!(p >= 1, "a kitty at {ch} px");
        let art: Vec<&GlowQuad> = painted(&app, wid)
            .over
            .iter()
            .filter(|q| q.alpha == u8::MAX)
            .collect();
        assert!(!art.is_empty(), "long path {long_path}: the kitty");
        let y0 = art.iter().map(|q| usize::from(q.y)).min().unwrap();
        let y1 = art
            .iter()
            .map(|q| usize::from(q.y) + usize::from(q.h))
            .max()
            .unwrap();
        // The card's banner is pane row 1: the head sits half the two-row
        // art above the line between the card's first two rows.
        assert_eq!(
            y0,
            (strip + 2) * ch - 7 * p,
            "long path {long_path}: the head"
        );
        assert_eq!(
            y1 - y0,
            art_h * p,
            "long path {long_path}: the art's height"
        );
        let scene = app.windows[&wid].claude_card.entries[0]
            .laid
            .as_ref()
            .and_then(|(_, laid)| laid.as_ref())
            .map(|(_, scene)| scene.kitty_rows);
        assert_eq!(scene, Some(rows_tall), "long path {long_path}");
        host.shutdown_and_join();
    }
}

/// The switch reads on by default, and its Manual row and default are
/// there (`[claude_footer] card`).
#[test]
fn the_card_switch_is_on_by_default() {
    let config = crate::app_config::Config::default();
    assert!(config.claude_footer_card());
    let off = crate::app_config::Config {
        claude_footer: Some(crate::app_config::ClaudeFooterConfig {
            card: Some(false),
            ..Default::default()
        }),
        ..Default::default()
    };
    assert!(!off.claude_footer_card());
    assert!(off.claude_footer_usage(), "the other switch is untouched");
}

/// The strip takes back exactly what was appended, once each, wherever
/// later splices left it — and nothing a refill put there. A stream that
/// holds none of ours — refilled empty (a split's compose clears it), or
/// with another producer's entries off ours' rows (a keystroke's light on
/// its own row) — costs no sort of our keys; the tail as appended costs none
/// either. CONTROL: ours among another's, and a stream with one of
/// ours scrubbed, are sorted for (the counter sees the sort).
#[test]
fn the_strip_removes_only_its_own() {
    let key = |x: &u64| [*x, 0, 0];
    // Each entry on the frame row its value names.
    let row = |x: &u64| u16::try_from(*x).unwrap_or(u16::MAX);
    let mut keys = Vec::new();
    let _ = crate::work_counts::take_card_strip_sorts();
    let sorts = crate::work_counts::take_card_strip_sorts;
    let mut untouched = vec![1, 2, 3, 4, 5];
    remove_each_once(&mut untouched, &[3, 4, 5], &mut keys, key, row);
    assert_eq!(untouched, [1, 2], "the tail, as appended");
    assert_eq!(sorts(), 0, "the tail: no sort");
    let mut stream = vec![1, 2, 3, 9, 4, 5];
    remove_each_once(&mut stream, &[3, 4, 5], &mut keys, key, row);
    assert_eq!(stream, [1, 2, 9], "another producer's after ours");
    assert_eq!(sorts(), 1, "CONTROL: ours among another's, sorted for");
    let mut refilled = vec![7, 8];
    remove_each_once(&mut refilled, &[3, 4], &mut keys, key, row);
    assert_eq!(refilled, [7, 8], "a refill holds none of ours");
    assert_eq!(sorts(), 0, "a refill off ours' rows: no sort");
    let mut below = vec![1, 2, 9];
    remove_each_once(&mut below, &[3, 4, 5], &mut keys, key, row);
    assert_eq!(below, [1, 2, 9], "rows either side of ours");
    assert_eq!(sorts(), 0, "either side of ours' rows: no sort");
    let mut empty: Vec<u64> = Vec::new();
    remove_each_once(&mut empty, &[3, 4], &mut keys, key, row);
    assert!(empty.is_empty());
    assert_eq!(sorts(), 0, "an emptied stream: no sort");
    let mut scrubbed = vec![1, 4, 2];
    remove_each_once(&mut scrubbed, &[3, 4], &mut keys, key, row);
    assert_eq!(scrubbed, [1, 2], "a scrubbed entry does not keep the rest");
    assert_eq!(sorts(), 1, "CONTROL: one of ours left, sorted for");
    let mut twice = vec![4, 4, 4];
    remove_each_once(&mut twice, &[4, 4], &mut keys, key, row);
    assert_eq!(twice, [4], "once each");
}

/// THE TWO PIXEL SPACES the card's streams are drawn in: `glow_under` is
/// window pixels (the border's left side reaching half a cell into the
/// window's padding), `nova_add` grid-interior pixels the renderers offset
/// by the padding themselves; in both, a quad's `row` is its frame row. A
/// strip row moves every quad one band down and its row one on, exactly;
/// window padding moves the window stream with it and leaves the nova
/// stream alone.
#[test]
fn the_streams_meet_the_renderers_pixel_spaces() {
    let (mut app, wid, session) = glass(120);
    let host = supervise(&mut app, session);
    TEST_NOW.with(|now| now.set(Some(Instant::now())));
    frame(&mut app, wid);
    let (under, over) = {
        let p = painted(&app, wid);
        (p.under.clone(), p.over.clone())
    };
    assert!(!under.is_empty() && !over.is_empty());
    let (cw, ch) = app.win_cell_size(wid);
    // A strip row above the grid.
    app.tab_strip_rows = 1;
    frame(&mut app, wid);
    assert_eq!(strip_rows(&app, wid), 1);
    let down = |q: &GlowQuad| GlowQuad {
        row: q.row + 1,
        y: q.y + u16_of(ch),
        ..*q
    };
    let p = painted(&app, wid);
    assert_eq!(p.under, under.iter().map(down).collect::<Vec<_>>());
    assert_eq!(p.over, over.iter().map(down).collect::<Vec<_>>());
    // Window padding, left and top.
    app.tab_strip_rows = 0;
    {
        let ws = app.windows.get_mut(&wid).unwrap();
        ws.metrics.pad = 12;
        ws.metrics.pad_top = 8;
    }
    assert_eq!(
        (app.win_pad(wid), app.win_pad_top(wid) + app.win_head(wid)),
        (12, 8)
    );
    frame(&mut app, wid);
    let p = painted(&app, wid);
    assert_eq!(p.over, over, "grid-interior: the renderer adds the padding");
    let left = |qs: &[GlowQuad]| qs.iter().map(|q| usize::from(q.x)).min().unwrap();
    let right = |qs: &[GlowQuad]| qs.iter().map(|q| usize::from(q.x + q.w)).max().unwrap();
    let top = |qs: &[GlowQuad]| qs.iter().map(|q| usize::from(q.y)).min().unwrap();
    let bottom = |qs: &[GlowQuad]| qs.iter().map(|q| usize::from(q.y + q.h)).max().unwrap();
    assert_eq!(right(&p.under), right(&under) + 12);
    assert_eq!(top(&p.under), top(&under) + 8);
    assert_eq!(bottom(&p.under), bottom(&under) + 8);
    assert_eq!(left(&under), 0, "no padding: the border at the grid's edge");
    assert_eq!(left(&p.under), 12 - cw / 2, "half a cell into the padding");
    let rows = |qs: &[GlowQuad]| {
        let mut rows: Vec<u16> = qs.iter().map(|q| q.row).collect();
        rows.sort_unstable();
        rows.dedup();
        rows
    };
    assert_eq!(rows(&p.under), rows(&under), "frame rows either way");
    TEST_NOW.with(|now| now.set(None));
    host.shutdown_and_join();
}

/// SERIOUS MODE (§3): a static border only — no kitty, trail or stars, the
/// box hugging the card's text and its line (its right side in the column
/// after the line) — and no wake. CONTROL: the same card out of serious mode
/// rides its kitty.
#[test]
fn serious_mode_keeps_a_still_border_only() {
    let (mut app, wid, session) = glass(120);
    let host = supervise(&mut app, session);
    app.windows.get_mut(&wid).unwrap().band_on_screen_for_test = true;
    frame(&mut app, wid);
    let right = |app: &App| {
        painted(app, wid)
            .under
            .iter()
            .map(|q| usize::from(q.x + q.w))
            .max()
            .unwrap()
    };
    assert!(!painted(&app, wid).over.is_empty());
    app.serious_mode = true;
    frame(&mut app, wid);
    let p = painted(&app, wid);
    assert!(!p.under.is_empty(), "the border");
    assert!(p.over.is_empty(), "no kitty, no stars");
    let (cw, _) = app.win_cell_size(wid);
    let line = expected_line(AutopilotView::Cruise, 11, 120);
    let line_end = 11 + LINE_LEAD + text_cells(&line);
    assert_eq!(right(&app), (line_end + 1) * cw, "the box hugs the text");
    assert_eq!(app.claude_card_deadline(Instant::now()), None, "still");
    let strip = strip_rows(&app, wid);
    assert!(glass_text(&app, wid, strip + 4, 0..120).contains("aterm harness"));
    host.shutdown_and_join();
}

/// Set the card's clock ([`card_now`]) to `t`, or back to the wall clock.
fn set_now(t: Option<Instant>) {
    TEST_NOW.with(|now| now.set(t));
}

/// THE TAB MENU over the card (`App::splice_tab_menu`, topmost chrome,
/// painted after the card, hanging from the first grid row where a fresh
/// card sits): none of the card's light, ink or star shows on the menu's
/// rectangle, what lies clear of it stays, and the record still matches the
/// glass — framed again with the menu open, each entry of the card is in its
/// stream exactly once. CONTROL: before the menu's splice, the border, the
/// line's ink and its star (the menu at the card's text), and the kitty and
/// its stars (the menu at the kitty) lay under the rectangle.
#[test]
fn the_tab_menu_hides_the_card_under_it() {
    let (mut app, wid, session) = glass(120);
    let host = supervise(&mut app, session);
    app.tab_strip_rows = 1;
    set_now(Some(Instant::now()));
    frame(&mut app, wid);
    let (cw, _) = app.win_cell_size(wid);
    let pad = app.win_pad(wid);
    let kitty = painted(&app, wid)
        .over
        .iter()
        .map(|q| usize::from(q.x) / cw)
        .min()
        .expect("the kitty and its stars");
    for (anchor, over_kitty) in [(3, false), (u16_of(kitty), true)] {
        assert!(app.open_tab_context_menu(wid, 0, anchor, false));
        frame(&mut app, wid);
        let before = {
            let p = painted(&app, wid);
            (
                p.under.clone(),
                p.over.clone(),
                p.ink.clone(),
                p.decos.clone(),
            )
        };
        app.splice_tab_menu(wid);
        let rect = app.windows[&wid].tab_menu_rect.expect("the menu is up");
        let on = |q: &GlowQuad, origin: usize| {
            let (x0, x1) = (usize::from(q.x), usize::from(q.x) + usize::from(q.w));
            rect.contains(usize::from(q.row), rect.col)
                && x1 > origin + rect.col * cw
                && x0 < origin + (rect.col + rect.w) * cw
        };
        let cell_on = |row: u16, col: u16| rect.contains(usize::from(row), usize::from(col));
        let (under, over, ink, decos) = &before;
        assert!(
            under.iter().any(|q| on(q, pad)),
            "{anchor}: CONTROL, the border"
        );
        if over_kitty {
            assert!(
                over.iter().any(|q| on(q, 0)),
                "{anchor}: CONTROL, the kitty"
            );
        } else {
            assert!(
                ink.iter().any(|c| cell_on(c.row, c.col)),
                "CONTROL, the ink"
            );
            assert!(
                decos.iter().any(|d| cell_on(d.row, d.col)),
                "CONTROL, the star"
            );
        }
        let p = painted(&app, wid);
        let s = &app.windows[&wid].input_scratch;
        assert!(
            !p.under.iter().any(|q| on(q, pad)),
            "{anchor}: no light on it"
        );
        assert!(
            !p.over.iter().any(|q| on(q, 0)),
            "{anchor}: no kitty or star on it"
        );
        assert!(
            !p.ink.iter().any(|c| cell_on(c.row, c.col)),
            "{anchor}: no ink"
        );
        assert!(
            !p.decos.iter().any(|d| cell_on(d.row, d.col)),
            "{anchor}: no star"
        );
        for q in under.iter().filter(|q| on(q, pad)) {
            assert!(
                !s.glow_under.contains(q),
                "{anchor}: cut from the glass too"
            );
        }
        for q in over.iter().filter(|q| on(q, 0)) {
            assert!(!s.nova_add.contains(q), "{anchor}: cut from the glass too");
        }
        for q in under.iter().filter(|q| !on(q, pad)) {
            assert!(
                p.under.contains(q) && s.glow_under.contains(q),
                "{anchor}: the card clear of it stays"
            );
        }
        // Framed again with the menu open: each of the card's entries once.
        frame(&mut app, wid);
        app.splice_tab_menu(wid);
        let p = painted(&app, wid);
        let s = &app.windows[&wid].input_scratch;
        let once = |stream: &[GlowQuad], ours: &[GlowQuad]| {
            ours.iter().all(|q| {
                stream.iter().filter(|o| *o == q).count() == ours.iter().filter(|o| *o == q).count()
            })
        };
        assert!(
            once(&s.glow_under, &p.under),
            "{anchor}: the record is the glass"
        );
        assert!(
            once(&s.nova_add, &p.over),
            "{anchor}: the record is the glass"
        );
        assert!(app.close_tab_menu(wid));
    }
    set_now(None);
    host.shutdown_and_join();
}

/// A QUAD CUT by a rectangle keeps its parts either side, on its own ramp:
/// clear of the columns it is untouched; inside them it goes; straddling
/// them it leaves a left and a right piece whose ends read the ramp at the
/// cut (a flat quad's pieces flat).
#[test]
fn a_cut_quad_keeps_its_ramp_either_side() {
    let ramp = GlowQuad::gradient(2, 100, 40, 100, 8, 0x00_00_00, 0, 0xc8_64_00, 0);
    let mut out = Vec::new();
    assert!(!cut_quad(&ramp, 200..300, &mut out), "clear of it");
    assert!(!cut_quad(&ramp, 0..100, &mut out), "ends where it starts");
    assert!(out.is_empty());
    assert!(cut_quad(&ramp, 90..210, &mut out), "inside it");
    assert!(out.is_empty(), "nothing left");
    assert!(cut_quad(&ramp, 125..150, &mut out));
    let [left, right] = out[..] else {
        panic!("two pieces: {out:?}");
    };
    assert_eq!((left.x, left.w, right.x, right.w), (100, 25, 150, 50));
    assert_eq!((left.row, left.y, left.h), (2, 40, 8));
    assert_eq!((left.color, left.color2), (0x00_00_00, 0x32_19_00));
    assert_eq!((right.color, right.color2), (0x64_32_00, 0xc8_64_00));
    let flat = GlowQuad::flat(0, 10, 0, 30, 4, 0x12_34_56, 200);
    out.clear();
    assert!(cut_quad(&flat, 20..30, &mut out));
    assert!(
        out.iter()
            .all(|q| (q.color, q.color2, q.alpha, q.alpha2) == (0x12_34_56, 0x12_34_56, 200, 200)),
        "{out:?}"
    );
    assert_eq!(
        out.iter().map(|q| (q.x, q.w)).collect::<Vec<_>>(),
        [(10, 10), (30, 10)]
    );
}

/// THE ANCHORED FRAME: while an episode plays, the next frame is due a
/// frame after the last one DRAWN, whatever instant a wake asks at — a wake
/// every 10 ms (another window's build output, each a `WaitCancelled` that
/// re-arms the loop's timer) leaves the deadline where it is and finds the
/// frame due on time; once asked, no wake spins on it; drawn, the next is a
/// frame on.
#[test]
fn other_wakes_never_push_an_episode_frame_on() {
    let (mut app, wid, session) = glass(120);
    let host = supervise(&mut app, session);
    app.windows.get_mut(&wid).unwrap().band_on_screen_for_test = true;
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    set_now(Some(t0));
    frame(&mut app, wid);
    for ms in [0, 10, 20, 30] {
        assert_eq!(
            app.claude_card_deadline(at(ms)),
            Some(at(FRAME_MS)),
            "a wake at {ms} ms: anchored at the frame drawn at 0"
        );
        assert!(app.claude_cards_due(at(ms)).is_empty(), "{ms} ms: not yet");
    }
    assert_eq!(app.claude_cards_due(at(40)), vec![wid], "due on time");
    assert_eq!(app.claude_card_deadline(at(40)), None, "asked once");
    assert!(app.claude_cards_due(at(41)).is_empty(), "asked once");
    set_now(Some(at(40)));
    frame(&mut app, wid);
    assert_eq!(app.claude_card_deadline(at(45)), Some(at(40 + FRAME_MS)));
    set_now(None);
    host.shutdown_and_join();
}

/// AN EPISODE PACES ITSELF, whatever else wakes the loop: under a stream of
/// wakes faster than its frame (a 120 Hz display's, the pointer's, another
/// pane's output), the window draws at most one card frame a [`FRAME_MS`] —
/// never one a wake — and the frame that settles the card is the last it
/// draws. The repaint gate is the loop's: the key is read BEFORE the splice
/// ticks the card, and a wake that finds it unmoved draws nothing.
/// CONTROL: the episode still plays (a frame every couple of frame times),
/// and a key that folded the frame just drawn — the card's fingerprint, as
/// it once did — draws on every wake (mutation-checked).
#[test]
fn an_episode_draws_a_frame_a_frame_time_whatever_else_wakes() {
    for wake_ms in [4u64, 8, 20] {
        let (mut app, wid, session) = glass(120);
        let host = supervise(&mut app, session);
        app.windows.get_mut(&wid).unwrap().band_on_screen_for_test = true;
        let plan = app.active_visible_leaf_plan(wid).unwrap();
        let t0 = Instant::now();
        set_now(Some(t0));
        let mut drawn = app.claude_footer_fp(wid, &plan);
        frame(&mut app, wid);
        let (mut frames, mut late) = (0u64, 0u64);
        let mut t = 0;
        while t < EPISODE_MS + 1000 {
            t += wake_ms;
            let now = t0 + Duration::from_millis(t);
            set_now(Some(now));
            let _ = app.claude_cards_due(now);
            let key = app.claude_footer_fp(wid, &plan);
            if key != drawn {
                drawn = key;
                frame(&mut app, wid);
                frames += 1;
                if t > EPISODE_MS + 100 {
                    late += 1;
                }
            }
        }
        let most = EPISODE_MS / FRAME_MS + 2;
        assert!(
            frames <= most,
            "wakes every {wake_ms} ms: {frames} frames, at most {most}"
        );
        assert!(
            frames >= EPISODE_MS / (2 * FRAME_MS),
            "wakes every {wake_ms} ms: the episode played ({frames} frames)"
        );
        assert_eq!(late, 0, "wakes every {wake_ms} ms: settled, nothing more");
        assert_eq!(
            app.claude_card_deadline(t0 + Duration::from_millis(t)),
            None
        );
        set_now(None);
        host.shutdown_and_join();
    }
}

/// A CARD MOVES WITH ITS PANE: zooming the pane moves it to another leaf of
/// the window's plan (as a sibling split or closed to its left does); the
/// card is its session's, so its episode goes on where it stood — not
/// replayed from the start — and no second card lingers. Unzoomed, the same.
#[test]
fn a_pane_that_moves_in_the_plan_keeps_its_card() {
    let (mut app, wid, _) = glass(160);
    // Claude Code runs in the pane split off to the right: leaf 1.
    let session = app.split_active_stub_tab(wid);
    let paint = |app: &App| {
        let (rows, cols) = {
            let term = crate::term_lock(&app.pool.get(session).unwrap().term);
            (usize::from(term.rows()), usize::from(term.cols()))
        };
        feed(app, session, &card_screen(rows, cols, MODEL, VERSION));
    };
    paint(&app);
    publish(&app, session, VERSION);
    let host = supervise(&mut app, session);
    let t0 = Instant::now();
    set_now(Some(t0));
    frame(&mut app, wid);
    let card = |app: &App| {
        let entries = &app.windows[&wid].claude_card.entries;
        assert_eq!(entries.len(), 1, "one card for the session");
        (entries[0].leaf_index, entries[0].armed_at, entries[0].shown)
    };
    assert_eq!(card(&app), (1, t0, true));
    // Two seconds in, zoomed: the pane is the plan's only leaf.
    set_now(Some(t0 + Duration::from_secs(2)));
    app.toggle_pane_zoom();
    assert_eq!(app.active_visible_leaf_plan(wid).unwrap().leaves.len(), 1);
    paint(&app);
    frame(&mut app, wid);
    assert_eq!(
        card(&app),
        (0, t0, true),
        "zoomed: the same card, not replayed"
    );
    // Unzoomed: back at leaf 1, still the same card.
    app.toggle_pane_zoom();
    paint(&app);
    frame(&mut app, wid);
    assert_eq!(card(&app), (1, t0, true), "unzoomed");
    set_now(None);
    host.shutdown_and_join();
}

/// A NATIVE TAB IN FRONT (Settings) runs no footer splice, so the card keeps
/// the last terminal frame's `shown`: its episode arms no wake and asks for
/// no frame while it is there. Back on the terminal tab, the episode asks
/// again. CONTROL: on the terminal tab the playing episode wants frames.
#[test]
fn a_native_tab_in_front_arms_no_card_wake() {
    let (mut app, wid, session) = glass(120);
    let host = supervise(&mut app, session);
    app.windows.get_mut(&wid).unwrap().band_on_screen_for_test = true;
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    set_now(Some(t0));
    frame(&mut app, wid);
    assert_eq!(
        app.claude_card_deadline(at(1)),
        Some(at(FRAME_MS)),
        "CONTROL"
    );
    assert!(app.open_harness_settings(wid, crate::harness_screen::HarnessFocus::Top));
    assert!(matches!(
        app.active_visible_content_route(wid),
        Some(crate::VisibleContentRoute::Native { .. })
    ));
    assert_eq!(app.claude_card_deadline(at(1)), None, "no wake behind it");
    assert!(app.claude_cards_due(at(40)).is_empty(), "no frame asked");
    app.switch_tab_in(wid, 0);
    assert_eq!(app.claude_card_deadline(at(40)), Some(at(FRAME_MS)), "back");
    set_now(None);
    host.shutdown_and_join();
}

/// SERIOUS MODE is in the repaint key: toggled on a settled card — whose
/// last frame, place and Autopilot all stay as they were — the key moves, so
/// the frame that strips the card to its border is drawn.
#[test]
fn serious_mode_moves_a_settled_cards_key() {
    let (mut app, wid, session) = glass(120);
    let host = supervise(&mut app, session);
    let plan = app.active_visible_leaf_plan(wid).unwrap();
    let t0 = Instant::now();
    set_now(Some(t0));
    frame(&mut app, wid);
    set_now(Some(t0 + Duration::from_millis(EPISODE_MS + 1000)));
    frame(&mut app, wid);
    let key = app.claude_footer_fp(wid, &plan);
    assert_eq!(app.claude_footer_fp(wid, &plan), key, "settled: constant");
    app.serious_mode = true;
    assert_ne!(
        app.claude_footer_fp(wid, &plan),
        key,
        "serious mode moves it"
    );
    app.serious_mode = false;
    assert_eq!(app.claude_footer_fp(wid, &plan), key, "and back");
    set_now(None);
    host.shutdown_and_join();
}

/// AN AUTOPILOT CHANGE ARMS AN EPISODE (§3): a settled card plays again when
/// its Autopilot moves from Cruise to Land, and when it comes back from Off
/// (the bare card) to Cruise; a frame with no change arms nothing.
#[test]
fn an_autopilot_change_arms_an_episode() {
    let (mut app, wid, session) = glass(120);
    let host = supervise(&mut app, session);
    app.windows.get_mut(&wid).unwrap().band_on_screen_for_test = true;
    let mut t = Instant::now();
    set_now(Some(t));
    frame(&mut app, wid);
    let mut settle = |app: &mut App| {
        t += Duration::from_millis(EPISODE_MS + 1000);
        set_now(Some(t));
        frame(app, wid);
        assert_eq!(app.claude_card_deadline(t), None, "settled");
        frame(app, wid);
        assert_eq!(app.claude_card_deadline(t), None, "no change, no episode");
        t
    };
    let playing = |app: &App, now: Instant| {
        app.claude_card_deadline(now) == Some(now + Duration::from_millis(FRAME_MS))
    };
    let now = settle(&mut app);
    assert!(app.set_session_autopilot(session, Some(Autopilot::Land)));
    frame(&mut app, wid);
    assert!(playing(&app, now), "Cruise to Land plays");
    let now = settle(&mut app);
    assert!(app.set_session_autopilot(session, Some(Autopilot::Off)));
    frame(&mut app, wid);
    assert!(painted(&app, wid).is_empty(), "Off: bare");
    assert!(app.set_session_autopilot(session, Some(Autopilot::Cruise)));
    frame(&mut app, wid);
    assert!(playing(&app, now), "Off to Cruise plays");
    set_now(None);
    host.shutdown_and_join();
}

/// OFF TO CRUISE PLAYS THROUGH THE CLAIM'S GAP, as the real host runs it: Off
/// stops the session's worker and gives back its claim, and the word goes
/// back to Cruise tens of milliseconds before the new worker claims again —
/// a frame drawn in that gap shows the card bare under Cruise. The decorated
/// frame after the claim still plays the episode. CONTROL: Cruise to Land
/// under a held claim plays (the arm itself works), and a frame drawn bare
/// under Cruise with no Off before it (a gap with nothing changed) arms
/// nothing once the claim is back.
#[test]
fn off_to_cruise_plays_though_the_claim_lands_a_frame_later() {
    let (mut app, wid, session) = glass(120);
    let host = supervise(&mut app, session);
    app.windows.get_mut(&wid).unwrap().band_on_screen_for_test = true;
    let ctx = app.pool.get(session).expect("session").ctx.clone();
    let reclaim = || {
        crate::session_timeline::claim_supervisor(
            &ctx,
            &crate::harness_host::holder(),
            None,
            None,
            crate::metrics::now_us(),
        )
        .expect("the claim");
    };
    let release = || {
        assert!(crate::session_timeline::release_supervisor(
            &ctx,
            None,
            crate::metrics::now_us()
        ));
    };
    let mut t = Instant::now();
    set_now(Some(t));
    frame(&mut app, wid);
    let mut settle = |app: &mut App| {
        t += Duration::from_millis(EPISODE_MS + 1000);
        set_now(Some(t));
        frame(app, wid);
        frame(app, wid);
        assert_eq!(app.claude_card_deadline(t), None, "settled");
        t
    };
    let playing = |app: &App, now: Instant| {
        app.claude_card_deadline(now) == Some(now + Duration::from_millis(FRAME_MS))
    };
    // CONTROL: the arm works under a held claim.
    let now = settle(&mut app);
    assert!(app.set_session_autopilot(session, Some(Autopilot::Land)));
    frame(&mut app, wid);
    assert!(playing(&app, now), "Cruise to Land plays");
    assert!(app.set_session_autopilot(session, Some(Autopilot::Cruise)));
    frame(&mut app, wid);
    let now = settle(&mut app);
    // Off: the worker stops and gives back its claim.
    assert!(app.set_session_autopilot(session, Some(Autopilot::Off)));
    release();
    frame(&mut app, wid);
    assert!(painted(&app, wid).is_empty(), "Off: bare");
    // Back to Cruise: a frame in the gap before the new worker's claim.
    assert!(app.set_session_autopilot(session, Some(Autopilot::Cruise)));
    frame(&mut app, wid);
    assert!(
        painted(&app, wid).is_empty(),
        "the gap: Cruise, unclaimed, bare"
    );
    reclaim();
    frame(&mut app, wid);
    assert!(!painted(&app, wid).is_empty(), "claimed: decorated");
    assert!(
        playing(&app, now),
        "Off to Cruise plays though the claim came late"
    );
    // CONTROL: a claim gap under Cruise with no Off before it arms nothing.
    let now = settle(&mut app);
    release();
    frame(&mut app, wid);
    assert!(painted(&app, wid).is_empty(), "unclaimed: bare");
    reclaim();
    frame(&mut app, wid);
    assert!(!painted(&app, wid).is_empty(), "claimed again: decorated");
    assert_eq!(
        app.claude_card_deadline(now),
        None,
        "the same mode back: no episode"
    );
    set_now(None);
    host.shutdown_and_join();
}

/// `tab_status` OFF (the footer's own gate: no status sweep, stale facts):
/// the splice draws no card — no light, no line — and no card is on show to
/// key the repaint or arm a wake. CONTROL: on, the card plays.
#[test]
fn tab_status_off_hides_every_card() {
    let (mut app, wid, session) = glass(120);
    let host = supervise(&mut app, session);
    app.windows.get_mut(&wid).unwrap().band_on_screen_for_test = true;
    let t0 = Instant::now();
    set_now(Some(t0));
    frame(&mut app, wid);
    assert!(app.claude_card_deadline(t0).is_some(), "CONTROL");
    app.config.tab_status = Some(false);
    frame(&mut app, wid);
    assert!(painted(&app, wid).is_empty());
    assert!(!app.windows[&wid].claude_card.any_shown());
    assert_eq!(app.claude_card_deadline(t0), None);
    let strip = strip_rows(&app, wid);
    assert!(!glass_text(&app, wid, strip + 4, 0..120).contains("aterm harness"));
    set_now(None);
    host.shutdown_and_join();
}

/// A buffer's address and capacity: the same pair before and after a pass
/// is a pass that reallocated nothing there.
fn buffer<T>(v: &Vec<T>) -> (usize, usize) {
    (v.as_ptr() as usize, v.capacity())
}

/// A FRAME UNDER THE TAB MENU REALLOCATES NOTHING (`cut_out`, at the frame
/// rate while an episode plays under the open menu): each cut is built in
/// the window's spare and copied back, so the four streams, the card's record
/// of them and the spares keep their buffers, frame after frame, once the
/// first frame under the menu has grown the spares. CONTROL: each frame's cut
/// takes something out — the border, the line's ink and its star (the menu
/// at the card's text), the kitty (the menu at the kitty) — so the passes
/// that rebuild a stream run.
#[test]
fn a_frame_under_the_tab_menu_keeps_its_buffers() {
    let (mut app, wid, session) = glass(120);
    let host = supervise(&mut app, session);
    app.tab_strip_rows = 1;
    set_now(Some(Instant::now()));
    frame(&mut app, wid);
    let (cw, _) = app.win_cell_size(wid);
    let kitty = painted(&app, wid)
        .over
        .iter()
        .map(|q| usize::from(q.x) / cw)
        .min()
        .expect("the kitty and its stars");
    let buffers = |app: &App| {
        let (s, c) = (
            &app.windows[&wid].input_scratch,
            &app.windows[&wid].claude_card,
        );
        [
            buffer(&s.glow_under),
            buffer(&s.nova_add),
            buffer(&s.ink),
            buffer(&s.word_decorations),
            buffer(&c.painted.under),
            buffer(&c.painted.over),
            buffer(&c.painted.ink),
            buffer(&c.painted.decos),
            buffer(&c.spare_quads),
            buffer(&c.spare_ink),
            buffer(&c.spare_decos),
        ]
    };
    let record = |app: &App| {
        let p = painted(app, wid);
        (
            p.under.clone(),
            p.over.clone(),
            p.ink.clone(),
            p.decos.clone(),
        )
    };
    for (anchor, over_kitty) in [(3, false), (u16_of(kitty), true)] {
        assert!(app.open_tab_context_menu(wid, 0, anchor, false));
        frame(&mut app, wid);
        app.splice_tab_menu(wid);
        for i in 0..3 {
            frame(&mut app, wid);
            let (under, over, ink, decos) = record(&app);
            let before = buffers(&app);
            app.splice_tab_menu(wid);
            let cut = record(&app);
            assert_ne!(cut.0, under, "{anchor}/{i}: CONTROL, the border is cut");
            if over_kitty {
                assert_ne!(cut.1, over, "{anchor}/{i}: CONTROL, the kitty is cut");
            } else {
                assert_ne!(cut.2, ink, "{anchor}/{i}: CONTROL, the ink is cut");
                assert_ne!(cut.3, decos, "{anchor}/{i}: CONTROL, the star is cut");
            }
            assert_eq!(buffers(&app), before, "{anchor}/{i}: nothing reallocated");
        }
        assert!(app.close_tab_menu(wid));
    }
    set_now(None);
    host.shutdown_and_join();
}

/// THE READING IS SHARED, NOT COPIED (`PaneTextCache::launch_card`): each
/// frame that reuses a pane's text hands its card the cache's own reading —
/// the same build, at the same address — and copies no build to compare it
/// with the process's (taken from the splice's own timeline lock, passed
/// in); and a REBUILD of the text (a repaint) under an unmoved card shares
/// the last rebuild's reading too, under a new rebuild number (the layout's
/// cells are compared once per rebuild). CONTROL: new text whose card names
/// another build is a new reading; a process of another build leaves the
/// card bare.
#[test]
fn a_reused_pane_text_shares_its_card_reading() {
    let (mut app, wid, session) = glass(120);
    let host = supervise(&mut app, session);
    frame(&mut app, wid);
    let pane = PaneGeom {
        strip: strip_rows(&app, wid),
        row_off: 0,
        col_off: 0,
        rows: 24,
        cols: 120,
    };
    // The process as the splice reads it, under its own timeline lock.
    let leaf = |app: &App| {
        let timeline = app.pool.get(session).unwrap().ctx.timeline.lock().unwrap();
        let version = timeline.claude_footer().and_then(|f| f.version.clone());
        let pgid = timeline.claude_footer_identity().map(|(pgid, _)| pgid);
        drop(timeline);
        card_leaf(
            &app.windows[&wid].claude_footer_texts.borrow(),
            app.config.claude_footer_card(),
            0,
            session,
            pane,
            RenderCell::default(),
            AutopilotView::Cruise,
            true,
            pgid.zip(version.as_deref()),
        )
        .card
    };
    let (first, again) = (leaf(&app), leaf(&app));
    let (Some((a, pgid, gen_a)), Some((b, _, gen_b))) = (&first, &again) else {
        panic!("a decorated card: {first:?} {again:?}");
    };
    assert_eq!((*pgid, &*a.version), (PGID, VERSION));
    assert!(Arc::ptr_eq(a, b), "one reading, shared");
    assert_eq!(gen_a, gen_b, "the same text");
    feed(&app, session, &card_screen(24, 120, MODEL, VERSION));
    frame(&mut app, wid);
    let rebuilt = leaf(&app);
    let Some((c, _, gen_c)) = &rebuilt else {
        panic!("a decorated card: {rebuilt:?}");
    };
    assert_ne!(gen_c, gen_a, "the text was rebuilt");
    assert!(Arc::ptr_eq(c, a), "the same card, the same reading");
    feed(&app, session, &card_screen(24, 120, MODEL, "2.1.284"));
    publish(&app, session, "2.1.284");
    frame(&mut app, wid);
    let fresh = leaf(&app);
    let Some((d, _, _)) = &fresh else {
        panic!("a decorated card: {fresh:?}");
    };
    assert!(!Arc::ptr_eq(d, a), "CONTROL: another card, a new reading");
    assert_eq!(&*d.version, "2.1.284");
    publish(&app, session, VERSION);
    assert!(leaf(&app).is_none(), "CONTROL: another build stays bare");
    host.shutdown_and_join();
}

/// THE CARDS BEFORE THE WINDOW SERVER (`crate::work_counts`): whether a
/// window is on screen is a round trip off macOS, and the card's two judges
/// run on every wake, so they ask it only of a window with a card frame
/// wanted — not with no card, not for a still card, not once its frame is
/// asked for. CONTROL: a card playing its episode is asked about, once per
/// judge that finds a frame wanted, and its frame comes.
#[test]
fn only_a_wanted_card_frame_asks_whether_its_window_is_on_screen() {
    let (mut app, wid, session) = glass(120);
    let host = supervise(&mut app, session);
    app.windows.get_mut(&wid).unwrap().band_on_screen_for_test = true;
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    set_now(Some(t0));
    let judge = |app: &mut App, now: Instant| {
        let _ = crate::work_counts::take_on_screen_asks();
        let wake = (app.claude_card_deadline(now), app.claude_cards_due(now));
        (wake, crate::work_counts::take_on_screen_asks())
    };
    assert_eq!(judge(&mut app, t0), ((None, vec![]), 0), "no card");
    // Unfocused, the card is drawn still and wants no frame.
    app.windows.get_mut(&wid).unwrap().focused = false;
    frame(&mut app, wid);
    assert!(app.windows[&wid].claude_card.any_shown());
    assert_eq!(judge(&mut app, t0), ((None, vec![]), 0), "a still card");
    // CONTROL: focus regained, its episode plays.
    app.windows.get_mut(&wid).unwrap().focused = true;
    app.rearm_claude_cards(wid, || t0);
    assert_eq!(
        judge(&mut app, t0),
        ((Some(at(FRAME_MS)), vec![]), 1),
        "CONTROL: a frame wanted, not yet due"
    );
    assert_eq!(
        judge(&mut app, at(FRAME_MS)),
        ((Some(at(FRAME_MS)), vec![wid]), 2),
        "CONTROL: due"
    );
    assert_eq!(
        judge(&mut app, at(FRAME_MS)),
        ((None, vec![]), 0),
        "asked for: nothing wanted until it is drawn"
    );
    set_now(None);
    host.shutdown_and_join();
}

// ---------------------------------------------------------------------------
// The steady state's cost (owner, 2026-10-15: "the user experince must be
// extremely smooth and zero latency!")
// ---------------------------------------------------------------------------

/// What one pass of the per-frame and per-wake paths cost: the footer's
/// repaint key (`App::claude_footer_fp`, with the chips' and the card's
/// terms), its splice (the card's pass and the chips' block inside it) and
/// the loop's two card judges (`App::claude_card_redraws`,
/// `App::claude_card_wake`) — counted by `crate::work_counts`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct PassCost {
    /// Heap allocations and reallocations.
    allocations: usize,
    /// The harness's clock reads (the card's, a claim's lapse, a chip
    /// click's read-back).
    clock_reads: usize,
    /// Card frames the emitter drew (not replayed).
    ticks: usize,
    /// Card layouts laid over the pane's cells.
    layouts: usize,
    /// Launch cards parsed off the pane's text.
    reads: usize,
    /// Sessions' Autopilot reads (one metadata lock each).
    autopilot_reads: usize,
    /// Card lines whose ink was laid (not kept).
    line_inks: usize,
    /// Session timeline locks the splice took.
    timeline_locks: usize,
    /// Rebuilds of a Claude Code pane's text (the footer's, card or none).
    text_rebuilds: usize,
    /// The footer's own clock reads (its lights').
    footer_clock_reads: usize,
    /// Sorts of the card's recorded keys to take its light back out of a
    /// stream (`remove_each_once`): none where the stream holds none of it.
    strip_sorts: usize,
    /// Light quads moved into the nova stream's pixels (`nova_of`): none
    /// for a card replayed at rest, whose moved kitty and stars are kept.
    nova_moves: usize,
    /// Window cell boxes the renderer derived off its face (the card's look
    /// asks the window's, `App::win_cell_size`): none while the window's
    /// px and font are what they were.
    cell_geometries: usize,
}

/// Run one pass on `wid` (the footer's key, its splice and the card's
/// judges) and say what it cost. The counters are taken before it, so a
/// cost is this pass's alone.
fn pass_cost(app: &mut App, wid: WindowId) -> PassCost {
    let (plan, route) = app.active_visible_frame_layout(wid).expect("a layout");
    let _ = (
        crate::work_counts::take_harness_clock_reads(),
        crate::work_counts::take_card_ticks(),
        crate::work_counts::take_card_layouts(),
        crate::work_counts::take_card_reads(),
        crate::work_counts::take_autopilot_reads(),
        crate::work_counts::take_card_line_inks(),
        crate::work_counts::take_footer_timeline_locks(),
        crate::work_counts::take_pane_text_rebuilds(),
        crate::work_counts::take_footer_clock_reads(),
        crate::work_counts::take_card_strip_sorts(),
        crate::work_counts::take_card_nova_moves(),
        crate::work_counts::take_cell_geometry_derivations(),
    );
    let ((), allocations) = crate::work_counts::allocations_in(|| {
        let _ = app.claude_footer_fp(wid, &plan);
        app.splice_claude_footer(wid, &plan, route);
        let _ = app.claude_card_redraws();
        let _ = app.claude_card_wake();
    });
    PassCost {
        allocations,
        clock_reads: crate::work_counts::take_harness_clock_reads(),
        ticks: crate::work_counts::take_card_ticks(),
        layouts: crate::work_counts::take_card_layouts(),
        reads: crate::work_counts::take_card_reads(),
        autopilot_reads: crate::work_counts::take_autopilot_reads(),
        line_inks: crate::work_counts::take_card_line_inks(),
        timeline_locks: crate::work_counts::take_footer_timeline_locks(),
        text_rebuilds: crate::work_counts::take_pane_text_rebuilds(),
        footer_clock_reads: crate::work_counts::take_footer_clock_reads(),
        strip_sorts: crate::work_counts::take_card_strip_sorts(),
        nova_moves: crate::work_counts::take_card_nova_moves(),
        cell_geometries: crate::work_counts::take_cell_geometry_derivations(),
    }
}

/// A keystroke into Claude's composer, framed: the engine takes the key's
/// echo (the grid is damaged), and the frame is extracted and stripped as
/// the capture route does, ready for [`pass_cost`].
fn keystroke(app: &mut App, wid: WindowId, session: u64, key: u8) {
    let term = app.pool.get(session).expect("session").term.clone();
    crate::term_lock(&term).process(&[key]);
    app.capture_words(wid, Instant::now());
    app.splice_tab_strip(wid);
}

/// The card fixture played out to its SETTLED frame on the test clock:
/// decorated at `t0`, its episode drawn, then the frame it owes.
fn settled_glass(cols: u16, t0: Instant) -> (App, WindowId, u64, crate::harness_host::HostHandle) {
    let (mut app, wid, session) = glass(cols);
    let host = supervise(&mut app, session);
    set_now(Some(t0));
    frame(&mut app, wid);
    set_now(Some(t0 + Duration::from_millis(FRAME_MS)));
    frame(&mut app, wid);
    set_now(Some(t0 + Duration::from_millis(EPISODE_MS + 1)));
    frame(&mut app, wid);
    let ws = &app.windows[&wid];
    assert!(ws.claude_card.any_shown(), "the card is on show");
    assert!(
        ws.claude_card.entries.iter().all(|e| e.card.at_rest()),
        "the card is settled"
    );
    (app, wid, session, host)
}

/// The same screen with no harness host: no chip drawn, no card wrapped —
/// the footer's own cost, which the harness's is measured against.
fn bare_glass(cols: u16) -> (App, WindowId, u64) {
    let (mut app, wid, session) = glass(cols);
    frame(&mut app, wid);
    frame(&mut app, wid);
    assert!(
        !app.windows[&wid].claude_card.any_shown(),
        "no card wrapped"
    );
    (app, wid, session)
}

/// STEADY STATE 1 — A FRAME AT REST WITH A SETTLED, DECORATED CARD costs the
/// harness NOTHING per frame, frame after frame: the pass costs exactly what
/// the same screen with no harness host costs, counter for counter — the
/// same allocations (the chips' block and the card's line laid into kept
/// buffers, the footer's write record reused), no clock read (the card at
/// rest, the claim with no lapse), no frame drawn (the settled frame
/// replayed), no layout and no card parsed (the pane's text unchanged), the
/// line's ink kept, the session's Autopilot read once by the key and once by
/// the splice (the chips and the card share each reading) and ONE timeline
/// lock (the splice's own: the card takes the process from it). The glass
/// is the settled frame, the same bytes every time, put on from where it is
/// kept: no quad of it moved into the nova stream's pixels again
/// (`nova_moves`), and the card's look reads the window's cell box from what
/// the renderer kept, deriving none off the face (`cell_geometries`).
///
/// NEGATIVE CONTROLS: a window regaining focus re-arms the card, and its
/// next pass DRAWS a frame on the clock, moves its kitty and stars and inks
/// its line (`ticks`, `clock_reads`, `nova_moves`, `line_inks` move — the
/// counters see the work the rest skips; a line cache that never keeps its
/// ink fails here, and so does a replay that moves its quads again,
/// mutation-checked); and a card decorated for the first time allocates its
/// buffers (the allocation counter sees the card's).
#[test]
fn a_settled_card_at_rest_costs_the_frame_nothing() {
    let t0 = Instant::now();
    let (mut app, wid, _session, host) = settled_glass(120, t0);
    let (mut bare, bare_wid, _) = bare_glass(120);
    let quads = painted(&app, wid).under.clone();
    let kitty = painted(&app, wid).over.clone();
    assert!(!quads.is_empty(), "the border is on the glass");
    assert!(
        !kitty.is_empty(),
        "the kitty and its stars are on the glass"
    );
    let _ = pass_cost(&mut app, wid);
    let _ = pass_cost(&mut bare, bare_wid);
    let floor = pass_cost(&mut bare, bare_wid);
    assert_eq!(
        (
            floor.timeline_locks,
            floor.autopilot_reads,
            floor.text_rebuilds
        ),
        (1, 2, 0),
        "the footer's own pass at rest: {floor:?}"
    );
    for i in 0..50u64 {
        set_now(Some(t0 + Duration::from_millis(EPISODE_MS + 100 + 7 * i)));
        assert_eq!(pass_cost(&mut app, wid), floor, "frame {i}");
        assert_eq!(painted(&app, wid).under, quads, "frame {i}: the same glass");
        assert_eq!(painted(&app, wid).over, kitty, "frame {i}: the same kitty");
    }
    assert_eq!(pass_cost(&mut bare, bare_wid), floor, "the floor holds");
    // NEGATIVE CONTROL: focus regained, the episode is drawn on the clock.
    let back = t0 + Duration::from_secs(60);
    set_now(Some(back));
    app.rearm_claude_cards(wid, || back);
    let replayed = pass_cost(&mut app, wid);
    assert_eq!(replayed.ticks, 1, "CONTROL: an episode frame is drawn");
    assert!(replayed.clock_reads > 0, "CONTROL: on the clock");
    assert_eq!(replayed.line_inks, 1, "CONTROL: its line inked anew");
    assert!(
        replayed.nova_moves > 0,
        "CONTROL: a drawn frame's kitty and stars are moved into the nova stream"
    );
    // NEGATIVE CONTROL: a card decorated for the first time grows its
    // buffers — a first frame with the harness allocates more than the same
    // first frame without it.
    let first_frame = |harness: bool| {
        let (mut fresh, fresh_wid, fresh_session) = glass(120);
        let fresh_host = harness.then(|| supervise(&mut fresh, fresh_session));
        fresh.capture_words(fresh_wid, Instant::now());
        fresh.splice_tab_strip(fresh_wid);
        let cost = pass_cost(&mut fresh, fresh_wid);
        if let Some(fresh_host) = fresh_host {
            fresh_host.shutdown_and_join();
        }
        cost
    };
    let (wrapped, unwrapped) = (first_frame(true), first_frame(false));
    assert!(
        wrapped.allocations > unwrapped.allocations && wrapped.ticks == 1,
        "CONTROL: the first decorated frame allocates ({wrapped:?} vs {unwrapped:?})"
    );
    set_now(None);
    host.shutdown_and_join();
}

/// Feed both `app`'s and `bare`'s session the same bytes and frame them as
/// the capture route does, ready for [`pass_cost`].
fn feed_both(app: &mut (App, WindowId, u64), bare: &mut (App, WindowId, u64), bytes: &[u8]) {
    for (app, wid, session) in [app, bare] {
        let term = app.pool.get(*session).expect("session").term.clone();
        crate::term_lock(&term).process(bytes);
        app.capture_words(*wid, Instant::now());
        app.splice_tab_strip(*wid);
    }
}

/// THE CHIPS COST THE RULE NOTHING (`claude_footer::rule_edits`): with the
/// harness's chips drawn at the composer rule's left end and the facts at
/// its right, a pass writes the bare footer's cells, the card's line and the
/// chips' few cells — never the rule's own glyphs between them, none of
/// which is written over with itself — keystroke after keystroke.
///
/// NEGATIVE CONTROLS: the rule between them is most of the pane's width
/// (what a whole write of the row copied, glyph over itself, before), and
/// the card's line is counted (the counter sees what the harness writes).
#[test]
fn the_chips_write_no_rule_glyph() {
    let t0 = Instant::now();
    let (mut app, wid, session, host) = settled_glass(120, t0);
    let (mut bare, bare_wid, bare_session) = bare_glass(120);
    let line = painted(&app, wid).ink.len();
    assert!(
        line > 40,
        "CONTROL: the card's line is on the glass ({line})"
    );
    for (i, key) in b"typing".iter().enumerate() {
        keystroke(&mut app, wid, session, *key);
        keystroke(&mut bare, bare_wid, bare_session, *key);
        let _ = crate::work_counts::take_footer_cells_written();
        let _ = pass_cost(&mut bare, bare_wid);
        let floor = crate::work_counts::take_footer_cells_written();
        let _ = pass_cost(&mut app, wid);
        let cells = crate::work_counts::take_footer_cells_written();
        let chips = cells.checked_sub(floor + line);
        assert!(
            chips.is_some_and(|c| (1..=8).contains(&c)),
            "pass {i}: {cells} cells written, the bare footer's {floor} and the line's {line}"
        );
        assert_eq!(
            app.windows[&wid]
                .claude_footer_undo
                .rewritten_as_they_were(),
            0,
            "pass {i}: no cell written over with itself"
        );
    }
    // CONTROL: the rule's own glyphs on the glass, which a whole write of the
    // row would have copied over themselves.
    let rule_row = strip_rows(&app, wid) + 22;
    let glyphs = glass_text(&app, wid, rule_row, 0..120)
        .chars()
        .filter(|c| *c == '\u{2500}')
        .count();
    assert!(glyphs > 80, "CONTROL: the rule between them: {glyphs}");
    set_now(None);
    host.shutdown_and_join();
}

/// The steady-state tests' common shape: the settled card (`app`) and its
/// bare twin (`bare`, the same screen with no harness host) take the same
/// change each pass (`step`, given the pass's index); every pass rebuilds
/// the pane's text (the control that the change reached the footer) and
/// costs the harness exactly the twin's pass, counter for counter — no
/// reading, no layout, no frame, no ink, no lock, no clock read, no sort of
/// its light's keys, no allocation of its own — and the card's glass stays
/// the settled frame.
fn costs_the_floor(
    what: &str,
    passes: usize,
    mut step: impl FnMut(usize, &mut (App, WindowId, u64), &mut (App, WindowId, u64)),
) {
    let t0 = Instant::now();
    let (app, wid, session, host) = settled_glass(120, t0);
    let mut app = (app, wid, session);
    let mut bare = bare_glass(120);
    // A few passes to grow what the change grows (the rows' buffers, a
    // streamed row's text, the streams a chrome row's own animation
    // republishes while it comes in), in both.
    const WARM: usize = 6;
    for i in 0..WARM {
        step(i, &mut app, &mut bare);
        let _ = pass_cost(&mut app.0, wid);
        let _ = pass_cost(&mut bare.0, bare.1);
    }
    let quads = painted(&app.0, wid).under.clone();
    assert!(!quads.is_empty(), "{what}: the border");
    let line = glass_text(&app.0, wid, strip_rows(&app.0, wid) + 4, 0..120);
    assert!(line.contains("aterm harness"), "{what}: the line: {line:?}");
    for i in WARM..WARM + passes {
        set_now(Some(
            t0 + Duration::from_millis(EPISODE_MS + 200 + 9 * i as u64),
        ));
        step(i, &mut app, &mut bare);
        let floor = pass_cost(&mut bare.0, bare.1);
        assert_eq!(
            floor.text_rebuilds, 1,
            "{what} {i}: CONTROL, rebuilt: {floor:?}"
        );
        assert_eq!(pass_cost(&mut app.0, wid), floor, "{what} {i}");
        assert_eq!(
            painted(&app.0, wid).under,
            quads,
            "{what} {i}: the same glass"
        );
        assert_eq!(
            glass_text(&app.0, wid, strip_rows(&app.0, wid) + 4, 0..120),
            line,
            "{what} {i}: the same line"
        );
    }
    set_now(None);
    host.shutdown_and_join();
}

/// STEADY STATE 2 — A KEYSTROKE INTO CLAUDE'S COMPOSER damages the grid and
/// re-splices the footer: the pane's text is rebuilt — only its composer row
/// changed, far below the card — and the harness costs nothing over the
/// bare footer's own pass: the card's reading is kept without a scan (no row
/// it reads changed: `CardReading::holds`), its layout kept by testing
/// again only the cells it read (`Seen::hold`), its light taken back out of
/// the re-extracted streams with no sort (`strip_sorts`: they hold none of
/// it), its frame replayed and its line kept, keystroke after keystroke.
///
/// NEGATIVE CONTROLS: the rebuild is real (`text_rebuilds`, in
/// `costs_the_floor`); keying the reading or the layout by the rebuild
/// again costs a reading and a layout per keystroke (mutation-checked); and
/// a keystroke while the card's episode plays draws the frame on the clock.
#[test]
fn a_keystroke_under_a_settled_card_costs_the_frame_nothing() {
    costs_the_floor("keystroke", 12, |i, app, bare| {
        feed_both(app, bare, &[b"keep typing!"[i % 12]]);
    });
    // NEGATIVE CONTROL: the card's episode plays; a keystroke draws a frame.
    let t0 = Instant::now();
    let (mut app, wid, session, host) = settled_glass(120, t0);
    let back = t0 + Duration::from_secs(60);
    set_now(Some(back));
    app.rearm_claude_cards(wid, || back);
    keystroke(&mut app, wid, session, b'!');
    let playing = pass_cost(&mut app, wid);
    assert_eq!(playing.ticks, 1, "CONTROL: an episode frame is drawn");
    assert!(playing.clock_reads > 0, "CONTROL: on the clock");
    set_now(None);
    host.shutdown_and_join();
}

/// STEADY STATE 3 — CLAUDE AT WORK UNDER THE CARD: a spinner ticking on its
/// row and streamed words landing between the card and the composer (the
/// normal state under Cruise) rebuild the pane's text every frame, and none
/// of it is a banner or a cell the card reads: the harness costs nothing
/// over the bare footer's pass, frame after frame.
#[test]
fn claude_streaming_under_the_card_costs_the_frame_nothing() {
    costs_the_floor("streamed", 12, |i, app, bare| {
        let spinner = ['\u{2722}', '\u{2733}', '\u{2736}', '\u{273B}'][i % 4];
        let bytes = format!(
            "\x1b7\x1b[9;1H\u{23FA} streamed line {i}: Claude Code v2.1.283 said so\x1b[K\
             \x1b[12;1H{spinner} Thinking\u{2026} ({i}s \u{b7} esc to interrupt)\x1b[K\x1b8"
        );
        feed_both(app, bare, bytes.as_bytes());
    });
}

/// STEADY STATE 4 — A REVISION BUMP THAT CHANGED NO CELL: a re-extract with
/// nothing new on the screen — bare, and under a tab strip's chrome row,
/// which forces a refill every frame — moves the scratch's revision and so
/// rebuilds the pane's text, every row the same: the harness costs nothing
/// over the bare footer's pass (the reading and the layout kept, the rows'
/// snapshot itself shared). The split compositor's every-frame compose is
/// `a_split_composed_every_frame_costs_the_card_nothing`.
#[test]
fn a_revision_bump_with_no_cell_changed_costs_the_frame_nothing() {
    for strip in [0, 1] {
        costs_the_floor(&format!("re-extract, strip {strip}"), 12, |i, app, bare| {
            for (app, wid, _) in [app, bare] {
                if i == 0 {
                    app.tab_strip_rows = strip;
                }
                app.capture_words(*wid, Instant::now());
                app.splice_tab_strip(*wid);
            }
        });
    }
}

/// A SPLIT, COMPOSED EVERY FRAME (`app_render` bumps the composite's
/// revision on each compose): the Claude pane's text is rebuilt every frame
/// with nothing in it changed, and its card costs no reading, no layout, no
/// frame and no allocation over the same split with no harness host.
/// CONTROL: each frame rebuilds the text.
#[test]
fn a_split_composed_every_frame_costs_the_card_nothing() {
    let split = |harness: bool| {
        let (mut app, wid, _) = glass(160);
        let session = app.split_active_stub_tab(wid);
        let (rows, cols) = {
            let term = crate::term_lock(&app.pool.get(session).unwrap().term);
            (usize::from(term.rows()), usize::from(term.cols()))
        };
        feed(&app, session, &card_screen(rows, cols, MODEL, VERSION));
        publish(&app, session, VERSION);
        let host = harness.then(|| supervise(&mut app, session));
        (app, wid, host)
    };
    let t0 = Instant::now();
    let (mut app, wid, host) = split(true);
    let (mut bare, bare_wid, _) = split(false);
    for t in [0, FRAME_MS, EPISODE_MS + 1] {
        set_now(Some(t0 + Duration::from_millis(t)));
        frame(&mut app, wid);
        frame(&mut bare, bare_wid);
    }
    assert!(
        !painted(&app, wid).under.is_empty(),
        "the card is on the glass"
    );
    let quads = painted(&app, wid).under.clone();
    for i in 0..10u64 {
        set_now(Some(t0 + Duration::from_millis(EPISODE_MS + 100 + 9 * i)));
        for (app, wid) in [(&mut app, wid), (&mut bare, bare_wid)] {
            app.capture_words(wid, Instant::now());
            app.splice_tab_strip(wid);
        }
        let floor = pass_cost(&mut bare, bare_wid);
        assert!(floor.text_rebuilds >= 1, "{i}: CONTROL, rebuilt: {floor:?}");
        assert_eq!(pass_cost(&mut app, wid), floor, "frame {i}");
        assert_eq!(painted(&app, wid).under, quads, "frame {i}: the same glass");
    }
    set_now(None);
    if let Some(host) = host {
        host.shutdown_and_join();
    }
}

/// STEADY STATE 5 — A WINDOW WITH NO CLAUDE CODE SESSION costs the harness
/// nothing at all: no allocation anywhere in the pass, no clock read (not
/// even the footer lights' own: none is timed), no Autopilot read, no card
/// work, no text read — while its host is up, and after a Claude Code whose
/// card it decorated has left the screen. The footer's one timeline lock per
/// pane (whether its facts are Claude's) is all the pass takes.
///
/// NEGATIVE CONTROL: the same window with Claude Code's facts published
/// reads the session's Autopilot, and its first frame reads the pane's text
/// and composes its footer (the counters are wired to the pass).
#[test]
fn a_window_with_no_claude_session_costs_the_harness_nothing() {
    let t0 = Instant::now();
    let (mut app, wid, session, host) = settled_glass(120, t0);
    // Claude Code leaves: a shell's screen, no footer facts.
    {
        let entry = app.pool.get(session).expect("session");
        entry
            .ctx
            .timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .set_claude_footer(PGID, None, None);
    }
    feed(&app, session, &["$ ls".to_string()]);
    frame(&mut app, wid);
    assert!(
        !app.windows[&wid].claude_card.any_shown(),
        "no card on show"
    );
    for i in 0..20u64 {
        set_now(Some(
            t0 + Duration::from_secs(20) + Duration::from_millis(i),
        ));
        if i % 2 == 0 {
            keystroke(&mut app, wid, session, b'x');
        }
        assert_eq!(
            pass_cost(&mut app, wid),
            PassCost {
                timeline_locks: 1,
                ..PassCost::default()
            },
            "pass {i}: nothing at all"
        );
    }
    // NEGATIVE CONTROL: Claude Code's facts again — the pass reads them.
    publish(&app, session, VERSION);
    feed(&app, session, &card_screen(24, 120, MODEL, VERSION));
    app.capture_words(wid, Instant::now());
    app.splice_tab_strip(wid);
    let claude = pass_cost(&mut app, wid);
    assert!(
        claude.autopilot_reads > 0 && claude.text_rebuilds == 1 && claude.allocations > 0,
        "CONTROL: a Claude Code pane is read ({claude:?})"
    );
    set_now(None);
    host.shutdown_and_join();
}

/// A LAPSING CLAIM reads the clock ONCE a pass, however many readers ask:
/// the host's own lease (`ttl=`) is live only until its instant, so the
/// footer's key and its splice each read the claim clock once — shared by
/// the chips and the card — and a claim with no lapse reads none.
/// CONTROL: the claim lapsed, the card goes bare.
#[test]
fn a_lapsing_claim_reads_the_clock_once_a_pass() {
    let t0 = Instant::now();
    let (mut app, wid, session, host) = settled_glass(120, t0);
    let ctx = app.pool.get(session).expect("session").ctx.clone();
    let now_us = crate::metrics::now_us();
    crate::session_timeline::claim_supervisor(
        &ctx,
        &crate::harness_host::holder(),
        None,
        Some(now_us + 600_000_000),
        now_us,
    )
    .expect("the lease");
    let (plan, route) = app.active_visible_frame_layout(wid).expect("a layout");
    let _ = crate::work_counts::take_harness_clock_reads();
    let _ = app.claude_footer_fp(wid, &plan);
    assert_eq!(crate::work_counts::take_harness_clock_reads(), 1, "the key");
    app.splice_claude_footer(wid, &plan, route);
    assert_eq!(
        crate::work_counts::take_harness_clock_reads(),
        1,
        "the splice"
    );
    assert!(app.windows[&wid].claude_card.any_shown(), "still wrapped");
    // CONTROL: the lease lapsed.
    crate::session_timeline::claim_supervisor(
        &ctx,
        &crate::harness_host::holder(),
        None,
        Some(now_us.saturating_sub(1)),
        now_us,
    )
    .expect("a lapsed lease");
    app.splice_claude_footer(wid, &plan, route);
    assert!(
        !app.windows[&wid].claude_card.any_shown(),
        "CONTROL: a lapsed claim leaves the card bare"
    );
    set_now(None);
    host.shutdown_and_join();
}

/// THE KEPT LAYOUT FOLLOWS THE CELLS: a pass over unchanged text lays
/// nothing out; text that moves beside the card (a word lands where the
/// kitty rode) is laid out anew on its frame — the kitty moves, the new
/// scene drawn once and then replayed — and the card's own text back again
/// is the first scene again, bit for bit. While a find bar is up (its tint
/// writes cells no revision moves) every pass lays the card out anew, and
/// none of those layouts is kept past it. CONTROL: the moved kitty differs
/// from the first.
#[test]
fn the_kept_layout_follows_the_cells() {
    let t0 = Instant::now();
    let (mut app, wid, session, host) = settled_glass(120, t0);
    set_now(Some(t0 + Duration::from_secs(10)));
    let first = painted(&app, wid).over.clone();
    assert!(!first.is_empty(), "the kitty and its stars");
    assert_eq!(pass_cost(&mut app, wid).layouts, 0, "unchanged text: kept");
    // A word lands right of the card's banner: the kitty moves on past it.
    let mut moved = card_screen(24, 120, MODEL, VERSION);
    moved[1] = format!("{:<44}WORD", moved[1]);
    feed(&app, session, &moved);
    app.capture_words(wid, Instant::now());
    app.splice_tab_strip(wid);
    let cost = pass_cost(&mut app, wid);
    assert_eq!(
        (cost.layouts, cost.ticks),
        (1, 1),
        "laid out and drawn anew"
    );
    let shifted = painted(&app, wid).over.clone();
    assert_ne!(shifted, first, "CONTROL: the kitty moved");
    let again = pass_cost(&mut app, wid);
    assert_eq!(
        (again.layouts, again.ticks),
        (0, 0),
        "then kept and replayed"
    );
    assert_eq!(painted(&app, wid).over, shifted);
    // The card's own text again: the first scene, bit for bit.
    feed(&app, session, &card_screen(24, 120, MODEL, VERSION));
    app.capture_words(wid, Instant::now());
    app.splice_tab_strip(wid);
    assert_eq!(pass_cost(&mut app, wid).layouts, 1);
    assert_eq!(painted(&app, wid).over, first, "the first scene again");
    // A find bar up: laid out every pass, nothing kept past it.
    app.windows.get_mut(&wid).unwrap().search = Some(Default::default());
    for i in 0..3 {
        assert_eq!(pass_cost(&mut app, wid).layouts, 1, "find bar up, pass {i}");
    }
    app.windows.get_mut(&wid).unwrap().search = None;
    assert_eq!(
        pass_cost(&mut app, wid).layouts,
        1,
        "the bar gone: laid anew"
    );
    assert_eq!(pass_cost(&mut app, wid).layouts, 0, "then kept");
    assert_eq!(painted(&app, wid).over, first);
    set_now(None);
    host.shutdown_and_join();
}

/// The cells a lone [`layout`] of `key`'s card reads over `wid`'s frame as it
/// stands — the count a frame that lays that card anew may read, and a frame
/// that keeps it may read again, and no more.
fn layout_cells_read(app: &App, wid: WindowId, key: &LaidKey) -> usize {
    let scratch = &app.windows[&wid].input_scratch;
    let _ = crate::work_counts::take_card_cells_read();
    let laid = layout(
        &pane_cells(scratch, key),
        &key.place,
        kitty_width(key),
        line_texts(key.view, key.end),
        &mut Seen::default(),
    );
    assert!(laid.is_some(), "the card is laid out");
    let _ = crate::work_counts::take_card_layouts();
    crate::work_counts::take_card_cells_read()
}

/// The key the card on `wid` was last laid for.
fn laid_key(app: &App, wid: WindowId) -> LaidKey {
    app.windows[&wid].claude_card.entries[0]
        .laid
        .as_ref()
        .map(|(key, _)| *key)
        .expect("laid")
}

/// A CARD LAID ANEW READS EACH CELL ONCE: while Claude's output scrolls the
/// card down the screen a row a frame (still on show), every frame reads it
/// and lays it out anew — and tests exactly the cells that layout reads,
/// noted as it reads them (`Seen`): none is read again to keep the layout
/// for the next frame. The pass after, the same rebuild, reads none.
/// CONTROLS: the layout reads cells at all; the frame laid the card for the
/// key the count was taken for; the card is a fresh window's over each
/// screen. Mutation-checked: a scan of the cells after a fresh layout (the
/// fingerprint 675d61e96 took) fails here.
#[test]
fn a_card_laid_anew_reads_each_cell_once() {
    let t0 = Instant::now();
    let (mut app, wid, session, host) = settled_glass(120, t0);
    let base = card_screen(24, 120, MODEL, VERSION);
    // The card `down` rows lower, the composer kept at the bottom.
    let scrolled = |down: usize| {
        let mut rows = vec![String::new(); down];
        rows.extend_from_slice(&base[..base.len() - 4 - down]);
        rows.extend_from_slice(&base[base.len() - 4..]);
        rows
    };
    for down in 1..=4 {
        let rows = scrolled(down);
        feed(&app, session, &rows);
        set_now(Some(t0 + Duration::from_secs(10 + down as u64)));
        app.capture_words(wid, Instant::now());
        app.splice_tab_strip(wid);
        let place = aterm_phase::launch_card_place(&rows).expect("a card");
        let key = LaidKey {
            place,
            ..laid_key(&app, wid)
        };
        let own = layout_cells_read(&app, wid, &key);
        assert!(own > 0, "down {down}: CONTROL, the layout reads cells");
        let cost = pass_cost(&mut app, wid);
        let read = crate::work_counts::take_card_cells_read();
        assert_eq!(
            (cost.reads, cost.layouts),
            (1, 1),
            "down {down}: read and laid anew: {cost:?}"
        );
        assert_eq!(laid_key(&app, wid), key, "down {down}: CONTROL, the key");
        assert_eq!(read, own, "down {down}: each cell its layout reads, once");
        let again = pass_cost(&mut app, wid);
        assert_eq!(
            (again.layouts, crate::work_counts::take_card_cells_read()),
            (0, 0),
            "down {down}: the same rebuild reads nothing"
        );
        let kept = card_glass(&app, wid);
        assert_eq!(
            kept,
            fresh_card(120, &|app, session| feed(app, session, &rows)),
            "down {down}: a fresh window's card"
        );
    }
    set_now(None);
    host.shutdown_and_join();
}

/// A KEPT LAYOUT IS CHECKED BY THE CELLS ITS LAYOUT READ, AND NO OTHERS: a
/// keystroke under the settled card rebuilds the pane's text, and the card
/// keeps its layout by testing again exactly the cells its layout read — the
/// count a fresh layout of the same screen reads, no run to the border, no
/// cell twice — and in a pane twice as wide, the same count: the layout reads
/// the kitty's rows only as far as decides where it sits. CONTROLS: the check
/// runs (it reads cells; nothing is read or laid anew). Mutation-checked: the
/// kitty's run read to the border, or a kept layout checked by every cell to
/// the border (675d61e96's fingerprint), fails here.
#[test]
fn a_kept_layout_reads_only_the_cells_its_layout_read() {
    let checked = |cols: u16| {
        let t0 = Instant::now();
        let (mut app, wid, session, host) = settled_glass(cols, t0);
        set_now(Some(t0 + Duration::from_secs(10)));
        let mut each = Vec::new();
        for key in *b"typed" {
            keystroke(&mut app, wid, session, key);
            let own = layout_cells_read(&app, wid, &laid_key(&app, wid));
            let cost = pass_cost(&mut app, wid);
            let read = crate::work_counts::take_card_cells_read();
            assert_eq!(
                (cost.reads, cost.layouts, cost.text_rebuilds),
                (0, 0, 1),
                "{cols} columns: kept over a rebuilt text: {cost:?}"
            );
            assert!(read > 0, "{cols} columns: CONTROL, the cells are tested");
            assert_eq!(read, own, "{cols} columns: the layout's own cells");
            each.push(read);
        }
        set_now(None);
        host.shutdown_and_join();
        assert!(each.windows(2).all(|w| w[0] == w[1]), "{each:?}");
        each[0]
    };
    let (narrow, wide) = (checked(120), checked(240));
    assert_eq!(wide, narrow, "twice the width, the same cells");
}

/// What a card puts on the glass: its light, its ink and star, and each line
/// it writes (frame row, column, cells).
#[derive(Debug, PartialEq)]
struct CardGlass {
    under: Vec<GlowQuad>,
    over: Vec<GlowQuad>,
    ink: Vec<InkCell>,
    decos: Vec<WordDecoration>,
    lines: Vec<(usize, usize, Vec<RenderCell>)>,
}

fn card_glass(app: &App, wid: WindowId) -> CardGlass {
    let p = painted(app, wid);
    CardGlass {
        under: p.under.clone(),
        over: p.over.clone(),
        ink: p.ink.clone(),
        decos: p.decos.clone(),
        lines: app.windows[&wid]
            .claude_card
            .lines()
            .map(|l| (l.frame_row, l.col, l.cells.clone()))
            .collect(),
    }
}

/// The card a FRESH window `cols` wide draws, settled, over the screen
/// `paint` draws: what a card kept across frames must be, bit for bit.
fn fresh_card(cols: u16, paint: &dyn Fn(&App, u64)) -> CardGlass {
    let (mut app, wid, session) = glass(cols);
    paint(&app, session);
    let host = supervise(&mut app, session);
    let t0 = Instant::now();
    for t in [0, FRAME_MS, EPISODE_MS + 1] {
        set_now(Some(t0 + Duration::from_millis(t)));
        frame(&mut app, wid);
    }
    let glass = card_glass(&app, wid);
    host.shutdown_and_join();
    glass
}

/// EVERY CHANGE THE CARD READS IS READ, AND THE CARD IS A FRESH WINDOW'S:
/// one settled card is taken through a run of screens, each framed and then
/// held for a second pass, and after each its glass — light, ink, star and
/// line cells — is the glass a fresh window draws over the same screen, bit
/// for bit. A screen that moved nothing the card reads (the same screen
/// again, a keystroke in the composer, a row of output under it) costs no
/// reading and no layout; a cell in each region the layout reads — the row
/// under the card where the line goes, the card's rows right of their text
/// where the kitty runs — laid anew, and so is a COLOUR-ONLY change of one
/// (a coloured space is no blank; its row's text is the same), a combining
/// mark on a space there (a side channel, the text the same) and a wide
/// glyph; a card row whose last glyph turns wide or narrow again with as
/// many chars (the same place: only the glyph's spacer cell says the row is
/// a cell longer) is laid anew; a newer card printed below, the card
/// scrolled, its own words edited and a resize read it anew; and the pass
/// after each keeps what it made. An anchor right of where the line goes is
/// past what the layout reads, and costs nothing.
/// CONTROL: the comparisons are not vacuous — the line leaves the row under
/// the card, the kitty moves, the card follows a scroll (each step's glass
/// differs from the settled one's). Mutation-checked: a kept layout blind to
/// colour, to the side channels, to the row under the card, to the kitty's
/// rows or to a card row's wide last glyph (`Seen`'s wide tests), and a
/// reading kept across a changed card row or a newer banner, each fail here.
#[test]
fn a_change_the_card_reads_lays_it_as_a_fresh_window_would() {
    let t0 = Instant::now();
    let (mut app, wid, session, host) = settled_glass(120, t0);
    let base = card_screen(24, 120, MODEL, VERSION);
    let settled = card_glass(&app, wid);
    let edit = |k: usize, row: String| {
        let mut rows = base.clone();
        rows[k] = row;
        rows
    };
    let pad = |k: usize, at: usize, tail: &str| format!("{:<at$}{tail}", base[k]);
    // A space in background colour `bg` (SGR) at pane cell (row, col), the
    // cursor kept: no blank, though its row's text reads the same.
    let space = |bg: u8, row: usize, col: usize| {
        format!("\x1b7\x1b[{};{}H\x1b[{bg}m \x1b[0m\x1b8", row + 1, col + 1)
    };
    // `text` at pane cell (row, col), the cursor kept.
    let at = |row: usize, col: usize, text: &str| {
        format!("\x1b7\x1b[{};{}H{text}\x1b8", row + 1, col + 1)
    };
    let mut newer = base.clone();
    newer[10..13].clone_from_slice(&base[1..4]);
    let mut scrolled = base[1..base.len() - 4].to_vec();
    scrolled.push(String::new());
    scrolled.extend_from_slice(&base[base.len() - 4..]);
    let mut composer = base.clone();
    composer[base.len() - 3] = "\u{276F} keep going".into();
    let mut output = base.clone();
    output[9] = "\u{23FA} Here is what changed in the footer.".into();
    // Each step: what it is, the screen's rows, bytes painted over them, the
    // readings and layouts its frame costs, and whether the card moved from
    // the settled one.
    type Step<'a> = (&'a str, Vec<String>, String, (usize, usize), bool);
    let steps: Vec<Step> = vec![
        (
            "the same screen",
            base.clone(),
            String::new(),
            (0, 0),
            false,
        ),
        ("a keystroke", composer, String::new(), (0, 0), false),
        (
            "output under the card",
            output,
            String::new(),
            (0, 0),
            false,
        ),
        (
            "words where the line goes",
            edit(4, pad(4, 40, "word")),
            String::new(),
            (0, 1),
            true,
        ),
        ("back", base.clone(), String::new(), (0, 1), false),
        (
            "an anchor right of the line",
            edit(4, pad(4, 100, "zz")),
            String::new(),
            (0, 0),
            false,
        ),
        (
            "a coloured space where the line goes",
            edit(4, pad(4, 100, "zz")),
            space(44, 4, 40),
            (0, 1),
            true,
        ),
        (
            "words where the kitty runs",
            edit(2, pad(2, 60, "WORD")),
            String::new(),
            (1, 1),
            true,
        ),
        (
            "a coloured anchor right of the kitty",
            base.clone(),
            space(41, 2, 112),
            (1, 1),
            false,
        ),
        (
            "a coloured space where the kitty runs",
            base.clone(),
            space(41, 2, 112) + &space(44, 2, 50),
            (0, 1),
            true,
        ),
        (
            "a coloured anchor right of the banner",
            base.clone(),
            space(41, 1, 112),
            (1, 1),
            false,
        ),
        (
            "a coloured space beside the banner",
            base.clone(),
            space(41, 1, 112) + &space(44, 1, 50),
            (0, 1),
            true,
        ),
        (
            "a plain space where the kitty runs",
            base.clone(),
            at(2, 50, " "),
            (1, 1),
            false,
        ),
        (
            "a combining mark on it",
            base.clone(),
            at(2, 50, " \u{301}"),
            (0, 1),
            true,
        ),
        (
            "a wide glyph where the line goes",
            base.clone(),
            at(4, 40, "\u{4E2D}"),
            (1, 1),
            true,
        ),
        (
            "the model row's last glyph wide",
            card_screen(24, 120, MODEL_WIDE_END, VERSION),
            String::new(),
            (1, 1),
            true,
        ),
        (
            "and narrow again",
            base.clone(),
            String::new(),
            (1, 1),
            false,
        ),
        ("a newer card below", newer, String::new(), (1, 1), true),
        ("the card scrolled", scrolled, String::new(), (1, 1), true),
        (
            "its words edited",
            card_screen(24, 120, "Sonnet 5.5", VERSION),
            String::new(),
            (1, 1),
            true,
        ),
        (
            "the settled card again",
            base.clone(),
            String::new(),
            (1, 1),
            false,
        ),
    ];
    for (i, (what, rows, extra, (reads, layouts), moved)) in steps.into_iter().enumerate() {
        let paint = |app: &App, session: u64| {
            feed(app, session, &rows);
            let term = app.pool.get(session).expect("session").term.clone();
            crate::term_lock(&term).process(extra.as_bytes());
        };
        paint(&app, session);
        set_now(Some(t0 + Duration::from_secs(10 + i as u64)));
        app.capture_words(wid, Instant::now());
        app.splice_tab_strip(wid);
        let cost = pass_cost(&mut app, wid);
        assert_eq!(
            (cost.reads, cost.layouts),
            (reads, layouts),
            "{what}: {cost:?}"
        );
        let kept = card_glass(&app, wid);
        assert_eq!(
            kept,
            fresh_card(120, &paint),
            "{what}: a fresh window's card"
        );
        assert_eq!(kept != settled, moved, "{what}: CONTROL, the card moved");
        set_now(Some(
            t0 + Duration::from_secs(10 + i as u64) + Duration::from_millis(500),
        ));
        let again = pass_cost(&mut app, wid);
        assert_eq!(
            (again.reads, again.layouts, again.ticks),
            (0, 0, 0),
            "{what}: then kept"
        );
        assert_eq!(card_glass(&app, wid), kept, "{what}: then kept");
    }
    // A resize: the pane's rows and columns move, and the card is read and
    // laid anew for them.
    assert!(app.apply_term_resize(wid, 24, 100));
    let narrower = card_screen(24, 100, MODEL, VERSION);
    feed(&app, session, &narrower);
    app.capture_words(wid, Instant::now());
    app.splice_tab_strip(wid);
    let cost = pass_cost(&mut app, wid);
    assert_eq!((cost.reads, cost.layouts), (1, 1), "a resize: {cost:?}");
    assert_eq!(
        card_glass(&app, wid),
        fresh_card(100, &|app, session| feed(app, session, &narrower)),
        "a resize: a fresh window's card"
    );
    set_now(None);
    host.shutdown_and_join();
}

/// A split window 160 wide: its left pane a shell's screen, its right pane
/// (its cells at a column offset) Claude Code's with the card, the facts of
/// its process published; then `left` and `right` painted over the two.
/// Returns the window, the two sessions and the right pane's width.
fn split_card(left: &str, right: &str) -> (App, WindowId, u64, u64, usize) {
    let (mut app, wid, shell) = glass(160);
    let session = app.split_active_stub_tab(wid);
    let (rows, cols) = {
        let term = crate::term_lock(&app.pool.get(session).unwrap().term);
        (usize::from(term.rows()), usize::from(term.cols()))
    };
    let shell_cols = {
        let term = crate::term_lock(&app.pool.get(shell).unwrap().term);
        usize::from(term.cols())
    };
    let lines: Vec<String> = (0..rows).map(|r| format!("$ ls {r}")).collect();
    feed(&app, shell, &lines);
    feed(&app, session, &card_screen(rows, cols, MODEL, VERSION));
    publish(&app, session, VERSION);
    for (s, bytes) in [(shell, left), (session, right)] {
        let term = app.pool.get(s).expect("session").term.clone();
        crate::term_lock(&term).process(bytes.as_bytes());
    }
    assert!(
        shell_cols > 20 && cols > 60,
        "two panes: {shell_cols} | {cols}"
    );
    (app, wid, shell, session, cols)
}

/// THE SAME IN A SPLIT, the card's pane on the right (its cells at a column
/// offset, its neighbour's text up against its left edge): a settled card
/// kept across frames that the split composes anew each time draws what a
/// fresh split window draws over the same two screens, bit for bit. Words up
/// to the boundary in the neighbour's pane on the card's rows, and a wide
/// glyph in its last columns, cost the card no reading and no layout; in the
/// card's own pane a plain space where the kitty runs costs nothing (the
/// composed rows' text and cells read the same), a combining
/// mark put on that space — the text the same, the cell no blank — lays it
/// anew, a coloured space in its place keeps that (the cells the layout read
/// say what they said: the kitty still stops there), a wide glyph where the
/// line goes lays it anew, and the screens back again. CONTROL: the
/// comparisons are not vacuous (each change that moves the card moves its
/// glass). Mutation-checked: a kept layout's check blind to the side
/// channels, or to the pane's column offset, fails here.
#[test]
fn a_change_in_a_split_lays_the_card_as_a_fresh_split_would() {
    let fresh = |left: &str, right: &str| {
        let (mut app, wid, _, session, _) = split_card(left, right);
        let host = supervise(&mut app, session);
        let t0 = Instant::now();
        for t in [0, FRAME_MS, EPISODE_MS + 1] {
            set_now(Some(t0 + Duration::from_millis(t)));
            frame(&mut app, wid);
        }
        let glass = card_glass(&app, wid);
        host.shutdown_and_join();
        glass
    };
    let t0 = Instant::now();
    let (mut app, wid, shell, session, cols) = split_card("", "");
    let host = supervise(&mut app, session);
    for t in [0, FRAME_MS, EPISODE_MS + 1] {
        set_now(Some(t0 + Duration::from_millis(t)));
        frame(&mut app, wid);
    }
    let settled = card_glass(&app, wid);
    assert!(
        !settled.under.is_empty() && !settled.over.is_empty(),
        "the card and its kitty"
    );
    let shell_cols = usize::from(crate::term_lock(&app.pool.get(shell).unwrap().term).cols());
    let at = |row: usize, col: usize, text: &str| {
        format!("\x1b7\x1b[{};{}H{text}\x1b8", row + 1, col + 1)
    };
    let up_to_the_edge: String = (1..=4)
        .map(|r| at(r, shell_cols - 12, &"x".repeat(12)))
        .collect();
    let wide_at_the_edge = at(2, shell_cols - 2, "\u{4E2D}");
    let plain = at(2, 50, " ");
    let combining = at(2, 50, " \u{301}");
    let coloured = at(2, 50, "\x1b[44m \x1b[0m");
    let wide_line = at(4, 40, "\u{4E2D}");
    // Each step: the neighbour's bytes, the card pane's, the readings and
    // layouts the frame costs, and whether the card moved.
    type Step<'a> = (&'a str, String, String, (usize, usize), bool);
    let steps: Vec<Step> = vec![
        (
            "the same screens",
            String::new(),
            String::new(),
            (0, 0),
            false,
        ),
        (
            "words up to the edge",
            up_to_the_edge.clone(),
            String::new(),
            (0, 0),
            false,
        ),
        (
            "a wide glyph at the edge",
            up_to_the_edge.clone() + &wide_at_the_edge,
            String::new(),
            (0, 0),
            false,
        ),
        (
            "a plain space where the kitty runs",
            up_to_the_edge.clone(),
            plain,
            (0, 0),
            false,
        ),
        (
            "a combining mark on it",
            up_to_the_edge.clone(),
            combining,
            (0, 1),
            true,
        ),
        (
            "a coloured space instead",
            up_to_the_edge.clone(),
            coloured,
            (0, 0),
            true,
        ),
        (
            "a wide glyph where the line goes",
            up_to_the_edge.clone(),
            wide_line,
            (0, 1),
            true,
        ),
        ("back", String::new(), String::new(), (0, 1), false),
    ];
    let shell_lines: Vec<String> = (0..24).map(|r| format!("$ ls {r}")).collect();
    for (i, (what, left, right, (reads, layouts), moved)) in steps.into_iter().enumerate() {
        let rows = usize::from(crate::term_lock(&app.pool.get(session).unwrap().term).rows());
        feed(&app, shell, &shell_lines[..rows]);
        feed(&app, session, &card_screen(rows, cols, MODEL, VERSION));
        for (s, bytes) in [(shell, &left), (session, &right)] {
            let term = app.pool.get(s).expect("session").term.clone();
            crate::term_lock(&term).process(bytes.as_bytes());
        }
        set_now(Some(t0 + Duration::from_secs(10 + i as u64)));
        app.capture_words(wid, Instant::now());
        app.splice_tab_strip(wid);
        let cost = pass_cost(&mut app, wid);
        assert_eq!(
            (cost.reads, cost.layouts),
            (reads, layouts),
            "{what}: {cost:?}"
        );
        let kept = card_glass(&app, wid);
        assert_eq!(kept, fresh(&left, &right), "{what}: a fresh split's card");
        assert_eq!(kept != settled, moved, "{what}: CONTROL, the card moved");
    }
    set_now(None);
    host.shutdown_and_join();
}

/// A WINDOW HIDDEN MID-EPISODE (minimized, occluded: its splice never runs
/// again) keeps its card on show, playing, and the loop's two judges read
/// the clock on every wake while the episode could still want a frame —
/// the window may come back. Past the episode and its settle grace the card
/// wants no frame at any later instant until a splice ticks it, and the
/// first wake that finds so lets it go: every wake after costs no clock read
/// and asks the window server nothing. Shown again, its frame settles the
/// card; focus regained plays it again.
/// CONTROLS: mid-episode the hidden window's frame is wanted and the judges
/// ask (two clock reads a wake); one shown again mid-episode is due at once;
/// and without the let-go every later wake reads the clock twice, for good
/// (mutation-checked).
#[test]
fn a_window_hidden_mid_episode_costs_the_wakes_nothing() {
    let (mut app, wid, session) = glass(120);
    let host = supervise(&mut app, session);
    app.windows.get_mut(&wid).unwrap().band_on_screen_for_test = true;
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    set_now(Some(t0));
    frame(&mut app, wid);
    let hide = |app: &mut App, hidden: bool| app.windows.get_mut(&wid).unwrap().occluded = hidden;
    // One loop wake's judges at `now`: what they ask for, and what they cost.
    let wake = |app: &mut App, now: Instant| {
        set_now(Some(now));
        let _ = (
            crate::work_counts::take_harness_clock_reads(),
            crate::work_counts::take_on_screen_asks(),
        );
        let judged = (app.claude_card_redraws(), app.claude_card_wake());
        (
            judged,
            crate::work_counts::take_harness_clock_reads(),
            crate::work_counts::take_on_screen_asks(),
        )
    };
    hide(&mut app, true);
    assert_eq!(
        wake(&mut app, at(100)),
        ((vec![], None), 2, 2),
        "CONTROL: mid-episode, the hidden window's frame is wanted and asked about"
    );
    // CONTROL: shown again mid-episode, its frame is due at once.
    hide(&mut app, false);
    assert_eq!(
        wake(&mut app, at(110)).0.0,
        vec![wid],
        "CONTROL: due when shown"
    );
    frame(&mut app, wid);
    hide(&mut app, true);
    let past = EPISODE_MS + aterm_effects::harness_card::SETTLE_GRACE_MS + 10;
    assert_eq!(
        wake(&mut app, at(past)),
        ((vec![], None), 1, 0),
        "the wake that finds no frame wanted any more"
    );
    for i in 1..=100 {
        assert_eq!(
            wake(&mut app, at(past + 7 * i)),
            ((vec![], None), 0, 0),
            "wake {i} after: nothing"
        );
    }
    assert!(app.windows[&wid].claude_card.any_shown(), "still on show");
    // Shown again: its frame draws the card settled, and it stays at rest.
    hide(&mut app, false);
    set_now(Some(at(past + 1000)));
    frame(&mut app, wid);
    assert!(
        app.windows[&wid]
            .claude_card
            .entries
            .iter()
            .all(|e| e.card.at_rest()),
        "settled"
    );
    assert_eq!(wake(&mut app, at(past + 1100)), ((vec![], None), 0, 0));
    // Focus regained: the episode again.
    let back = at(past + 2000);
    app.rearm_claude_cards(wid, || back);
    assert_eq!(wake(&mut app, back).0, (vec![wid], None), "playing again");
    set_now(None);
    host.shutdown_and_join();
}

/// AN EPISODE CUT OFF BY LOST FOCUS RESTS AT NO COST: focus lost 66 ms into
/// the episode, the next frame draws the card still — its settled frame,
/// once — and the episode keeps its clock, held back. A minute on, frame
/// after frame costs the harness nothing over the bare footer's pass: the
/// held card is replayed (no tick), no clock read, its line kept.
/// NEGATIVE CONTROL: the same window still focused 66 ms in draws the
/// episode's frame on the clock; and a replay that refused a held-back
/// playing episode (before 82f408659) ticks every frame a minute on
/// (mutation-checked).
#[test]
fn an_episode_cut_off_by_lost_focus_rests_at_no_cost() {
    for lose_focus in [false, true] {
        let t0 = Instant::now();
        let (mut app, wid, session) = glass(120);
        let host = supervise(&mut app, session);
        let (mut bare, bare_wid, _) = bare_glass(120);
        bare.windows.get_mut(&bare_wid).unwrap().focused = false;
        for t in [0, FRAME_MS] {
            set_now(Some(t0 + Duration::from_millis(t)));
            frame(&mut app, wid);
        }
        app.windows.get_mut(&wid).unwrap().focused = !lose_focus;
        set_now(Some(t0 + Duration::from_millis(2 * FRAME_MS)));
        let cut = pass_cost(&mut app, wid);
        assert_eq!(
            cut.ticks, 1,
            "lose focus {lose_focus}: 66 ms in, a frame is drawn"
        );
        if !lose_focus {
            // NEGATIVE CONTROL: focused, that frame is the episode's.
            assert!(cut.clock_reads > 0, "CONTROL: on the clock");
            assert!(!app.windows[&wid].claude_card.entries[0].card.is_still());
            host.shutdown_and_join();
            continue;
        }
        let held = &app.windows[&wid].claude_card.entries[0].card;
        assert!(
            held.at_rest() && !held.is_still(),
            "held back, its clock kept"
        );
        let quads = painted(&app, wid).under.clone();
        let _ = pass_cost(&mut bare, bare_wid);
        for i in 0..20u64 {
            set_now(Some(
                t0 + Duration::from_secs(60) + Duration::from_millis(7 * i),
            ));
            let floor = pass_cost(&mut bare, bare_wid);
            assert_eq!(pass_cost(&mut app, wid), floor, "a minute on, frame {i}");
            assert_eq!(painted(&app, wid).under, quads, "frame {i}: the held frame");
        }
        set_now(None);
        host.shutdown_and_join();
    }
}

/// A zone re-derivation can keep HH:MM and another-day presence while
/// changing the weekday. The same timer that changes the card's words
/// must move the footer key that admits their next composition.
#[test]
fn a_timer_weekday_change_moves_the_footer_key_with_its_card_words() {
    use crate::autopilot_timers::AutopilotTimers;
    use crate::claude_lights::{ChipGlass, WindowLights};

    let now = Instant::now();
    let wall = 20_000 * 86_400_000_i64 + 12 * 3_600_000;
    let at = wall + 86_400_000;
    let session = 17;
    let mut timers = AutopilotTimers::default();
    let lights = WindowLights::default();
    let west = |_: i64| Some(-10 * 3600);
    let east = |_: i64| Some(14 * 3600);
    let term = |glass| {
        lights.autopilot_term(
            session,
            Some(Autopilot::Cruise),
            AutopilotView::Cruise,
            ChipGlass {
                land_at: glass,
                ..ChipGlass::default()
            },
        )
    };
    let words = |glass| {
        line_texts(
            AutopilotView::Cruise,
            LineEnd::Mode.timed(AutopilotView::Cruise, glass),
        )
    };

    assert!(timers.arm(session, at, now, wall, &west, None));
    let before = timers.glass(session).expect("armed face");
    let before_key = term(Some(before));
    let before_words = words(Some(before));
    assert_ne!(before_key, term(None), "CONTROL: arming moves the key");
    assert_ne!(before_words, words(None), "CONTROL: armed timer words");
    assert!(timers.rederive(now, wall, &west).is_empty());
    assert_eq!(term(timers.glass(session)), before_key);
    assert_eq!(words(timers.glass(session)), before_words);

    assert_eq!(timers.rederive(now, wall, &east), vec![session]);
    let after = timers.glass(session).expect("re-derived face");
    assert_eq!(before.clock, after.clock, "the same HH:MM");
    assert!(before.other_day.is_some() && after.other_day.is_some());
    assert_ne!(before.other_day, after.other_day);
    assert_ne!(words(Some(after)), before_words, "the card's words moved");
    assert_ne!(
        term(Some(after)),
        before_key,
        "TIMER_WEEKDAY_DISPLAY_MUST_MOVE_FOOTER_KEY"
    );
}
