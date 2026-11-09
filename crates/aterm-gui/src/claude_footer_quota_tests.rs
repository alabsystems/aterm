// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for line E, the quota row ([`super`]): `docs/DESIGN-harness-glass-rainbow-2026-11-05.md`
//! §1.3, §2.6, §5.2 and §5.3, on Claude Code's own screens (aterm-phase's
//! `claude-2.1.318-tap-row-*` and `claude-2.1.300-tap-row-*` fixtures) and the design's four
//! cases ([`aterm_agent::harness::quota_scenes`]).

use std::time::{Duration, Instant, SystemTime};

use aterm_agent::harness::footer::{self, FactKind, FooterFacts};
use aterm_agent::harness::outlook::Offsets;
use aterm_agent::harness::profile::SubscriptionDisplay;
use aterm_agent::harness::quota_scenes;
use aterm_agent::harness::quota_view::{QuotaLine, QuotaView, Role};
use aterm_core::terminal::{RenderCell, UnderlineStyle};
use aterm_effects::glass_inks::{GlassInks, field};
use aterm_effects::harness_card::CardPalette;
use aterm_window::event::{ElementState, MouseButton};

use super::super::{probe, rgb_of};
use super::{GRACE, INDENT, QuotaKey, RowSpan, compose_cells, row_span, time_the_grace};
use crate::claude_lights::{Fact, FactHit, RowPart};
use crate::{App, VisibleContentRoute, WindowId};

/// The tap's words as the fixtures' stand-in command prints them.
const WORDS: &str = "week 63% used \u{b7} resets Sat 10:00";

/// The rows of every fixture pane.
const ROWS: usize = 24;

/// Case `name` of the design (§1), as `main`'s tab sees it.
fn case(name: &str) -> QuotaView {
    quota_scenes::cases()
        .into_iter()
        .find(|(n, _)| *n == name)
        .map(|(_, view)| view)
        .unwrap_or_else(|| panic!("case ({name})"))
}

/// Claude Code's screen in aterm-phase's fixture `name` as a [`ROWS`]-row pane shows it (its
/// last rows, under blank ones where the capture is shorter), and the pane's width (its rules').
/// A row of the 2.1.300 captures that holds the old five-hour words (`5h 100% · 7d 61%`) has the
/// tap's week words in their place, the notice beside them where it was.
fn screen(name: &str) -> (u16, Vec<String>) {
    const OLD: &str = "  5h 100% \u{b7} 7d 61%";
    let path = aterm_tempfile::manifest_dir()
        .join("../aterm-phase/src/fixtures")
        .join(format!("{name}.txt"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let rows: Vec<String> = text
        .lines()
        .skip(1)
        .map(|row| match row.strip_prefix(OLD) {
            Some(rest) => {
                let notice = rest.trim_start();
                let room = OLD.chars().count() - 2 + rest.len() - notice.len();
                format!("  {WORDS:<room$}{notice}")
            }
            None => row.to_owned(),
        })
        .collect();
    let cols = rows
        .iter()
        .find(|r| r.starts_with('\u{2500}'))
        .map(|r| r.chars().count())
        .expect("a rule");
    let mut shown = vec![String::new(); ROWS.saturating_sub(rows.len())];
    shown.extend_from_slice(&rows[rows.len().saturating_sub(ROWS)..]);
    (u16::try_from(cols).expect("a width"), shown)
}

/// The pane row of the composer's bottom rule on `rows`.
fn bottom(rows: &[String]) -> usize {
    rows.iter()
        .rposition(|r| r.starts_with('\u{2500}'))
        .expect("the bottom rule")
}

/// A [`ROWS`]-row pane `cols` wide showing `rows`.
fn pane(cols: u16, rows: &[String]) -> (App, WindowId, u64) {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let session = app.focused_session_id(wid).expect("the front session");
    // The window starts 80 by 24, where a resize to its own grid reports no change.
    let _ = app.apply_term_resize(wid, ROWS as u16, cols);
    let ws = &app.windows[&wid];
    assert_eq!(
        (usize::from(ws.rows), usize::from(ws.cols)),
        (ROWS, usize::from(cols))
    );
    probe::feed(&app, session, rows);
    (app, wid, session)
}

/// The facts a Claude Code pane publishes, with `view`, and a tap whose newest reading says the
/// row is its own (`tap_own`) or an owner's command's.
fn facts_with(view: Option<QuotaView>, tap_own: bool) -> FooterFacts {
    FooterFacts {
        account: Some("main".into()),
        subscription: Some(SubscriptionDisplay {
            tap_own,
            ..SubscriptionDisplay::default()
        }),
        quota: view,
        ..probe::facts()
    }
}

/// One frame through the capture route's splices, the footer's clock reading `at`.
fn frame_at(app: &mut App, wid: WindowId, at: SystemTime) {
    let prepared = app.prepare_terminal_capture_grid_with_cursor_fx_and_plan_outcome(
        wid,
        crate::app_render::ComposedCursorFxClock::Advance(Instant::now()),
    );
    let crate::app_render::CapturePreparation::Ready((grid, plan)) = prepared else {
        panic!("the headless capture must produce a frame");
    };
    app.splice_tab_strip(wid);
    let route = VisibleContentRoute::Terminal {
        composed: grid.composed,
    };
    let now = at
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0));
    let clock = move || footer::ReadClock {
        read_at: at,
        now,
        offset_at: &super::super::offset_at_cached,
    };
    app.splice_claude_footer_at(wid, &plan, route, &clock);
}

fn frame(app: &mut App, wid: WindowId) {
    frame_at(app, wid, SystemTime::now());
}

/// The frame row of pane row `row`.
fn frame_row(app: &App, wid: WindowId, row: usize) -> usize {
    let ws = &app.windows[&wid];
    ws.input_scratch.cells.len() - usize::from(ws.rows) + row
}

/// Pane row `row` as the glass shows it, a char a column.
fn glass(app: &App, wid: WindowId, row: usize) -> String {
    app.windows[&wid].input_scratch.cells[frame_row(app, wid, row)]
        .iter()
        .map(|c| c.ch)
        .collect()
}

/// Pane row `row` of the grid: what a copy and a recording read.
fn grid(app: &App, session: u64, row: usize) -> String {
    let term = app.pool.get(session).expect("session").term.clone();
    crate::term_lock(&term).row_text(row).expect("the row")
}

/// The quota row's hits on the glass.
fn row_hits(app: &App, wid: WindowId) -> Vec<FactHit> {
    app.windows[&wid]
        .claude_lights
        .fact_hits
        .iter()
        .filter(|h| h.part.is_some())
        .copied()
        .collect()
}

/// Whether line D, the bottom rule, carries the account unit: the account's hit off line E.
fn line_d_unit(app: &App, wid: WindowId) -> bool {
    app.windows[&wid]
        .claude_lights
        .fact_hits
        .iter()
        .any(|h| h.fact == Fact::Account && h.part.is_none())
}

/// `view` laid in `room` cells.
fn laid(view: &QuotaView, room: usize) -> QuotaLine {
    let mut line = QuotaLine::default();
    assert!(view.lay(room, &mut line), "a rung fits {room}");
    line
}

/// The window pixel at the centre of frame cell `(row, col)`.
fn px_of(app: &App, wid: WindowId, (row, col): (usize, usize)) -> (f64, f64) {
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

/// Rest the pointer on frame cell `at` and paint.
fn hover(app: &mut App, wid: WindowId, at: (usize, usize)) {
    let (x, y) = px_of(app, wid, at);
    app.on_cursor_moved(wid, x, y);
    frame(app, wid);
}

/// A pane on Claude's screen `name` whose tap's newest reading says line E is its own, `view`
/// published, two frames painted.
fn claimed(name: &str, view: &QuotaView) -> (App, WindowId, u64, Vec<String>) {
    let (cols, rows) = screen(name);
    let (mut app, wid, session) = pane(cols, &rows);
    probe::publish(&app, session, facts_with(Some(view.clone()), true));
    frame(&mut app, wid);
    frame(&mut app, wid);
    (app, wid, session, rows)
}

/// LINE E IS GLASS ONLY OVER THE TAP'S OWN ROW (§5.2): on Claude Code 2.1.318's screen at 170
/// columns, the row under the bottom rule reading the tap's words and the newest reading saying
/// the row is the tap's, the glass lays case (b) from column 2 in the room of §2.4 (166 cells),
/// the grid keeps the tap's words, the row is the account in three hits and line D carries no
/// unit. CONTROLS: the same words on a row an owner's command draws (the reading says so) and a
/// row that is not the tap's words both stay Claude's, with no hit, and line D names the account.
#[test]
fn line_e_is_glass_only_over_the_taps_own_row() {
    let view = case("b");
    let (app, wid, session, rows) = claimed("claude-2.1.318-tap-row-week", &view);
    let tap = bottom(&rows) + 1;
    let line = laid(&view, 166);
    let shown = glass(&app, wid, tap);
    assert!(
        shown.starts_with(&format!("  {}", line.text())),
        "the row is case (b) at 166 cells: {shown:?}"
    );
    assert_eq!(
        grid(&app, session, tap).trim_end(),
        format!("  {WORDS}"),
        "the grid keeps the words"
    );
    let hits = row_hits(&app, wid);
    let parts: Vec<RowPart> = hits.iter().filter_map(|h| h.part).collect();
    assert_eq!(
        parts,
        [RowPart::Lead, RowPart::Chart(line.per_day), RowPart::Tail]
    );
    assert!(hits.iter().all(|h| h.fact == Fact::Account));
    assert_eq!(hits[0].cols.0, INDENT, "from column 2");
    assert!(!line_d_unit(&app, wid), "line D carries no unit");

    // CONTROL: the owner's row.
    let (cols, rows) = screen("claude-2.1.318-tap-row-week");
    let (mut app, wid, session) = pane(cols, &rows);
    probe::publish(&app, session, facts_with(Some(view.clone()), false));
    frame(&mut app, wid);
    assert_eq!(glass(&app, wid, tap).trim_end(), format!("  {WORDS}"));
    assert!(row_hits(&app, wid).is_empty());
    assert!(line_d_unit(&app, wid), "line D names the account");

    // CONTROL: a row that is not the tap's words.
    let mut other = rows;
    other[tap] = "  my own status line".into();
    let (mut app, wid, session) = pane(cols, &other);
    probe::publish(&app, session, facts_with(Some(view), true));
    frame(&mut app, wid);
    assert_eq!(glass(&app, wid, tap).trim_end(), "  my own status line");
    assert!(row_hits(&app, wid).is_empty());
    assert!(line_d_unit(&app, wid));
}

/// A SELECTION OR THE CURSOR KEEPS THE TAP'S WORDS (§5.2, the third condition): with the cursor
/// on line E, or a selection on the screen, the row stands as Claude drew it and is no hit, yet
/// the frame counts as claimed: line D still carries no unit. CONTROLS: the cursor back on the
/// composer, and the selection let go, lay the glass again.
#[test]
fn a_selection_or_the_cursor_keeps_the_taps_words() {
    use aterm_core::selection::{SelectionSide, SelectionType};
    let view = case("b");
    let (mut app, wid, session, rows) = claimed("claude-2.1.318-tap-row-week", &view);
    let tap = bottom(&rows) + 1;
    assert_eq!(row_hits(&app, wid).len(), 3, "CONTROL: claimed");
    probe::bytes(&app, session, format!("\x1b[{};3H", tap + 1).as_bytes());
    frame(&mut app, wid);
    assert_eq!(
        glass(&app, wid, tap).trim_end(),
        format!("  {WORDS}"),
        "the cursor's row"
    );
    assert!(row_hits(&app, wid).is_empty());
    assert!(!line_d_unit(&app, wid), "held, it counts as claimed");
    let caret = rows
        .iter()
        .rposition(|r| r.starts_with('\u{276F}'))
        .expect("caret");
    probe::bytes(&app, session, format!("\x1b[{};3H", caret + 1).as_bytes());
    frame(&mut app, wid);
    assert_eq!(row_hits(&app, wid).len(), 3, "CONTROL: the cursor gone");

    let term = app.pool.get(session).expect("session").term.clone();
    let select = |on: bool| {
        let mut t = crate::term_lock(&term);
        let selection = t.text_selection_mut();
        selection.clear();
        if on {
            selection.start_selection(0, 2, SelectionSide::Left, SelectionType::Simple);
            selection.update_selection(0, 20, SelectionSide::Right);
            selection.complete_selection();
        }
    };
    select(true);
    frame(&mut app, wid);
    assert!(
        app.windows[&wid].input_scratch.any_selection(),
        "a selection"
    );
    assert_eq!(
        glass(&app, wid, tap).trim_end(),
        format!("  {WORDS}"),
        "a selection on the screen"
    );
    assert!(row_hits(&app, wid).is_empty());
    assert!(!line_d_unit(&app, wid));
    select(false);
    frame(&mut app, wid);
    assert_eq!(row_hits(&app, wid).len(), 3, "CONTROL: let go");
}

/// THE GLASS STOPS TWO CELLS BEFORE CLAUDE'S NOTICE (§5.2): on 2.1.300's 80-column screen with
/// `Now using usage credits` right-aligned on the tap's row (from column 55), the room runs from
/// column 2 to column 53: no write reaches the two blanks before the notice, and the notice is
/// Claude's, unchanged. CONTROL: with no notice the room runs to `cols − 2`.
#[test]
fn the_glass_stops_two_cells_before_claudes_notice() {
    let view = case("b");
    let words = WORDS.chars().count();
    let (app, wid, _, rows) = claimed("claude-2.1.300-tap-row-80-credits", &view);
    let tap = bottom(&rows) + 1;
    let notice = rows[tap].find("Now").expect("the notice");
    let notice = rows[tap][..notice].chars().count();
    assert_eq!(notice, 55, "the capture's notice column is kept");
    assert_eq!(row_span(&rows[tap], 80), Some(RowSpan { words, room: 51 }));
    assert_eq!(
        row_span(&format!("  {WORDS}"), 80),
        Some(RowSpan { words, room: 76 }),
        "CONTROL: no notice"
    );
    let hits = row_hits(&app, wid);
    assert!(!hits.is_empty(), "claimed");
    let end = hits.iter().map(|h| h.cols.1).max().expect("hits");
    assert_eq!(
        end,
        INDENT + laid(&view, 51).cells.len().max(words),
        "the rung that fits 51"
    );
    assert!(end <= notice - 2, "the glass ends by column 53: {end}");
    let at = frame_row(&app, wid, tap);
    let spans: Vec<_> = app.windows[&wid]
        .claude_footer_undo
        .painted_spans()
        .filter(|s| s.0 == at)
        .collect();
    assert!(!spans.is_empty());
    assert!(
        spans.iter().all(|&(_, col, n)| col + n <= notice - 2),
        "no write reaches the notice: {spans:?}"
    );
    let tail: String = glass(&app, wid, tap).chars().skip(notice - 2).collect();
    assert_eq!(tail.trim_end(), "  Now using usage credits");
}

/// THE GLASS COVERS THE TAP'S WORDS BESIDE A CLOSE NOTICE (§5.2): in manual mode Claude sets its
/// notice one blank after the words (`-credits-manual-60col`); the span is then the words alone,
/// a rung that fits them is laid there, and the notice keeps its cells.
#[test]
fn the_glass_covers_the_taps_words_beside_a_close_notice() {
    let view = case("b");
    let words = WORDS.chars().count();
    let (app, wid, _, rows) = claimed("claude-2.1.318-tap-row-week-credits-manual-60col", &view);
    let tap = bottom(&rows) + 1;
    assert_eq!(
        row_span(&rows[tap], 60),
        Some(RowSpan { words, room: words })
    );
    let hits = row_hits(&app, wid);
    assert!(!hits.is_empty(), "claimed");
    assert!(hits.iter().all(|h| h.cols.1 <= INDENT + words), "{hits:?}");
    let shown = glass(&app, wid, tap);
    assert!(
        shown.starts_with(&format!("  {}", laid(&view, words).text())),
        "{shown:?}"
    );
    let tail: String = shown.chars().skip(INDENT + words).collect();
    assert_eq!(tail.trim_end(), " Now using usage credits");
}

/// THE LAUNCH EFFORT HINT KEEPS ITS CELLS (§5.2): at launch Claude sets `◐ medium · /effort` on
/// the tap's row from column 150 of 170; the glass stops two cells before it and the hint stays
/// Claude's.
#[test]
fn the_launch_effort_hint_keeps_its_cells() {
    let view = case("b");
    let (app, wid, _, rows) = claimed("claude-2.1.318-tap-row-week-launch", &view);
    let tap = bottom(&rows) + 1;
    let hint = rows[tap].find('\u{25D0}').expect("the hint");
    let hint = rows[tap][..hint].chars().count();
    assert_eq!(hint, 150);
    assert_eq!(
        row_span(&rows[tap], 170).map(|s| s.room),
        Some(hint - 2 - INDENT)
    );
    let hits = row_hits(&app, wid);
    assert_eq!(hits.len(), 3, "claimed");
    assert!(hits.iter().all(|h| h.cols.1 <= hint - 2), "{hits:?}");
    let tail: String = glass(&app, wid, tap).chars().skip(hint - 2).collect();
    assert_eq!(tail.trim_end(), "  \u{25D0} medium \u{b7} /effort");
}

/// A CUT ROW IS NEVER CLAIMED (§5.2): below about 36 columns Claude cuts the row with `…`
/// (`-30col`), which is not the tap's words: no glass, no hit, and line D's unit, where it fits,
/// is the account's. CONTROL: the whole row at 70 columns (`-70col`) is claimed.
#[test]
fn a_cut_row_is_never_claimed() {
    // Every width the tap's words do not fit, as Claude cuts them (the lab stand-in's
    // `status_row`: `cols − 5` cells of the words, then `…`): S5b's parser read the cuts at 19
    // and 20 columns (`week 63% used …`, `week 63% used ·…`) as the bare form.
    for cols in 5..WORDS.chars().count() + 4 {
        let kept: String = WORDS.chars().take(cols - 5).collect();
        let row = format!("  {kept}\u{2026}");
        assert_eq!(row_span(&row, cols), None, "{cols} columns: {row:?}");
    }
    let view = case("b");
    let (app, wid, _, rows) = claimed("claude-2.1.318-tap-row-week-30col", &view);
    let tap = bottom(&rows) + 1;
    assert!(rows[tap].ends_with('\u{2026}'), "{:?}", rows[tap]);
    assert_eq!(row_span(&rows[tap], 30), None);
    assert!(row_hits(&app, wid).is_empty());
    assert_eq!(glass(&app, wid, tap).trim_end(), rows[tap]);
    let (app, wid, _, _) = claimed("claude-2.1.318-tap-row-week-70col", &view);
    assert_eq!(
        row_hits(&app, wid).len(),
        3,
        "CONTROL: 70 columns are claimed"
    );
    assert!(!line_d_unit(&app, wid));
}

/// LINE D CARRIES THE UNIT TEN SECONDS AFTER LINE E IS LOST (§5.2): a pane whose line E is not
/// claimed (the reading says the row is an owner's) shows no unit on line D from its first frame
/// until ten seconds after it, then the words-only unit; a claim hides it at once, and a row
/// lost again, after a long rest, gives it back ten seconds after the first frame that missed
/// the claim, never at once. CONTROLS: at nine seconds the unit is not yet there, each time.
#[test]
fn line_d_carries_the_unit_ten_seconds_after_line_e_is_lost() {
    time_the_grace();
    assert_eq!(GRACE, Duration::from_secs(10));
    let view = case("b");
    let (cols, rows) = screen("claude-2.1.318-tap-row-week");
    let (mut app, wid, session) = pane(cols, &rows);
    probe::publish(&app, session, facts_with(Some(view.clone()), false));
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
    frame_at(&mut app, wid, t0);
    assert!(!line_d_unit(&app, wid), "the first frame");
    frame_at(&mut app, wid, t0 + Duration::from_secs(9));
    assert!(
        !line_d_unit(&app, wid),
        "CONTROL: at nine seconds the unit is not yet there"
    );
    frame_at(&mut app, wid, t0 + GRACE);
    assert!(line_d_unit(&app, wid), "ten seconds on");
    probe::publish(&app, session, facts_with(Some(view), true));
    let last = t0 + GRACE + Duration::from_secs(1);
    frame_at(&mut app, wid, last);
    assert_eq!(row_hits(&app, wid).len(), 3);
    assert!(!line_d_unit(&app, wid), "a claim hides it at once");
    probe::bytes(
        &app,
        session,
        format!("\x1b7\x1b[{};1H\x1b[2K  my own row\x1b8", bottom(&rows) + 2).as_bytes(),
    );
    // The row is lost after a minute at rest: no frame between.
    let lost = last + Duration::from_secs(60);
    frame_at(&mut app, wid, lost);
    assert!(row_hits(&app, wid).is_empty(), "lost");
    assert!(!line_d_unit(&app, wid), "the grace starts at the loss");
    frame_at(&mut app, wid, lost + Duration::from_secs(9));
    assert!(
        !line_d_unit(&app, wid),
        "CONTROL: nine seconds after the loss"
    );
    frame_at(&mut app, wid, lost + GRACE);
    assert!(line_d_unit(&app, wid), "ten seconds after the loss");
}

/// THE GRACE IS THE SESSION'S, NOT THE LEAF'S (§5.2): a pane that never claims line E (the
/// reading says the row is an owner's, as for any session the tap does not draw) shows line D's
/// unit once its grace has lapsed, and keeps it on the first frame after its tab is shown again,
/// though its leaf's footer was another session's in between (as first written the grace lived
/// in that footer, and line D dropped the unit for ten seconds after every tab switch).
/// NEGATIVE CONTROL: a session at its own first frame (the second tab, on the same screen and
/// facts) shows no unit, which is what a re-shown pane would show with its grace started anew.
#[test]
fn the_grace_is_the_sessions_not_the_leafs() {
    time_the_grace();
    let view = case("b");
    let (cols, rows) = screen("claude-2.1.318-tap-row-week");
    let (mut app, wid, first) = pane(cols, &rows);
    probe::publish(&app, first, facts_with(Some(view.clone()), false));
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
    let at = |s: u64| t0 + Duration::from_secs(s);
    frame_at(&mut app, wid, t0);
    assert!(!line_d_unit(&app, wid), "the first frame");
    frame_at(&mut app, wid, t0 + GRACE);
    assert!(line_d_unit(&app, wid), "the grace has lapsed");
    let second = app.next_session_id;
    app.push_stub_tab(wid, crate::stub_session(second));
    assert_eq!(app.focused_session_id(wid), Some(second));
    probe::feed(&app, second, &rows);
    probe::publish(&app, second, facts_with(Some(view), false));
    frame_at(&mut app, wid, at(11));
    assert!(
        !line_d_unit(&app, wid),
        "NEGATIVE CONTROL: the second tab's first frame"
    );
    app.switch_tab_in(wid, 0);
    assert_eq!(app.focused_session_id(wid), Some(first));
    frame_at(&mut app, wid, at(12));
    assert!(
        line_d_unit(&app, wid),
        "the first frame back keeps the unit"
    );
    app.switch_tab_in(wid, 1);
    frame_at(&mut app, wid, at(13));
    assert!(
        !line_d_unit(&app, wid),
        "the second tab's grace runs on from its own first frame"
    );
    frame_at(&mut app, wid, at(11) + GRACE);
    assert!(line_d_unit(&app, wid), "and lapses ten seconds after it");
}

/// THE QUOTA ROW READS AND LIGHTS AS ONE (§1.3, §2.6): at rest each cell of case (b) wears its
/// role's token at its column's place in the field (the lit ink, the values', the quiet ink, the
/// measured levels the marks' hue, the projected line the letters', the warnings warn), bold
/// where the line says; lit, every glyph but the warnings and the projected line lifts one step,
/// and every glyph of the lead and the tail is underlined in its own ink, but none of the
/// chart's: an underline is drawn whatever the glyph, and under a level-2-to-8 line, a `╳` or
/// `▕` it reads as a second line at level 1 (as S5 first lit it). NEGATIVE CONTROL: the measured
/// levels' ink is not the projected line's.
#[test]
fn the_quota_row_reads_and_lights_as_one() {
    let view = case("b");
    let line = laid(&view, 166);
    let cols = 170;
    let blank = RenderCell {
        ch: ' ',
        fg: [200, 200, 200],
        ..RenderCell::default()
    };
    let glass = GlassInks::of(Some(CardPalette::Cruise), 0, 0x00c8_c8c8, 0x003c_3c3c);
    let key = |lit: bool| QuotaKey {
        laid: (1, 166),
        cells: 166,
        cols,
        glass,
        blank,
        lit,
        cell: (8, 16),
    };
    let (mut rest, mut lit) = (Vec::new(), Vec::new());
    compose_cells(&line, &key(false), &mut rest);
    compose_cells(&line, &key(true), &mut lit);
    assert_eq!(rest.len(), 166);
    for (i, &(ch, role, bold)) in line.cells.iter().enumerate() {
        let t = field(INDENT + i, cols);
        let want = match role {
            Role::Lit => glass.lit(),
            Role::Value => glass.value(),
            Role::Quiet => glass.quiet(),
            Role::Mark => glass.mark(t),
            Role::Line => glass.letter(t),
            Role::Warn => glass.warn(),
            Role::Track => glass.track(),
        };
        assert_eq!(
            (rest[i].ch, rest[i].fg, rest[i].bold),
            (ch, rgb_of(want), bold),
            "cell {i} {role:?}"
        );
        assert_eq!(rest[i].underline, UnderlineStyle::None, "cell {i}");
        if line.chart.contains(&i) {
            assert_eq!(
                lit[i].underline,
                UnderlineStyle::None,
                "chart cell {i} {ch:?}"
            );
            assert_eq!(lit[i].underline_color, None, "chart cell {i}");
        } else if ch != ' ' {
            assert_eq!(lit[i].underline, UnderlineStyle::Single, "cell {i}");
            assert_eq!(lit[i].underline_color, Some(lit[i].fg), "cell {i}");
        }
        if ch != ' ' {
            let lifts = !matches!(role, Role::Warn | Role::Line);
            assert_eq!(lit[i].fg != rest[i].fg, lifts, "cell {i} {ch:?} {role:?}");
        }
    }
    assert!(
        line.chart
            .clone()
            .any(|i| line.cells[i].1 == Role::Mark && lit[i].fg != rest[i].fg),
        "lit, the chart's measured levels lift"
    );
    assert!(rest[line.cells.len()..].iter().all(|c| c.ch == ' '));
    let ink_of = |role: Role| {
        line.cells
            .iter()
            .position(|c| c.1 == role)
            .map(|i| rest[i].fg)
            .unwrap_or_else(|| panic!("{role:?} in case (b)"))
    };
    assert_ne!(
        ink_of(Role::Mark),
        ink_of(Role::Line),
        "NEGATIVE CONTROL: the measured and the projected differ"
    );
}

/// THE QUOTA ROW READS, LIGHTS AND OPENS THE ACCOUNTS CARD (§2.6): the pointer on any part of
/// the row lights the account, and with it every part of the row (the lead and the tail
/// underlined, the chart's inks lifted), and the lead reads the account's title; a click on it
/// opens the Accounts card through the account's own route. CONTROL: at rest nothing on the row
/// is underlined, and the chart wears its resting inks.
#[test]
fn the_quota_row_reads_lights_and_opens_the_accounts_card() {
    let view = case("b");
    let (mut app, wid, session, _) = claimed("claude-2.1.318-tap-row-week", &view);
    let hits = row_hits(&app, wid);
    assert_eq!(hits.len(), 3);
    let underlined = |app: &App, hit: &FactHit| {
        app.windows[&wid].input_scratch.cells[hit.frame_row][hit.cols.0..hit.cols.1]
            .iter()
            .any(|c| c.underline == UnderlineStyle::Single)
    };
    let inks = |app: &App, hit: &FactHit| -> Vec<[u8; 3]> {
        app.windows[&wid].input_scratch.cells[hit.frame_row][hit.cols.0..hit.cols.1]
            .iter()
            .map(|c| c.fg)
            .collect()
    };
    let chart = |h: &&FactHit| matches!(h.part, Some(RowPart::Chart(_)));
    let resting = inks(&app, hits.iter().find(chart).expect("the chart's hit"));
    assert!(
        !hits.iter().any(|h| underlined(&app, h)),
        "CONTROL: nothing lit at rest"
    );
    for hit in &hits {
        hover(&mut app, wid, (hit.frame_row, hit.cols.0));
        assert_eq!(
            app.windows[&wid].claude_lights.lit_fact(session),
            Some(FactKind::Account),
            "{:?}",
            hit.part
        );
        for h in &hits {
            if chart(&h) {
                assert!(
                    !underlined(&app, h),
                    "{:?}: the chart is not underlined",
                    hit.part
                );
                assert_ne!(inks(&app, h), resting, "{:?}: the chart lifts", hit.part);
            } else {
                assert!(underlined(&app, h), "{:?}: the whole row lights", hit.part);
            }
        }
    }
    hover(&mut app, wid, (hits[0].frame_row, hits[0].cols.0));
    let title = app.fact_title(session, Fact::Account).0;
    assert_eq!(
        app.windows[&wid]
            .claude_lights
            .fact_reading(session, Fact::Account),
        Some(title.as_str()),
        "the lead reads the account's title"
    );
    app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
    app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
    let menu = app.autopilot_menu.as_ref().expect("the card opened");
    assert_eq!(
        menu.owner,
        crate::autopilot_menu::MenuOwner::Fact(Fact::Account)
    );
}

/// HOVERING A CHART COLUMN NAMES ITS INSTANT (§2.6): the pointer on a column of the row's chart
/// reads that column's title on this Mac's clock ([`QuotaView::column_title`]), another column
/// another title, and back on the lead the row's own title. CONTROL: the now cell, which holds no
/// time, reads the row's title.
#[test]
fn hovering_a_chart_column_names_its_instant() {
    let view = case("b");
    let (mut app, wid, session, _) = claimed("claude-2.1.318-tap-row-week", &view);
    let hits = row_hits(&app, wid);
    let chart = hits
        .iter()
        .find(|h| matches!(h.part, Some(RowPart::Chart(_))))
        .copied()
        .expect("the chart's hit");
    let Some(RowPart::Chart(per_day)) = chart.part else {
        unreachable!("found as a chart")
    };
    let offsets =
        Offsets::read_around(view.chart.now, crate::presence::local_offset_at).expect("a clock");
    let row_title = app.fact_title(session, Fact::Account).0;
    let read = |app: &mut App, cell: usize| -> String {
        hover(app, wid, (chart.frame_row, chart.cols.0 + cell));
        assert_eq!(
            app.windows[&wid].claude_lights.quota_column,
            Some((session, per_day, cell))
        );
        app.windows[&wid]
            .claude_lights
            .fact_reading(session, Fact::Account)
            .expect("a reading")
            .to_owned()
    };
    let first = read(&mut app, 0);
    assert_eq!(
        Some(&first),
        view.column_title(per_day, 0, &offsets).as_ref()
    );
    assert!(first.contains("(read on this Mac)"), "{first}");
    let k = usize::from(per_day);
    let ahead = read(&mut app, k + 2);
    assert_eq!(
        Some(&ahead),
        view.column_title(per_day, k + 2, &offsets).as_ref()
    );
    assert_ne!(ahead, first);
    assert_eq!(read(&mut app, k), row_title, "CONTROL: the now cell");
    hover(&mut app, wid, (hits[0].frame_row, hits[0].cols.0));
    assert_eq!(app.windows[&wid].claude_lights.quota_column, None);
    assert_eq!(
        app.windows[&wid]
            .claude_lights
            .fact_reading(session, Fact::Account),
        Some(row_title.as_str())
    );
}

#[test]
fn leaving_or_unclaiming_the_quota_chart_clears_its_reading_and_cursor() {
    let view = case("b");
    let (mut app, wid, session, _) = claimed("claude-2.1.318-tap-row-week", &view);
    let chart = row_hits(&app, wid)
        .into_iter()
        .find(|h| matches!(h.part, Some(RowPart::Chart(_))))
        .expect("the chart's hit");
    let Some(RowPart::Chart(per_day)) = chart.part else {
        unreachable!("found as a chart")
    };
    hover(&mut app, wid, (chart.frame_row, chart.cols.0));
    assert_eq!(
        app.windows[&wid].claude_lights.quota_column,
        Some((session, per_day, 0))
    );
    assert!(
        app.windows[&wid]
            .claude_lights
            .fact_reading(session, Fact::Account)
            .is_some()
    );

    let (blank_x, blank_y) = px_of(&app, wid, (0, 0));
    app.on_cursor_moved(wid, blank_x, blank_y);
    assert_eq!(app.windows[&wid].claude_lights.lit_fact(session), None);
    assert_eq!(app.windows[&wid].claude_lights.quota_column, None);
    assert_eq!(
        app.windows[&wid]
            .claude_lights
            .fact_reading(session, Fact::Account),
        None
    );

    let (chart_x, chart_y) = px_of(&app, wid, (chart.frame_row, chart.cols.0 + 1));
    app.on_cursor_moved(wid, chart_x, chart_y);
    assert_eq!(
        app.windows[&wid].claude_lights.quota_column,
        Some((session, per_day, 1))
    );
    assert!(app.windows[&wid].hover_pointer);
    probe::publish(&app, session, facts_with(Some(view), false));
    frame(&mut app, wid);
    app.refresh_glass_cursor_for_frame(wid);
    assert_eq!(app.windows[&wid].claude_lights.lit_fact(session), None);
    assert_eq!(app.windows[&wid].claude_lights.quota_column, None);
    assert_eq!(
        app.windows[&wid]
            .claude_lights
            .fact_reading(session, Fact::Account),
        None
    );
    assert!(!app.windows[&wid].hover_pointer);
    assert!(app.windows[&wid].native_text_cursor);
}

/// THE QUOTA ROW COSTS NOTHING AT REST (as `claude_footer_cost_tests`): once a claimed row is
/// composed, a pass over the unchanged pane allocates nothing, composes nothing and reads no
/// clock (as first written, every claimed frame read one), and so does one whose recovery is
/// paused (Autopilot Off), whose observer is busy, or whose account is walled with no recovery
/// observed: the view's choice reads the progress's fields (as first written it built the
/// reconciled progress, a clone of the decision, twice a frame in each of those states).
/// NEGATIVE CONTROL: the pointer on the row composes it again, once.
#[test]
fn the_quota_row_costs_nothing_at_rest() {
    use aterm_agent::harness::account_progress::{
        AccountProgress, ProgressPhase, SESSION_AUTOPILOT_OFF_FINDING,
    };
    use aterm_agent::harness::session_usage::{UsageFacts, Wall};
    let tap_row = format!("  {WORDS}");
    let under = [tap_row.as_str(), probe::BYPASS];
    let live = probe::Live {
        status: probe::DONE,
        draft: &[""],
        under: &under,
    };
    let mut paused = AccountProgress::unknown(100);
    paused.phase = ProgressPhase::Paused;
    paused.details.push(SESSION_AUTOPILOT_OFF_FINDING.into());
    let watching = AccountProgress {
        phase: ProgressPhase::Watching,
        ..AccountProgress::unknown(100)
    };
    let wall = || {
        Some(UsageFacts {
            wall: Some(Wall {
                label: "7d",
                resets: "Sat 10:00".into(),
            }),
            ..probe::usage(48_000_000).expect("usage")
        })
    };
    let states = [
        ("at rest", facts_with(Some(case("c")), true)),
        (
            "paused",
            FooterFacts {
                account_progress: Some(paused),
                ..facts_with(Some(case("c")), true)
            },
        ),
        (
            "busy",
            FooterFacts {
                account_progress: Some(watching.clone()),
                account_progress_busy: true,
                ..facts_with(Some(case("c")), true)
            },
        ),
        (
            "walled, unobserved",
            FooterFacts {
                usage: wall(),
                ..facts_with(Some(case("c")), true)
            },
        ),
        (
            "walled, watching",
            FooterFacts {
                usage: wall(),
                account_progress: Some(watching),
                ..facts_with(Some(case("c")), true)
            },
        ),
    ];
    for (state, facts) in states {
        assert!(
            state == "at rest" || facts.account_progress_view().is_some(),
            "{state}: the state has a progress view"
        );
        let (mut app, wid, _) = probe::glass_with(&live, facts);
        assert_eq!(row_hits(&app, wid).len(), 3, "{state}: claimed");
        let (plan, route) = app.active_visible_frame_layout(wid).expect("a layout");
        for _ in 0..2 {
            probe::refill(&mut app, wid);
            let _ = probe::pass_on(&mut app, wid, &plan, route);
        }
        let _ = crate::work_counts::take_footer_rules_composed();
        probe::refill(&mut app, wid);
        let (_, allocations) =
            crate::work_counts::allocations_in(|| probe::pass_on(&mut app, wid, &plan, route));
        assert_eq!(allocations, 0, "{state}: nothing allocates at rest");
        assert_eq!(
            crate::work_counts::take_footer_rules_composed(),
            0,
            "{state}"
        );
        assert_eq!(row_hits(&app, wid).len(), 3, "{state}: still claimed");
    }
    let (mut app, wid, _) = probe::glass_with(&live, facts_with(Some(case("c")), true));
    let (plan, route) = app.active_visible_frame_layout(wid).expect("a layout");
    for _ in 0..2 {
        probe::refill(&mut app, wid);
        let _ = probe::pass_on(&mut app, wid, &plan, route);
    }
    let _ = crate::work_counts::take_footer_rules_composed();
    // Nor reads a clock: the grace reads one only on a frame that misses the claim.
    let reads = std::cell::Cell::new(0);
    let clock = || {
        reads.set(reads.get() + 1);
        footer::ReadClock {
            read_at: SystemTime::now(),
            now: 0,
            offset_at: &super::super::offset_at_cached,
        }
    };
    probe::refill(&mut app, wid);
    app.splice_claude_footer_at(wid, &plan, route, &clock);
    assert_eq!(reads.get(), 0, "a claimed row at rest reads no clock");
    assert_eq!(row_hits(&app, wid).len(), 3);
    assert_eq!(crate::work_counts::take_footer_rules_composed(), 0);
    let hit = row_hits(&app, wid)[0];
    let (x, y) = px_of(&app, wid, (hit.frame_row, hit.cols.0));
    app.on_cursor_moved(wid, x, y);
    probe::refill(&mut app, wid);
    let _ = probe::pass_on(&mut app, wid, &plan, route);
    assert!(
        crate::work_counts::take_footer_rules_composed() >= 1,
        "NEGATIVE CONTROL: lit, the row is composed again"
    );
}

/// A READER HEARS THE ROW IN WORDS (§2.6): the row is one chip over its three parts, named by
/// the view's spoken words, which hold no box, block, legacy-computing or braille glyph.
#[cfg(a11y_tree)]
#[test]
fn a_reader_hears_the_row_in_words() {
    let view = case("c");
    let (app, wid, session, _) = claimed("claude-2.1.318-tap-row-week", &view);
    let hits = row_hits(&app, wid);
    let at = hits[0].frame_row;
    let chips: Vec<_> = app
        .claude_chips_a11y(wid)
        .into_iter()
        .filter(|c| {
            c.session == session && c.chip == crate::a11y_tree::HarnessChip::Account && c.row == at
        })
        .collect();
    assert_eq!(chips.len(), 1, "{chips:?}");
    let chip = &chips[0];
    assert_eq!(chip.label, view.spoken);
    assert_eq!(chip.col, hits[0].cols.0);
    assert_eq!(
        chip.col + chip.width,
        hits.iter().map(|h| h.cols.1).max().expect("hits")
    );
    assert!(
        !chip.label.chars().any(|c| matches!(
            u32::from(c),
            0x2500..=0x259F | 0x2800..=0x28FF | 0x1FB00..=0x1FBFF
        )),
        "{}",
        chip.label
    );
}

/// NO MIDNIGHT STRIKES A LETTER (§1.3, §2.2): over cases (a) to (d), each laid in the rooms of
/// 170, 100 and 70 columns, a local midnight's tick stands only in a chart cell that draws a
/// level, `┄` or nothing, at its midnight's exact place, at the foot of the cell and a quarter of
/// it tall, so only `▕` stands a whole cell high and no tick meets a `▔` to read as `T`; one
/// on a glyph that inks its foot quarter (a measured step, the projected line at level 1 `▁` or
/// 2 `🭻`, which cases (a) and (b) draw after their reset) takes the ground's ink, a notch
/// through it, and every other the track's ([`super::TickInks`]). NEGATIVE CONTROL (the ink):
/// the track on every tick, or the ground on the measured steps alone, strikes the low line.
/// NEGATIVE CONTROL: case (c) at 70 columns has midnights in a hand-off's name or mark (`→main`,
/// `→alt1`), which a full-height hairline at every midnight (as S5 first drew them) struck
/// through.
#[test]
fn no_midnight_strikes_a_letter() {
    use aterm_agent::harness::quota_view::CELL_PLACES;
    use aterm_core::render::{FxItem, FxRect};
    let (cw, ch) = (8u16, 16u16);
    let (mut struck, mut ticks, mut crossed) = (0, 0, 0);
    for (name, view) in quota_scenes::cases() {
        for room in [166, 96, 66] {
            let mut line = QuotaLine::default();
            if !view.lay(room, &mut line) {
                continue;
            }
            let placed: Vec<(usize, i64)> = view.chart.midnights_at(line.per_day).collect();
            let over = |cell: usize| {
                let (glyph, role, _) = line.cells[line.chart.start + cell];
                glyph == ' ' || matches!(role, Role::Mark | Role::Line | Role::Track)
            };
            let kept: Vec<i64> = placed
                .iter()
                .filter(|&&(cell, _)| over(cell))
                .map(|&(_, place)| place * i64::from(cw) / CELL_PLACES)
                .collect();
            if (name, room) == ("c", 66) {
                struck += placed.len() - kept.len();
            }
            let mut laid = Vec::new();
            let chart = &line.cells[line.chart.clone()];
            super::push_ticks(
                &mut laid,
                &line.ticks,
                (0, line.chart.len()),
                (cw, ch),
                |i| {
                    chart
                        .get(i)
                        .map_or(super::TickOn::Ground, |c| super::TickOn::of(c.0, false))
                },
            );
            let inks = super::TickInks::new(0x0088_8888, 0x00F8_FAFD, None);
            let rects: Vec<FxRect> =
                super::tick_content(&laid, inks).map_or_else(Vec::new, |content| {
                    content
                        .items()
                        .iter()
                        .filter_map(|item| match item {
                            FxItem::Rect(r) => Some(*r),
                            _ => None,
                        })
                        .collect()
                });
            assert_eq!(
                rects.iter().map(|r| i64::from(r.x)).collect::<Vec<_>>(),
                kept,
                "({name}) in {room}: {}",
                line.text()
            );
            for r in &rects {
                let cell = usize::try_from(r.x).expect("a place") / usize::from(cw);
                let (glyph, role, _) = line.cells[line.chart.start + cell];
                assert!(
                    glyph == ' ' || matches!(role, Role::Mark | Role::Line | Role::Track),
                    "({name}) in {room}: a tick over {glyph:?}"
                );
                assert_eq!((r.w, r.h, r.y), (1, ch / 4, i32::from(ch - ch / 4)));
                let low = role == Role::Line && ['\u{2581}', '\u{1fb7b}'].contains(&glyph);
                let on = if role == Role::Mark || low {
                    super::TickOn::Fill
                } else {
                    super::TickOn::Ground
                };
                assert_eq!(
                    r.ink,
                    on.ink(),
                    "({name}) in {room}: the ground's ink over {glyph:?}, else the track's"
                );
                crossed += usize::from(low);
                ticks += 1;
            }
        }
    }
    assert!(ticks > 0, "the cases have midnights to tick");
    assert!(
        crossed > 0,
        "the cases have midnights on a low projected line"
    );
    assert!(
        struck > 0,
        "NEGATIVE CONTROL: case (c) at 70 columns has a midnight in a letter's cell"
    );
}

/// THE KITTY KEEPS OFF THE PAINTED CELLS: the resident pet is told the row's own cells are
/// painted glass and the tap's words past them blank glass, so it never takes Claude's hidden
/// letters there for ink. NEGATIVE CONTROL (the write held whole as painted, as every write
/// before line E was): the blanked cells would read as chrome.
#[test]
fn the_kitty_keeps_off_the_painted_cells() {
    use aterm_effects::pet_world::PetRect;
    let view = case("b");
    let words = WORDS.chars().count();
    let line = laid(&view, words);
    assert!(
        line.cells.len() < words,
        "the rung is narrower than the words: {}",
        line.text()
    );
    let (mut app, wid, _, rows) =
        claimed("claude-2.1.318-tap-row-week-credits-manual-60col", &view);
    app.splice_autopilot_menu(wid);
    let tap = bottom(&rows) + 1;
    let chrome = &app.windows[&wid].pet_chrome;
    let within = |rects: &[PetRect], col: usize| {
        let (r, c) = (tap as f32 + 0.5, col as f32 + 0.5);
        rects.iter().any(|rect| {
            rect.row <= r && r < rect.row + rect.rows && rect.col <= c && c < rect.col + rect.cols
        })
    };
    for col in INDENT..INDENT + words {
        let painted = col < INDENT + line.cells.len();
        assert_eq!(within(chrome.held(), col), painted, "col {col}: painted");
        assert_eq!(
            within(chrome.held_blanked(), col),
            !painted,
            "col {col}: blanked"
        );
    }
}

/// THE QUOTA ROW AS A PERSON SEES IT (§7): cases (a) to (d) at 170, 100 and 70 columns, under
/// Cruise, Land, Autopilot Off and no harness (no palette), on Aterm Day and Aterm Night, through
/// the capture route `aterm ctl image` takes: each frame whole, and its last six rows twice their
/// size, written to `ATERM_QUOTA_ROW_GALLERY` (a scratch folder where unset).
#[test]
fn quota_row_gallery() {
    use aterm_agent::supervise::Autopilot;
    let (dir, _gallery) =
        crate::scripted_redraw_fixture::gallery_directory("ATERM_QUOTA_ROW_GALLERY");
    let tap_row = format!("  {WORDS}");
    let under = [tap_row.as_str(), probe::BYPASS];
    let live = probe::Live {
        status: probe::DONE,
        draft: &[""],
        under: &under,
    };
    for theme in ["Aterm Day", "Aterm Night"] {
        for palette in ["cruise", "land", "off", "none"] {
            for cols in [170u16, 100, 70] {
                for (name, view) in quota_scenes::cases() {
                    let rows = probe::screen_at(&live, ROWS as u16, cols);
                    let (mut app, wid, session) = pane(cols, &rows);
                    app.set_session_theme(
                        session,
                        Some(crate::session_theme::SessionTheme {
                            selector: theme.into(),
                            origin: crate::session_theme::SessionThemeOrigin::Ctl,
                        }),
                    );
                    let ctx = app.pool.get(session).expect("session").ctx.clone();
                    {
                        let mut timeline = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
                        timeline.note_foreground_group(4242);
                        timeline.set_program(4242, Some("claude".into()));
                        timeline.set_agent_reader_for_test(true);
                        assert!(timeline.set_claude_footer(
                            4242,
                            None,
                            Some(facts_with(Some(view), true))
                        ));
                    }
                    let _host = (palette != "none").then(|| {
                        let host = probe::hosted(&mut app);
                        crate::session_timeline::claim_supervisor(
                            &ctx,
                            &crate::harness_host::holder(),
                            None,
                            None,
                            crate::metrics::now_us(),
                        )
                        .expect("the claim");
                        host
                    });
                    let word = match palette {
                        "land" => Some(Autopilot::Land),
                        "off" => Some(Autopilot::Off),
                        _ => None,
                    };
                    if let Some(word) = word {
                        assert!(app.set_session_autopilot(
                            session,
                            Some(word),
                            crate::autopilot::Surface::Chip
                        ));
                    }
                    probe::feed(&app, session, &rows);
                    frame(&mut app, wid);
                    frame(&mut app, wid);
                    let what = format!("{theme} {palette} {cols} ({name})");
                    assert!(!row_hits(&app, wid).is_empty(), "{what}: claimed");
                    let png = capture(&mut app, wid);
                    let stem = format!(
                        "{}-{palette}-{cols}-{name}",
                        theme.to_lowercase().replace(' ', "-")
                    );
                    std::fs::write(dir.join(format!("{stem}.png")), &png).expect("the PNG");
                    let (rgba, w, h) = aterm_render::decode_png_rgba8(&png).expect("a PNG");
                    let y0 = h - h * 6 / ROWS;
                    let (cw, ch) = (2 * w, 2 * (h - y0));
                    let mut crop = Vec::with_capacity(cw * ch * 4);
                    for y in 0..ch {
                        for x in 0..cw {
                            let at = ((y0 + y / 2) * w + x / 2) * 4;
                            crop.extend_from_slice(&rgba[at..at + 4]);
                        }
                    }
                    let png = crate::app_introspect::encode_rgba8_png(
                        &crop,
                        u32::try_from(cw).expect("a width"),
                        u32::try_from(ch).expect("a height"),
                    )
                    .expect("the crop encodes");
                    std::fs::write(dir.join(format!("{stem}-rows-2x.png")), png).expect("the PNG");
                }
            }
        }
    }
}

/// The PNG the capture route renders of window `wid` (`aterm ctl image`).
fn capture(app: &mut App, wid: WindowId) -> Vec<u8> {
    app.redraw_window(wid);
    crate::scripted_redraw_fixture::headless(app, |app| {
        let (reply, rx) = std::sync::mpsc::channel();
        app.render_image(crate::control::ImageReq {
            handoff: crate::control::ReplyRetentionPermit::from_own_budget(),
            target: crate::control_auth::ConfinedImage::in_memory(),
            clean: false,
            session: None,
            want_bytes: true,
            want_metadata: false,
            frame_metadata: std::sync::Arc::new(std::sync::OnceLock::new()),
            cancel: crate::control::CaptureCancellation::new(),
            reply,
        });
        rx.recv_timeout(Duration::from_secs(60))
            .expect("image reply")
            .expect("the image")
            .value
            .2
            .expect("PNG bytes")
    })
}
