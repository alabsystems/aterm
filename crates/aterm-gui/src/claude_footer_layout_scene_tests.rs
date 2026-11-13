// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_agent::harness::quota_view::QuotaLine;

/// Also the glass design's line D and line E (§1.2, §1.3): line D gives up its account unit
/// while line E is claimed, and carries it, the case's words, while not. Negative control:
/// noting every sample's line E as unclaimed (`note_line_e(false)`) puts `⎆` back on a
/// claimed sample's line D, and the test fails.
///
/// A frame that declares no terminal band (`grid_bot_row` 0, as a windowless capture lays
/// it) is filled as a windowed one is, so `lab instance --scene harness-layouts --image`
/// shows the samples headless too. NEGATIVE CONTROL (run once, restored): bounding the
/// samples by `grid_bot_row` alone, as the scene did, leaves that frame unpainted and fails.
/// Each quota sample wears Cruise's chip (`∞ yolo`) under Cruise's rainbow, as §1.2 draws
/// it, and the worded-chips sample Land's under Land's sunset, so no chip contradicts its
/// palette. NEGATIVE CONTROL: composing the Land sample with Cruise's glass leaves its words
/// intact but gives `land` Cruise's letter ink, and fails the exact palette assertion.
#[test]
fn the_private_scene_draws_whole_products_and_actionable_rings_at_actual_cell_size() {
    for ((fg, bg), declared) in [("d0d0d0", "111318"), ("202020", "ffffff")]
        .into_iter()
        .flat_map(|ground| [(ground, true), (ground, false)])
    {
        let mut term = aterm_core::terminal::Terminal::new(40, 250);
        term.process(format!("\x1b]10;#{fg}\x07\x1b]11;#{bg}\x07x").as_bytes());
        let mut input = term.cell_frame(40, 250);
        input.grid_bot_row = if declared { input.rows } else { 0 };
        let original = input.clone();
        paint(&mut input, (0, 14));
        assert_eq!(input, original, "invalid metrics do not replace the frame");
        paint(&mut input, (7, 14));
        let row = |offset: usize| read_glass(&input.cells[input.grid_top_row + offset]);
        assert!(row(2).contains(PRODUCT));
        assert!(row(5).contains(PRODUCT));
        assert!(row(8).contains("1 shell"));
        assert!(row(8).contains(UP_ARROW), "the actionable colour arrow");
        for offset in [11, 17] {
            assert!(row(offset).contains('↑'), "a held move's quiet arrow");
            assert!(!row(offset).contains(UP_ARROW), "held is not actionable");
        }
        assert!(row(14).contains(&"7".repeat(CLI_VERSION_MAX - "0.1.0-dev.".len())));
        assert!(
            row(20).contains("updating to") || row(20).contains('→'),
            "asked forms keep the verb or under-way marker"
        );
        assert!(
            row(23).contains("land"),
            "the sample uses a worded Land chip"
        );
        assert!(row(23).contains("yolo"), "its permission chip follows it");
        assert!(
            row(23).contains(" ─ "),
            "worded chips are set apart by the rule"
        );
        let word_col = |offset: usize, word: &str| {
            input.cells[input.grid_top_row + offset]
                .windows(word.chars().count())
                .position(|cells| cells.iter().map(|cell| cell.ch).eq(word.chars()))
                .unwrap_or_else(|| panic!("{word:?} in scene row {offset}"))
        };
        let blank = RenderCell {
            ch: ' ',
            ..input.implicit_blank
        };
        let rule = RenderCell {
            ch: '\u{2500}',
            fg: mix(blank.fg, blank.bg, 0.6),
            ..blank
        };
        let palette_ink = |palette, col| {
            let (glass, _) = scene_glass(palette, input.cols, rule, blank);
            rgb_of(glass.letter(aterm_effects::glass_inks::field(col, input.cols)))
        };
        let land_col = word_col(23, "land");
        let land_chip_col = word_col(23, AutopilotView::Land.glyph());
        assert_eq!(
            input.cells[input.grid_top_row + 23][land_col].fg,
            palette_ink(CardPalette::Land, land_chip_col),
            "the worded Land chip wears Land's sunset"
        );
        assert_ne!(
            palette_ink(CardPalette::Land, land_chip_col),
            palette_ink(CardPalette::Cruise, land_chip_col),
            "CONTROL: the palettes differ at the asserted cell"
        );
        assert!(
            row(26).contains("Opus 5.5") && row(26).contains("medium"),
            "the parsed hint fills the card's open effort on the composed rule: {:?}",
            row(26)
        );
        assert_eq!(
            row(27).trim_end(),
            "1 shell",
            "effort filled from the admitted hint leaves only jobs"
        );
        for d in [30, 34, 38] {
            assert!(
                row(d).contains("\u{221e} yolo") && !row(d).contains("land"),
                "a quota sample wears Cruise's chip: {:?}",
                row(d)
            );
            let cruise_chip_col = word_col(d, AutopilotView::Cruise.glyph());
            assert_eq!(
                input.cells[input.grid_top_row + d][cruise_chip_col].fg,
                palette_ink(CardPalette::Cruise, cruise_chip_col),
                "the quota sample's Cruise chip wears Cruise's rainbow"
            );
        }
        for d in [30, 34] {
            assert!(row(d).contains("Opus 5.5"), "line D's facts: {:?}", row(d));
            assert!(
                !row(d).contains('⎆'),
                "line D gives up its account unit while line E is claimed: {:?}",
                row(d)
            );
        }
        for e in [31, 35] {
            assert!(row(e).contains('▕'), "line E's chart: {:?}", row(e));
            assert!(row(e).contains('↺'), "the reset on the chart: {:?}", row(e));
            assert!(
                !row(e).contains("resets"),
                "the tap's words are covered: {:?}",
                row(e)
            );
        }
        assert!(
            row(31).contains("main 37% left"),
            "case (b)'s figure: {:?}",
            row(31)
        );
        assert!(row(31).contains('╳'), "case (b) runs out: {:?}", row(31));
        assert!(
            row(35).contains("main 25% left"),
            "case (c)'s figure: {:?}",
            row(35)
        );
        assert!(
            row(35).contains('→'),
            "case (c)'s work moves: {:?}",
            row(35)
        );
        assert!(
            row(38).contains("⎆ main 37% left") && row(38).contains("↺ Sat 10:00"),
            "unclaimed, line D carries the words-only unit: {:?}",
            row(38)
        );
        assert!(
            row(39).trim().is_empty(),
            "an unclaimed sample has no line E"
        );
        for e in [31, 35] {
            assert!(
                input
                    .fx_layers
                    .iter()
                    .any(|l| l.offset.1 == (input.grid_top_row + e) as i32 * 14),
                "line E's midnights are hairlines on its row"
            );
        }
        let ring = input
            .fx_layers
            .iter()
            .find(|l| l.id == top_ring_id(3))
            .expect("the actionable ready arrow is ringed");
        assert_eq!(ring.offset.1, (input.grid_top_row + 8) as i32 * 14);
        for i in [4, 6] {
            assert!(
                !input.fx_layers.iter().any(|l| l.id == top_ring_id(i)),
                "a held arrow has no hover ring"
            );
        }
        assert!(
            !input.fx_layers.iter().any(|l| l.id == top_ring_id(7)),
            "the asked move's one-cell marker has no ring"
        );
        let painted = input.clone();
        paint(&mut input, (7, 14));
        assert_eq!(input, painted, "a second frame does not duplicate sprites");
    }
}

/// The pane's width, the row under the bottom rule and the mode row's own words under it
/// (Claude's indent and words up to a notice of its own) in aterm-phase's capture `name`.
fn captured_tap_row(name: &str) -> (usize, String, String) {
    let path = aterm_tempfile::manifest_dir()
        .join("../aterm-phase/src/fixtures")
        .join(format!("{name}.txt"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let rows: Vec<&str> = text.lines().skip(1).collect();
    let rule = rows
        .iter()
        .rposition(|r| r.starts_with('\u{2500}'))
        .expect("the bottom rule");
    let tap = rows.get(rule + 1).copied().unwrap_or_default();
    let mode = rows.get(rule + 2).copied().unwrap_or_default();
    let own = mode[2..].find("  ").map_or(mode, |gap| &mode[..2 + gap]);
    (rows[rule].chars().count(), tap.to_owned(), own.to_owned())
}

/// One notice sample as a frame shows it: the pane's width, the notice, the room the glass
/// was given, and the laid line's cells.
type NoticeRung = (usize, &'static str, usize, usize);

/// THE SCENE'S LINE E ENDS TWO CELLS BEFORE CLAUDE'S NOTICE (§5.2, §7): the notice samples
/// set Claude's own notice on line E's row where Claude sets it ([`tap_row`], held here to
/// eight captures of Claude Code 2.1.300, 2.1.318 and 2.1.319, each with its own mode row),
/// and at 170, 169, 100 and 70 columns, on a dark ground and a light one, the claim's span
/// ([`quota::row_span`] of the row the scene claims) ends exactly two cells before the
/// notice, the glass lays from column 2 the rung that fits that span, every cell from the
/// glass's end to the notice is the pane's blank ground, and the notice keeps its cells in
/// the pane's ink. The rung shows the span's end only where it fills the span; since the
/// lead says `main 37% left` (2026-11-09, five cells wider than `main 63%`), no sample's rung
/// fills its span, and the `row_span` equality alone pins the margin of 2 (until then the
/// credits notice at 169 columns left R2's 140 cells exactly). At 170 and 169 the credits
/// notice costs case (b) a rung (R3, 115 cells, beside R2's 144 with the hint); at 100 and 70
/// it costs none. At 70 the launch's mode row leaves
/// the hint no room on the tap's row, so that sample is not laid. NEGATIVE CONTROLS (each
/// run once, restored): a claim that reads the row without its notice lays the rung that
/// fits `cols − 4` over the notice at 100 and 70 columns and fails; a notice set one cell
/// further right fails the captures; a write that stops at the glass's last ink leaves the
/// tail of Claude's words showing past the words-only rung at 70 columns and fails; a span
/// one cell short of the notice (`MARGIN` 1, 2026-11-08), which lays every frame of 170,
/// 100 and 70 columns as before, fails the span's end.
#[test]
fn the_scenes_line_e_ends_two_cells_before_claudes_notice() {
    let words = "week 37% left \u{b7} resets Sat 10:00";
    for (name, notice) in [
        ("claude-2.1.318-tap-row-week-credits", CREDITS),
        ("claude-2.1.318-tap-row-week-credits-70col", CREDITS),
        ("claude-2.1.318-tap-row-week-credits-manual-60col", CREDITS),
        ("claude-2.1.318-tap-row-week-launch", EFFORT_HINT),
    ] {
        let (cols, row, mode) = captured_tap_row(name);
        let captured = Notice {
            words: notice.words,
            mode: &mode,
        };
        assert_eq!(
            tap_row(words, Some(captured), cols).as_deref(),
            Some(row.as_str()),
            "{name}"
        );
        if name.ends_with("-manual-60col") {
            assert_eq!(
                tap_row(words, Some(captured), 59),
                None,
                "at 59 columns the notice has a row of its own"
            );
        }
    }
    for (name, notice) in [
        ("claude-2.1.318-tap-row-week-credits", CREDITS),
        ("claude-2.1.318-tap-row-week-launch", EFFORT_HINT),
    ] {
        assert_eq!(captured_tap_row(name).2, notice.mode, "{name}'s mode row");
    }
    // 2.1.300's row said the five-hour words, its notice where 2.1.318 sets it.
    let (cols, row, mode) = captured_tap_row("claude-2.1.300-tap-row-80-credits");
    let captured = Notice {
        words: CREDITS.words,
        mode: &mode,
    };
    assert_eq!(
        tap_row("5h 100% \u{b7} 7d 61%", Some(captured), cols).as_deref(),
        Some(row.as_str())
    );
    let (cols, row, mode) = captured_tap_row("claude-2.1.318-tap-row-week-credits-50col");
    assert_eq!(row, format!("  {words}"), "the notice has a row of its own");
    assert_eq!(mode, CREDITS.mode);
    assert_eq!(tap_row(words, Some(CREDITS), cols), None);
    // 2.1.319 at launch, before its first reading: the hint keeps the tap's row at 74
    // columns, one blank after the launch's 53-cell mode row, and has a row of its own at 73.
    let unread = "week \u{b7} no figure yet";
    let (cols, row, mode) = captured_tap_row("claude-2.1.319-tap-row-launch-74col");
    assert_eq!((cols, mode.as_str()), (74, EFFORT_HINT.mode));
    assert_eq!(
        tap_row(unread, Some(EFFORT_HINT), cols).as_deref(),
        Some(row.as_str()),
        "the hint on the tap's row at 74 columns"
    );
    let (cols, row, mode) = captured_tap_row("claude-2.1.319-tap-row-launch-73col");
    assert_eq!((cols, mode.as_str()), (73, EFFORT_HINT.mode));
    assert_eq!(row, format!("  {unread}"), "the hint has a row of its own");
    assert_eq!(tap_row(unread, Some(EFFORT_HINT), cols), None);
    let view = quota_scenes::cases()
        .into_iter()
        .find(|(name, _)| *name == "b")
        .map(|(_, view)| view)
        .expect("case (b)");
    assert_eq!(
        quota_scenes::tap_words(&view),
        words,
        "the captures' words are case (b)'s"
    );
    let mut rungs: Vec<NoticeRung> = Vec::new();
    for (fg, bg) in [("d0d0d0", "111318"), ("202020", "ffffff")] {
        for cols in [170_u16, 169, 100, 70] {
            let mut term = aterm_core::terminal::Terminal::new(48, cols);
            term.process(format!("\x1b]10;#{fg}\x07\x1b]11;#{bg}\x07x").as_bytes());
            let mut input = term.cell_frame(48, usize::from(cols));
            input.grid_bot_row = input.rows;
            paint(&mut input, (7, 14));
            let blank = RenderCell {
                ch: ' ',
                ..input.implicit_blank
            };
            for sample in &QUOTA_SAMPLES {
                let Some(notice) = sample.notice else {
                    continue;
                };
                let top = input.grid_top_row + sample.row;
                if tap_row(words, Some(notice), usize::from(cols)).is_none() {
                    assert!(
                        input.cells[top..top + 3]
                            .iter()
                            .flatten()
                            .all(|c| *c == blank),
                        "{cols} columns: no sample for {:?}",
                        notice.words
                    );
                    continue;
                }
                let cells = &input.cells[top + 2];
                let n = notice.words.chars().count();
                let at = usize::from(cols) - 2 - n;
                let shown: String = cells[at..at + n].iter().map(|c| c.ch).collect();
                assert_eq!(
                    shown, notice.words,
                    "{cols} columns: Claude's notice, where it sets it"
                );
                assert!(
                    cells[at..]
                        .iter()
                        .all(|c| c.fg == blank.fg && c.bg == blank.bg),
                    "{cols} columns: the notice keeps the pane's ink"
                );
                // The span the scene's claim reads off the row it set (as a live pane's
                // claim reads it): two cells short of the notice, whatever rung it lays.
                let tap = tap_row(words, Some(notice), usize::from(cols)).expect("its row");
                let span = quota::row_span(&tap, usize::from(cols)).expect("the tap's words");
                assert_eq!(
                    quota::INDENT + span.room + 2,
                    at,
                    "{cols} columns: the span ends two cells before the notice"
                );
                let room = span.room;
                let mut line = QuotaLine::default();
                assert!(view.lay(room, &mut line), "a rung fits {room}");
                let laid: String = cells[quota::INDENT..quota::INDENT + line.cells.len()]
                    .iter()
                    .map(|c| c.ch)
                    .collect();
                assert_eq!(
                    laid,
                    line.text(),
                    "{cols} columns: the rung that fits {room}"
                );
                assert!(
                    cells[quota::INDENT..quota::INDENT + line.cells.len()]
                        .iter()
                        .any(|c| c.fg != blank.fg),
                    "{cols} columns: the row is the glass's"
                );
                // From the glass's end: Claude's own words, longer than the words-only rung
                // at 70 columns, do not show past it.
                let end = quota::INDENT + line.cells.len();
                assert!(
                    end + 2 <= at,
                    "{cols} columns: the glass ends by {}",
                    at - 2
                );
                assert!(
                    cells[end..at].iter().all(|c| *c == blank),
                    "{cols} columns: blank ground from the glass's end to the notice"
                );
                rungs.push((usize::from(cols), notice.words, room, line.cells.len()));
            }
        }
    }
    let night = &rungs[..rungs.len() / 2];
    assert_eq!(night, &rungs[rungs.len() / 2..], "Day lays as Night");
    assert_eq!(
        night,
        [
            (170, CREDITS.words, 141, 115),
            (170, EFFORT_HINT.words, 146, 144),
            (169, CREDITS.words, 140, 115),
            (169, EFFORT_HINT.words, 145, 144),
            (100, CREDITS.words, 71, 69),
            (100, EFFORT_HINT.words, 76, 69),
            (70, CREDITS.words, 41, 34),
        ],
        "case (b)'s R3 beside the credits and R2 beside the hint at 170 and 169 (R2 with \
         none), its 4-a-day stop rung at 100, and its words alone at 70"
    );
}

/// THE SCENE DRAWS ITS GLASS MARKS AS A LIVE PANE DOES: every glass mark a composed row
/// holds (line D's `⎆ ◆ ⌂ ⎇`, a top rule's `●`) is drawn as its chrome icon in its own cell,
/// as the live footer's write draws it (`WindowCards::paint_marks`), and no other cell is.
/// The first Day captures read line D's `⎆ ⌂ ⎇` at 1.8:1 to 2.3:1 against `◆`'s 4.5:1: the
/// scene drew them from a fallback face's thin outlines, which no live pane shows, while
/// the icons stand in the cell's own ink, whole strokes. NEGATIVE CONTROL (run once,
/// restored): a scene that lays no marks leaves them to the font (a top rule's `●` first)
/// and fails.
#[test]
fn the_private_scene_draws_its_glass_marks_as_icons() {
    use aterm_core::render::BandIcon;
    let mut term = aterm_core::terminal::Terminal::new(40, 250);
    term.process(b"\x1b]10;#202020\x07\x1b]11;#ffffff\x07x");
    let mut input = term.cell_frame(40, 250);
    input.grid_bot_row = input.rows;
    paint(&mut input, (7, 14));
    let mut drawn = 0;
    for (r, cells) in input.cells.iter().enumerate() {
        let icons = input
            .chrome_raster(r)
            .map_or(&[][..], |raster| &raster.icons);
        for (c, cell) in cells.iter().enumerate() {
            let mark = BandIcon::glass_mark(cell.ch)
                .filter(|_| !cell.wide && cell.script == aterm_core::terminal::Script::Normal);
            let icon = icons
                .iter()
                .find(|i| usize::from(i.col) == c)
                .map(|i| i.icon);
            assert_eq!(icon, mark, "row {r} col {c} {:?}", cell.ch);
            drawn += usize::from(icon.is_some());
        }
    }
    let line_d = input.grid_top_row + 38;
    for ch in ['\u{2386}', '\u{25c6}', '\u{2302}', '\u{2387}'] {
        let col = input.cells[line_d]
            .iter()
            .position(|cell| cell.ch == ch)
            .unwrap_or_else(|| panic!("{ch:?} on line D"));
        assert_eq!(
            input
                .chrome_raster(line_d)
                .and_then(|raster| raster.icon_at(col)),
            BandIcon::glass_mark(ch),
            "line D's {ch:?} is its icon"
        );
    }
    assert!(
        input
            .chrome_raster(input.grid_top_row + 2)
            .is_some_and(|raster| !raster.icons.is_empty()),
        "a top rule's `●` is its icon"
    );
    assert!(drawn > 4, "{drawn} marks drawn");
}
