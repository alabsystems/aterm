// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The card line's link and the harness banner over Claude Code's startup announcement, on a
//! real engine and the real splice.

use super::*;

#[cfg(unix)]
use aterm_window::keyboard::ModifiersState;

/// The row the announcement's slot starts on in [`slot_screen`]: under the card's blank row.
const SLOT_TOP: usize = 5;

/// The measured captures' slots, each with the pane width it was read at: the auto-mode notice
/// wrapped at 144 and 80 columns, a credit offer and its hidden-count row, and the debug-mode
/// notice's rows. Each is fed as plain text, in the pane's own ink as an announcement's words
/// are; the debug-mode notice's own bar is faint, which keeps it the vendor's
/// (`only_an_announcements_inks_wear_alabs_banner`).
#[cfg(unix)]
const MEASURED: [(&str, usize); 4] = [
    ("claude-2.1.283-api-error-enotfound-measured.txt", 144),
    ("claude-2.1.283-api-error-enotfound-80col-measured.txt", 80),
    ("claude-2.1.281-box-rm-auto-deny.txt", 120),
    ("claude-2.1.308-usage-dismissed-fullscreen.txt", 120),
];

/// The slot rows of capture `name`: its quote-bar rows, then its hidden-count row if it has one.
fn measured_slot(name: &str) -> Vec<String> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../aterm-phase/src/fixtures")
        .join(name);
    let text = std::fs::read_to_string(&path).expect("the capture");
    let rows = aterm_phase::prompt::fixtures::screen(&text);
    let top = rows
        .iter()
        .position(|r| r.starts_with('\u{258E}'))
        .expect("the slot");
    let mut slot: Vec<String> = rows[top..]
        .iter()
        .take_while(|r| r.starts_with('\u{258E}'))
        .cloned()
        .collect();
    if let Some(hidden) = rows
        .get(top + slot.len())
        .filter(|r| r.contains(aterm_phase::anchor!("notice.more")))
    {
        slot.push(hidden.clone());
    }
    slot
}

fn slot_screen(cols: usize, slot: &[String]) -> Vec<String> {
    let mut screen = card_screen(24, cols, MODEL, VERSION);
    for (k, row) in slot.iter().enumerate() {
        screen[SLOT_TOP + k].clone_from(row);
    }
    screen
}

fn slot_glass(cols: usize, slot: &[String]) -> (App, WindowId, u64) {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let session = app.focused_session_id(wid).expect("the front session");
    if cols != 80 {
        assert!(app.apply_term_resize(wid, 24, u16_of(cols)));
    }
    publish(&app, session, VERSION);
    feed(&app, session, &slot_screen(cols, slot));
    (app, wid, session)
}

/// The glass's text over `cols` of `row`, a row's implicit blanks past its last cell read as
/// spaces.
fn padded_text(app: &App, wid: WindowId, row: usize, cols: std::ops::Range<usize>) -> String {
    let mut text = glass_text(app, wid, row, cols.clone());
    let short = cols.len() - text.chars().count();
    text.extend(std::iter::repeat_n(' ', short));
    text
}

fn hits_of(
    app: &App,
    wid: WindowId,
    link: GlassLink,
) -> Vec<(u64, usize, std::ops::Range<usize>, bool)> {
    app.windows[&wid]
        .claude_card
        .glass_hits
        .iter()
        .filter(|h| h.link == link)
        .map(|h| (h.session, h.frame_row, h.cols.clone(), h.url.is_some()))
        .collect()
}

#[cfg(unix)]
fn hand_at(app: &mut App, wid: WindowId, at: (usize, usize), mods: ModifiersState) -> bool {
    let (x, y) = cell_px(app, wid, at);
    app.windows.get_mut(&wid).unwrap().mods = mods;
    app.on_cursor_moved(wid, x, y);
    app.update_hover_cursor(wid);
    let hand = app.windows[&wid].hover_pointer;
    app.windows.get_mut(&wid).unwrap().mods = ModifiersState::empty();
    hand
}

/// THE CARD'S LINE LINKS THE SITE: its `alab.systems` letters alone carry a single underline in
/// their own ink, one hit on exactly those cells; the hand shows over them and a plain press opens
/// the site, once per double click; a Shift press, a press elsewhere on the line and a press in
/// the gap open nothing.
#[cfg(unix)]
#[test]
fn the_cards_line_links_the_site() {
    let (mut app, wid, session) = glass(120);
    let host = supervise(&mut app, session);
    frame(&mut app, wid);
    let row = strip_rows(&app, wid) + 4;
    let start = 11 + LINE_LEAD;
    let want = expected_line(11, 120);
    assert_eq!(want, line_texts_for(3)[0], "CONTROL: the widest rung");
    assert_eq!(
        glass_text(&app, wid, row, start..start + want.chars().count()),
        want
    );
    let site = start + text_cells(&want) - text_cells(SITE);
    assert_eq!(glass_text(&app, wid, row, site..site + 12), SITE);
    let scratch = &app.windows[&wid].input_scratch;
    let underlined: Vec<usize> = (0..scratch.cells[row].len())
        .filter(|&col| scratch.cells[row][col].underline != UnderlineStyle::None)
        .collect();
    assert_eq!(
        underlined,
        (site..site + 12).collect::<Vec<_>>(),
        "the site's letters alone underlined"
    );
    assert!(
        scratch.cells[row][site..site + 12]
            .iter()
            .all(|c| c.underline == UnderlineStyle::Single && c.underline_color.is_none()),
        "a single underline in each letter's own ink"
    );
    assert_eq!(
        hits_of(&app, wid, GlassLink::Line),
        [(session, row, site..site + 12, true)],
        "one link, on the site's letters"
    );
    assert!(
        hand_at(&mut app, wid, (row, site), ModifiersState::empty()),
        "the hand over the link"
    );
    assert!(
        hand_at(&mut app, wid, (row, site + 11), link_modifier()),
        "and with the link modifier"
    );
    assert!(
        !hand_at(&mut app, wid, (row, start), ModifiersState::empty()),
        "CONTROL: no hand on the rest of the line"
    );
    assert!(
        !hand_at(&mut app, wid, (row, site), ModifiersState::SHIFT),
        "CONTROL: no hand under Shift"
    );
    let _ = crate::platform::take_opened_urls();
    let mut when = a_minute_ago();
    for _ in 0..3 {
        click_when(
            &mut app,
            wid,
            (row, site + 3),
            ModifiersState::empty(),
            when,
        );
        frame(&mut app, wid);
    }
    assert_eq!(
        crate::platform::take_opened_urls(),
        vec![SITE_URL.to_string()],
        "a triple click opens the site once"
    );
    when += apart();
    click_when(&mut app, wid, (row, site), link_modifier(), when);
    assert_eq!(
        crate::platform::take_opened_urls(),
        vec![SITE_URL.to_string()],
        "a link-modifier press opens it"
    );
    frame(&mut app, wid);
    for (at, mods) in [
        (site, ModifiersState::SHIFT),
        (start, ModifiersState::empty()),
        (site - 2, ModifiersState::empty()),
    ] {
        when += apart();
        click_when(&mut app, wid, (row, at), mods, when);
        frame(&mut app, wid);
        assert!(
            crate::platform::take_opened_urls().is_empty(),
            "col {at} {mods:?}: nothing opened"
        );
    }
    host.shutdown_and_join();
}

/// A NARROW LINE KEEPS ITS LINK: at 50 columns, too narrow for the whole line, it steps down to
/// `alab claude harness · alab.systems`, the site's letters still underlined and one hit (the
/// 2026-10-26 ladder showed the head alone there, with no link).
#[test]
fn a_narrow_line_keeps_its_link() {
    let (mut app, wid, session) = glass(50);
    let host = supervise(&mut app, session);
    frame(&mut app, wid);
    let row = strip_rows(&app, wid) + 4;
    let want = expected_line(11, 50);
    assert_eq!(
        want,
        line_texts_for(3)[2],
        "CONTROL: the harness and the site"
    );
    assert_eq!(
        glass_text(&app, wid, row, 13..13 + want.chars().count()),
        want
    );
    let site = line_link(&want).expect("the site's letters");
    let site = 13 + site.start..13 + site.end;
    let underlined: Vec<usize> = app.windows[&wid].input_scratch.cells[row]
        .iter()
        .enumerate()
        .filter(|(_, c)| c.underline != UnderlineStyle::None)
        .map(|(col, _)| col)
        .collect();
    assert_eq!(
        underlined,
        site.clone().collect::<Vec<_>>(),
        "the site's letters alone underlined"
    );
    assert_eq!(
        hits_of(&app, wid, GlassLink::Line),
        [(session, row, site, true)],
        "one hit, on the site's letters"
    );
    host.shutdown_and_join();
}

/// THE STARTUP ANNOUNCEMENT WEARS ALAB'S BANNER ON THE GLASS ONLY, over every measured slot: the
/// slot's first row shows `✦` and the widest rung that fits, its URL alone underlined and inked
/// letter by letter with the rest; every other slot row and the hidden-count row are blank to the
/// pane's edge; the link's hit is the URL's cells, each covered row a hit of its own; the hand
/// shows over the URL and a plain press opens the site; the terminal's text stays the vendor's.
#[cfg(unix)]
#[test]
fn the_startup_announcement_wears_alabs_banner_on_the_glass_only() {
    for (name, cols) in MEASURED {
        let slot = measured_slot(name);
        assert!(!slot.is_empty(), "{name}: CONTROL: the slot");
        let (mut app, wid, session) = slot_glass(cols, &slot);
        let before = engine_rows(&app, session);
        frame(&mut app, wid);
        let strip = strip_rows(&app, wid);
        for (k, vendor) in slot.iter().enumerate() {
            assert_eq!(
                glass_text(&app, wid, strip + SLOT_TOP + k, 0..vendor.chars().count()),
                *vendor,
                "{name}: CONTROL: no host, the vendor's slot"
            );
        }
        assert_eq!(hits_of(&app, wid, GlassLink::Announcement), []);
        let host = supervise(&mut app, session);
        frame(&mut app, wid);
        let row = strip + SLOT_TOP;
        let rung = ANNOUNCEMENT
            .words_within(cols - 1 - LINE_LEAD)
            .expect("a rung fits");
        assert_eq!(rung, ANNOUNCEMENT.rungs[0], "{name}: the widest rung");
        let words = format!("{}{}", rung.lead, rung.link);
        let cells = rung.cells();
        assert_eq!(glass_text(&app, wid, row, 0..LINE_LEAD), "  ", "{name}");
        assert_eq!(
            glass_text(&app, wid, row, LINE_LEAD..LINE_LEAD + cells),
            words,
            "{name}: ALab's announcement"
        );
        assert_eq!(
            padded_text(&app, wid, row, LINE_LEAD + cells..cols),
            " ".repeat(cols - LINE_LEAD - cells),
            "{name}: blank after the words"
        );
        for k in 1..slot.len() {
            assert_eq!(
                padded_text(&app, wid, row + k, 0..cols),
                " ".repeat(cols),
                "{name}: slot row {k} blanked"
            );
        }
        let at = LINE_LEAD + text_cells(rung.lead);
        let url = at..at + text_cells(rung.link);
        let scratch = &app.windows[&wid].input_scratch;
        let underlined: Vec<usize> = (0..scratch.cells[row].len())
            .filter(|&col| scratch.cells[row][col].underline != UnderlineStyle::None)
            .collect();
        assert_eq!(
            underlined,
            url.clone().collect::<Vec<_>>(),
            "{name}: the URL's letters alone underlined"
        );
        assert!(
            scratch.cells[row][url.clone()]
                .iter()
                .all(|c| c.underline == UnderlineStyle::Single && c.underline_color.is_none()),
            "{name}: a single underline in each letter's own ink"
        );
        let inked: Vec<usize> = scratch
            .ink
            .iter()
            .filter(|c| usize::from(c.row) == row)
            .map(|c| usize::from(c.col))
            .collect();
        assert_eq!(
            inked,
            (LINE_LEAD..LINE_LEAD + cells).collect::<Vec<_>>(),
            "{name}: every letter inked, nothing else on the row"
        );
        assert!(
            scratch
                .ink
                .iter()
                .all(|c| !(row + 1..row + slot.len()).contains(&usize::from(c.row))),
            "{name}: nothing inked on the blanked rows"
        );
        let star = scratch
            .word_decorations
            .iter()
            .find(|d| usize::from(d.row) == row)
            .expect("the banner's star");
        assert_eq!(
            (usize::from(star.col), star.glyph),
            (0, DecoGlyph::Star4),
            "{name}: the star over the quote bar"
        );
        let mut want = vec![
            (session, row, url.clone(), true),
            (session, row, 0..cols, false),
        ];
        want.extend((1..slot.len()).map(|k| (session, row + k, 0..cols, false)));
        assert_eq!(
            hits_of(&app, wid, GlassLink::Announcement),
            want,
            "{name}: the link on the URL, every covered row the banner's"
        );
        assert_eq!(
            engine_rows(&app, session),
            before,
            "{name}: the terminal's text"
        );
        assert!(
            hand_at(&mut app, wid, (row, url.start), ModifiersState::empty()),
            "{name}: the hand over the URL"
        );
        assert!(
            !hand_at(&mut app, wid, (row, LINE_LEAD), ModifiersState::empty()),
            "{name}: CONTROL: no hand over the announcement's words"
        );
        let _ = crate::platform::take_opened_urls();
        let when = a_minute_ago();
        click_when(
            &mut app,
            wid,
            (row, url.end - 1),
            ModifiersState::empty(),
            when,
        );
        assert_eq!(
            crate::platform::take_opened_urls(),
            vec![SITE_URL.to_string()],
            "{name}: a plain press opens the site"
        );
        frame(&mut app, wid);
        if slot.len() > 1 {
            click_when(
                &mut app,
                wid,
                (row + 1, 4),
                ModifiersState::empty(),
                when + apart(),
            );
            assert!(
                crate::platform::take_opened_urls().is_empty(),
                "{name}: nothing from a blanked row"
            );
        }
        let ws = app.windows.get_mut(&wid).unwrap();
        assert!(crate::claude_footer::undo(
            &mut ws.claude_footer_undo,
            &mut ws.input_scratch
        ));
        for (k, vendor) in slot.iter().enumerate() {
            let back: String = ws.input_scratch.cells[row + k]
                .iter()
                .take(vendor.chars().count())
                .map(|c| c.ch)
                .collect();
            assert_eq!(back, *vendor, "{name}: undo gives the vendor's row back");
        }
        host.shutdown_and_join();
    }
}

/// The hidden-count row under a one-row slot is blanked with it, and a reader meets the URL as
/// a link of its own slot and presses it.
#[cfg(a11y_tree)]
#[test]
fn the_hidden_count_goes_with_the_slot_and_a_reader_presses_the_link() {
    let slot = measured_slot("claude-2.1.281-box-rm-auto-deny.txt");
    assert_eq!(slot.len(), 2, "CONTROL: the offer and its hidden count");
    let (mut app, wid, session) = slot_glass(120, &slot);
    let host = supervise(&mut app, session);
    frame(&mut app, wid);
    let row = strip_rows(&app, wid) + SLOT_TOP;
    assert_eq!(padded_text(&app, wid, row + 1, 0..120), " ".repeat(120));
    let tree = app.grid_a11y_tree(wid, None).expect("the grid's tree");
    let links: Vec<_> = tree
        .nodes
        .iter()
        .filter(|(_, n)| n.role() == aterm_a11y::model::Role::Link)
        .filter_map(|(id, n)| Some((crate::a11y_tree::chip_for(*id)?, n.label()?.to_owned())))
        .collect();
    let announcement = crate::a11y_tree::HarnessChip::Link(GlassLink::Announcement);
    assert!(
        links.contains(&((session, announcement), SITE_URL.to_owned())),
        "the announcement's link node: {links:?}"
    );
    let _ = crate::platform::take_opened_urls();
    app.press_claude_chip_for_reader(wid, session, announcement);
    assert_eq!(
        crate::platform::take_opened_urls(),
        vec![SITE_URL.to_string()]
    );
    host.shutdown_and_join();
}

/// THE LAUNCH NEWS WITH ITS BAR OFF WEARS ALAB'S BANNER TOO: 2.1.280's `  Get to finished work
/// sooner with Opus 5.5. …` at the indent and its `  1 more notice hidden`, over a blank row and
/// the transcript's first row, show ALab's announcement, the hidden count blanked, the URL the
/// link a plain press opens; the terminal's text stays the vendor's. So do they over blank rows
/// down to the composer at the screen's foot (the fullscreen renderer's launch). With words at
/// column 0 under them (no edge: model output) the rows are no slot, and stay the vendor's.
#[cfg(unix)]
#[test]
fn the_launch_news_with_its_bar_off_wears_alabs_banner_too() {
    let name = "claude-2.1.280-box-bash-touch.txt";
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../aterm-phase/src/fixtures")
        .join(name);
    let recorded =
        aterm_phase::prompt::fixtures::screen(&std::fs::read_to_string(path).expect("the capture"));
    let slot: Vec<String> = recorded[5..7].to_vec();
    assert!(
        slot[0].starts_with("  Get to finished work sooner") && slot[1] == "  1 more notice hidden",
        "CONTROL: the capture's slot with its bar off: {slot:?}"
    );
    let cols = 120;
    for (under, banner) in [
        (Some("\u{276F} hi"), true),
        (None, true),
        (Some("words at column 0"), false),
    ] {
        let mut screen = slot_screen(cols, &slot);
        if let Some(under) = under {
            screen[SLOT_TOP + slot.len() + 1] = under.into();
        }
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let session = app.focused_session_id(wid).expect("the front session");
        assert!(app.apply_term_resize(wid, 24, u16_of(cols)));
        publish(&app, session, VERSION);
        feed(&app, session, &screen);
        let before = engine_rows(&app, session);
        let host = supervise(&mut app, session);
        frame(&mut app, wid);
        let row = strip_rows(&app, wid) + SLOT_TOP;
        if !banner {
            for (k, vendor) in slot.iter().enumerate() {
                assert_eq!(
                    glass_text(&app, wid, row + k, 0..vendor.chars().count()),
                    *vendor,
                    "NEGATIVE CONTROL: no edge under the rows, the vendor's rows"
                );
            }
            assert_eq!(hits_of(&app, wid, GlassLink::Announcement), []);
            host.shutdown_and_join();
            continue;
        }
        let rung = ANNOUNCEMENT.rungs[0];
        let cells = rung.cells();
        assert_eq!(
            glass_text(&app, wid, row, LINE_LEAD..LINE_LEAD + cells),
            format!("{}{}", rung.lead, rung.link),
            "ALab's announcement over the vendor's news"
        );
        assert_eq!(
            padded_text(&app, wid, row, LINE_LEAD + cells..cols),
            " ".repeat(cols - LINE_LEAD - cells),
            "blank after the words"
        );
        assert_eq!(
            padded_text(&app, wid, row + 1, 0..cols),
            " ".repeat(cols),
            "the hidden count blanked"
        );
        let at = LINE_LEAD + text_cells(rung.lead);
        let url = at..at + text_cells(rung.link);
        assert_eq!(
            hits_of(&app, wid, GlassLink::Announcement),
            [
                (session, row, url.clone(), true),
                (session, row, 0..cols, false),
                (session, row + 1, 0..cols, false),
            ],
            "the link on the URL, both covered rows the banner's"
        );
        assert_eq!(engine_rows(&app, session), before, "the terminal's text");
        let _ = crate::platform::take_opened_urls();
        click_when(
            &mut app,
            wid,
            (row, url.start + 1),
            ModifiersState::empty(),
            a_minute_ago(),
        );
        assert_eq!(
            crate::platform::take_opened_urls(),
            vec![SITE_URL.to_string()],
            "a plain press opens the site"
        );
        host.shutdown_and_join();
    }
}

/// SGR inks Claude Code 2.1.312 draws its notices in: faint (an info notice's bar, a footer), the
/// Claude colour (an announcement's bar or title), the warning colour (the emergency tip), and the
/// pane's own ink back.
const FAINT: &str = "\x1b[2m";
const CLAUDE: &str = "\x1b[38;2;215;119;87m";
const WARNING: &str = "\x1b[38;2;255;193;7m";
/// Claude Code's `permission` colour, which its markdown draws code in.
const CODE: &str = "\x1b[38;2;177;185;249m";
const PLAIN: &str = "\x1b[0m";

/// ONLY AN ANNOUNCEMENT'S INKS WEAR ALAB'S BANNER. The reader places a slot by its text, so its
/// inks decide: a barred slot whose bar is the pane's ink drawn faint is an info notice (Claude
/// Code dims every bar but an announcement's) and stays the vendor's, as does a slot with its bar
/// off holding no letter in the pane's own ink (the emergency tip, all warning colour); the same
/// rows with the bar in the Claude colour or the words in the pane's ink wear the banner
/// (controls). Each verdict holds once the card has scrolled off over the slot.
#[test]
fn only_an_announcements_inks_wear_alabs_banner() {
    let barred = |ink: &str| {
        vec![format!(
            "{ink}\u{258E}{PLAIN} Debug mode enabled \u{b7} logging to ~/.claude/debug/x.txt"
        )]
    };
    let bare = |ink: &str| {
        vec![format!(
            "  {ink}Lab emergency tip: update Claude Code now.{PLAIN}"
        )]
    };
    let cases = [
        ("a faint bar: an info notice", barred(FAINT), false),
        ("CONTROL: the Claude colour's bar", barred(CLAUDE), true),
        ("CONTROL: a bar in the pane's ink", barred(""), true),
        (
            "its bar off, all in warning colour: the tip",
            bare(WARNING),
            false,
        ),
        ("CONTROL: its bar off, in the pane's ink", bare(""), true),
    ];
    let cols = 120;
    for (what, slot, banner) in cases {
        let mut screen = slot_screen(cols, &slot);
        screen[SLOT_TOP + slot.len() + 1] = "\u{276F} hi".into();
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let session = app.focused_session_id(wid).expect("the front session");
        assert!(app.apply_term_resize(wid, 24, u16_of(cols)));
        publish(&app, session, VERSION);
        feed(&app, session, &screen);
        let host = supervise(&mut app, session);
        frame(&mut app, wid);
        let row = strip_rows(&app, wid) + SLOT_TOP;
        let vendor = engine_rows(&app, session)[SLOT_TOP].clone();
        let shown = glass_text(&app, wid, row, 0..vendor.chars().count());
        if banner {
            assert_ne!(shown, vendor, "{what}: ALab's banner over the slot");
            assert!(
                !hits_of(&app, wid, GlassLink::Announcement).is_empty(),
                "{what}: the banner's link"
            );
        } else {
            assert_eq!(shown, vendor, "{what}: the vendor's rows");
            assert_eq!(
                hits_of(&app, wid, GlassLink::Announcement),
                [],
                "{what}: no link"
            );
        }
        // The card scrolls off, the slot's rows on top: the verdict its card's frames gave holds.
        feed(&app, session, &grown(&screen, SLOT_TOP));
        frame(&mut app, wid);
        assert_eq!(
            !hits_of(&app, wid, GlassLink::Announcement).is_empty(),
            banner,
            "{what}: the card scrolled off"
        );
        host.shutdown_and_join();
    }
}

/// A SLOT WITH ITS BAR OFF WEARS THE BANNER FROM ITS NEWS DOWN: 2.1.312's measured news (a title
/// in the Claude colour, its words in the pane's ink, a faint footer) is covered whole, and so is
/// news in paragraphs, a blank row between them, with no title (over a row of code too), or with
/// no colour at all; the emergency tip over the news, in warning colour or faint (drawn at full
/// ink with `faint_opacity` 1, or blended to it at 0.999), keeps the vendor's words while the news
/// under it wears the banner, and a hook's message straight under the news stays the vendor's, before the card
/// scrolls off and after.
#[test]
fn a_slot_with_its_bar_off_wears_the_banner_from_its_news_down() {
    let title = format!("  {CLAUDE}Lab news title{PLAIN}");
    let words = "  A lab startup announcement with its accent bar off.".to_owned();
    let second = "  A second paragraph of it.".to_owned();
    let footer = format!("  {FAINT}Lab footer: learn more at example.invalid{PLAIN}");
    let tip = "Lab emergency tip: update Claude Code now.";
    let cases = [
        (
            "the measured news",
            None,
            vec![title.clone(), words.clone(), footer.clone()],
            0..3,
        ),
        (
            "news in paragraphs",
            None,
            vec![
                title.clone(),
                words.clone(),
                String::new(),
                second.clone(),
                footer.clone(),
            ],
            0..5,
        ),
        (
            "news with no title, in paragraphs",
            None,
            vec![words.clone(), String::new(), second.clone(), footer.clone()],
            0..4,
        ),
        (
            "news in paragraphs with no colour at all",
            None,
            vec![
                "  Lab news title".to_owned(),
                words.clone(),
                String::new(),
                second,
                "  Lab footer: learn more at example.invalid".to_owned(),
            ],
            0..5,
        ),
        (
            "the tip over the news",
            None,
            vec![
                format!("  {WARNING}{tip}{PLAIN}"),
                String::new(),
                title.clone(),
                words.clone(),
                footer.clone(),
            ],
            2..5,
        ),
        (
            "the faint tip over the news, faint drawn at full ink",
            Some(1.0),
            vec![
                format!("  {FAINT}{tip}{PLAIN}"),
                String::new(),
                title.clone(),
                words.clone(),
                footer.clone(),
            ],
            2..5,
        ),
        (
            "the faint tip over the news, faint blended to the pane's ink",
            Some(0.999),
            vec![
                format!("  {FAINT}{tip}{PLAIN}"),
                String::new(),
                title.clone(),
                words.clone(),
                footer.clone(),
            ],
            2..5,
        ),
        (
            "CONTROL: the same rows with faint drawn faint, a block in the pane's ink news",
            None,
            vec![
                format!("  {tip}"),
                String::new(),
                title.clone(),
                words.clone(),
                "  Lab footer: learn more at example.invalid".to_owned(),
            ],
            0..5,
        ),
        (
            "news with no title over a row of code",
            None,
            vec![
                words.clone(),
                String::new(),
                format!("  {CODE}claude --remote{PLAIN}"),
                footer.clone(),
            ],
            0..4,
        ),
        (
            "the news over a hook's message straight under it",
            None,
            vec![
                title,
                words,
                footer,
                format!(
                    "  {FAINT}\u{23BF}{PLAIN}  SessionStart:startup says: Loaded 3 project memories"
                ),
            ],
            0..3,
        ),
    ];
    let cols = 120;
    for (what, faint_opacity, slot, covered) in cases {
        let mut screen = slot_screen(cols, &slot);
        screen[SLOT_TOP + slot.len() + 1] = "\u{276F} hi".into();
        let mut app = App::headless_for_test();
        app.config.faint_opacity = faint_opacity;
        app.install_resolved_colors(crate::app_colorscheme::ThemeChange::ConfigReload);
        let wid = WindowId(0);
        let session = app.focused_session_id(wid).expect("the front session");
        assert!(app.apply_term_resize(wid, 24, u16_of(cols)));
        publish(&app, session, VERSION);
        feed(&app, session, &screen);
        let host = supervise(&mut app, session);
        let look = |app: &mut App, top: usize, when: &str| {
            frame(app, wid);
            let vendor = engine_rows(app, session);
            for k in (0..slot.len()).filter(|&k| !slot[k].is_empty()) {
                let at = top + k;
                let shown = glass_text(app, wid, strip_rows(app, wid) + at, 0..cols);
                if covered.contains(&k) {
                    assert_ne!(shown, vendor[at], "{what} {when}: row {k} wears the banner");
                } else {
                    assert_eq!(
                        shown, vendor[at],
                        "{what} {when}: row {k} stays the vendor's"
                    );
                }
            }
        };
        look(&mut app, SLOT_TOP, "under the card");
        feed(&app, session, &grown(&screen, SLOT_TOP));
        look(&mut app, 0, "once the card scrolled off");
        host.shutdown_and_join();
    }
}

/// A SLOT REDRAWN UNDER A CUT CARD IS READ AGAIN: news wears the banner under the whole card;
/// the header redrawn as the card's top rows go off the screen, the slot now the emergency tip
/// over the saved-model nudge, the banner is over the nudge alone, and it stays off the tip once
/// the card has scrolled off.
#[test]
fn a_slot_redrawn_under_a_cut_card_is_read_again() {
    let cols = 120;
    let with_prompt = |slot: &[String]| {
        let mut screen = slot_screen(cols, slot);
        screen[SLOT_TOP + slot.len() + 1] = "\u{276F} hi".into();
        screen
    };
    let news = with_prompt(&[
        format!("  {CLAUDE}Lab news title{PLAIN}"),
        "  A lab startup announcement with its accent bar off.".to_owned(),
        format!("  {FAINT}Lab footer: learn more at example.invalid{PLAIN}"),
    ]);
    let redrawn = with_prompt(&[
        format!("  {WARNING}Lab emergency tip: update Claude Code now.{PLAIN}"),
        String::new(),
        "  Lab saved-model nudge in the pane's own ink.".to_owned(),
    ]);
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let session = app.focused_session_id(wid).expect("the front session");
    assert!(app.apply_term_resize(wid, 24, u16_of(cols)));
    publish(&app, session, VERSION);
    feed(&app, session, &news);
    let host = supervise(&mut app, session);
    frame(&mut app, wid);
    assert!(
        !hits_of(&app, wid, GlassLink::Announcement).is_empty(),
        "CONTROL: the news wears the banner"
    );
    for (k, when) in [
        (2, "under the cut card"),
        (SLOT_TOP, "once the card scrolled off"),
    ] {
        feed(&app, session, &grown(&redrawn, k));
        frame(&mut app, wid);
        let vendor = engine_rows(&app, session);
        let top = SLOT_TOP - k;
        let shown = |r: usize| glass_text(&app, wid, strip_rows(&app, wid) + top + r, 0..cols);
        assert_eq!(shown(0), vendor[top], "{when}: the tip stays the vendor's");
        assert_ne!(
            shown(2),
            vendor[top + 2],
            "{when}: the nudge wears the banner"
        );
    }
    host.shutdown_and_join();
}

/// A REFUSED SLOT KEEPS NO BANNER THE ENTRY DREW BEFORE: news in the pane's ink wears the banner;
/// once the vendor's slot gives way to the emergency tip in warning colour (the saved-model nudge
/// released, the header redrawn) the banner leaves, and it stays gone as the card scrolls off over
/// the tip's rows.
#[test]
fn a_refused_slot_keeps_no_banner_the_entry_drew_before() {
    let cols = 120;
    let screen_of = |row: &str| {
        let mut screen = slot_screen(cols, &[row.to_owned()]);
        screen[SLOT_TOP + 2] = "\u{276F} hi".into();
        screen
    };
    let news = screen_of("  Lab launch news in the pane's own ink.");
    let tip = screen_of("  \x1b[38;2;255;193;7mLab emergency tip: update Claude Code now.\x1b[0m");
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let session = app.focused_session_id(wid).expect("the front session");
    assert!(app.apply_term_resize(wid, 24, u16_of(cols)));
    publish(&app, session, VERSION);
    feed(&app, session, &news);
    let host = supervise(&mut app, session);
    frame(&mut app, wid);
    assert!(
        !hits_of(&app, wid, GlassLink::Announcement).is_empty(),
        "CONTROL: the news wears the banner"
    );
    feed(&app, session, &tip);
    frame(&mut app, wid);
    assert_eq!(
        hits_of(&app, wid, GlassLink::Announcement),
        [],
        "the tip under the card: no banner"
    );
    feed(&app, session, &grown(&tip, SLOT_TOP));
    frame(&mut app, wid);
    assert_eq!(
        hits_of(&app, wid, GlassLink::Announcement),
        [],
        "the card scrolled off over the tip: no banner"
    );
    host.shutdown_and_join();
}

/// NARROW PANES PICK NARROWER RUNGS: at 50 columns the decorated card's banner reads
/// `ALab · https://alab.systems`; forged narrower panes step down to the URL and the site's
/// name, each inside the pane's last column with only its link underlined, and none at all
/// below the site's name.
#[test]
fn narrow_panes_pick_narrower_announcement_rungs() {
    let slot = measured_slot("claude-2.1.308-usage-dismissed-fullscreen.txt");
    let (mut app, wid, session) = slot_glass(50, &slot);
    let host = supervise(&mut app, session);
    frame(&mut app, wid);
    let row = strip_rows(&app, wid) + SLOT_TOP;
    let rung = ANNOUNCEMENT.rungs[1];
    assert_eq!(rung.lead, "ALab \u{b7} ");
    assert_eq!(
        glass_text(&app, wid, row, LINE_LEAD..LINE_LEAD + rung.cells()),
        format!("{}{}", rung.lead, rung.link),
        "the second rung at 50 columns"
    );
    let pane = PaneGeom {
        strip: strip_rows(&app, wid),
        row_off: 0,
        col_off: 0,
        rows: 24,
        cols: 50,
    };
    let ws = &app.windows[&wid];
    let gate = app
        .pool
        .get(session)
        .unwrap()
        .ctx
        .timeline
        .lock()
        .unwrap()
        .card_gate();
    let mut check = GateCheck::new(gate, FenceFrame::of(&ws.input_scratch));
    let leaf = card_leaf(
        &ws.claude_footer_texts.borrow(),
        app.config.claude_footer_card(),
        0,
        session,
        pane,
        RenderCell::default(),
        AutopilotView::Cruise,
        true,
        &mut check,
    );
    let (read, _, _) = leaf.card.as_ref().expect("the decorated card's reading");
    let place = leaf.announcement.expect("the announcement beside the card");
    assert_eq!((place.top, place.rows, place.hidden), (SLOT_TOP, 2, None));
    let card = &ws.claude_card.entries[0].card;
    let drawn = |cols: usize| {
        let pane = PaneGeom { cols, ..pane };
        let mut banner = BannerGlass::default();
        make_banner(
            &mut banner,
            ANNOUNCEMENT,
            BannerPlace::announcement(&place),
            card,
            Some(read),
            pane,
            RenderCell::default(),
            &ws.input_scratch,
            CardPalette::default(),
            None,
            false,
        );
        banner
    };
    let mut seen = Vec::new();
    for cols in 4..=80 {
        let banner = drawn(cols);
        let room = cols - 1 - LINE_LEAD;
        let Some(rung) = ANNOUNCEMENT.words_within(room) else {
            assert!(!banner.on, "{cols}: no rung fits, no banner");
            continue;
        };
        assert!(banner.on, "{cols}: a banner");
        if !seen.contains(&rung) {
            seen.push(rung);
        }
        let first = &banner.rows()[0];
        assert_eq!(first.cells.len(), cols, "{cols}: blank to the pane's edge");
        let letters: String = first.cells[LINE_LEAD..LINE_LEAD + rung.cells()]
            .iter()
            .map(|c| c.ch)
            .collect();
        assert_eq!(letters, format!("{}{}", rung.lead, rung.link), "{cols}");
        let at = LINE_LEAD + text_cells(rung.lead);
        let underlined: Vec<usize> = (0..first.cells.len())
            .filter(|&k| first.cells[k].underline == UnderlineStyle::Single)
            .collect();
        assert_eq!(
            underlined,
            (at..at + text_cells(rung.link)).collect::<Vec<_>>(),
            "{cols}: the link alone underlined"
        );
        let from = if rung.lead.is_empty() { 0 } else { at };
        assert_eq!(banner.link, from..at + text_cells(rung.link), "{cols}");
        assert!(
            LINE_LEAD + rung.cells() < cols,
            "{cols}: inside the last column"
        );
    }
    assert_eq!(
        seen,
        ANNOUNCEMENT.rungs.iter().rev().copied().collect::<Vec<_>>(),
        "every rung, narrowest first as the pane widens"
    );
    host.shutdown_and_join();
}

/// No banner where the card is not decorated: without a host, under the master switch off, and
/// with `[claude_footer] card = false`, the vendor's slot stays and nothing is hit.
#[test]
fn the_announcement_stays_the_vendors_where_the_card_is_bare() {
    let slot = measured_slot("claude-2.1.308-usage-dismissed-fullscreen.txt");
    let bare = |app: &App, wid: WindowId| {
        let row = strip_rows(app, wid) + SLOT_TOP;
        slot.iter()
            .enumerate()
            .all(|(k, vendor)| glass_text(app, wid, row + k, 0..vendor.chars().count()) == *vendor)
            && hits_of(app, wid, GlassLink::Announcement).is_empty()
    };
    let (mut app, wid, _) = slot_glass(120, &slot);
    frame(&mut app, wid);
    assert!(bare(&app, wid), "no host");
    let (mut app, wid, session) = slot_glass(120, &slot);
    let host = supervise(&mut app, session);
    frame(&mut app, wid);
    assert!(!bare(&app, wid), "CONTROL: hosted, the banner");
    app.config.claude_footer = Some(crate::app_config::ClaudeFooterConfig {
        card: Some(false),
        ..Default::default()
    });
    frame(&mut app, wid);
    assert!(bare(&app, wid), "card = false");
    host.shutdown_and_join();
    let (mut app, wid, session) = slot_glass(120, &slot);
    let host = supervise(&mut app, session);
    frame(&mut app, wid);
    assert!(!bare(&app, wid), "CONTROL: hosted again, the banner");
    app.harness_policy = crate::harness_policy_cell::HarnessPolicyCell::new(
        crate::app_config::HarnessPolicy::read("[harness]\nenabled = false\n"),
    );
    frame(&mut app, wid);
    assert!(bare(&app, wid), "master switch off");
    host.shutdown_and_join();
}

/// A SETTLED ANNOUNCEMENT AT REST COSTS THE FRAME NOTHING: once read, streamed output and a
/// spinner under it cost what the bare footer's frame does; a change to its rows is read again.
#[test]
fn a_settled_announcement_at_rest_costs_the_frame_nothing() {
    let slot = measured_slot("claude-2.1.308-usage-dismissed-fullscreen.txt");
    let rows: String = slot
        .iter()
        .enumerate()
        .map(|(k, row)| format!("\x1b[{};1H{row}", SLOT_TOP + 1 + k))
        .collect();
    let feed_slot = format!("\x1b7{rows}\x1b8");
    let banner_on = |app: &App, wid: WindowId| {
        hits_of(app, wid, GlassLink::Announcement)
            .iter()
            .any(|h| h.3)
    };
    costs_the_floor("announcement", 12, |i, app, bare| {
        if i == 0 {
            feed_both(app, bare, feed_slot.as_bytes());
            return;
        }
        assert!(banner_on(&app.0, app.1), "pass {i}: the banner");
        let spinner = ['\u{2722}', '\u{2733}', '\u{2736}', '\u{273B}'][i % 4];
        let bytes = format!(
            "\x1b7\x1b[11;1H\u{23FA} streamed line {i}\x1b[K\
             \x1b[14;1H{spinner} Thinking\u{2026} ({i}s)\x1b[K\x1b8"
        );
        feed_both(app, bare, bytes.as_bytes());
    });
    let t0 = Instant::now();
    let (mut app, wid, session, host) = settled_glass(120, t0);
    let term = app.pool.get(session).unwrap().term.clone();
    let step = |app: &mut App, bytes: &[u8]| {
        crate::term_lock(&term).process(bytes);
        app.capture_words(wid, Instant::now());
        app.splice_tab_strip(wid);
        pass_cost(app, wid)
    };
    assert_eq!(
        step(&mut app, feed_slot.as_bytes()).notice_reads,
        1,
        "it comes"
    );
    assert!(banner_on(&app, wid));
    assert_eq!(step(&mut app, b"x").notice_reads, 0, "kept");
    let reworded = format!("\x1b7\x1b[{};1H  plain words\x1b[K\x1b8", SLOT_TOP + 1);
    assert_eq!(
        step(&mut app, reworded.as_bytes()).notice_reads,
        1,
        "CONTROL: a change to its rows is read again"
    );
    assert!(!banner_on(&app, wid), "CONTROL: and the banner goes");
    set_now(None);
    host.shutdown_and_join();
}

/// `whole`'s transcript grown by `k` rows, as the inline renderer draws it: the rows above the
/// composer moved up `k`, the top `k` gone off the screen, new output under them; the composer
/// stays.
fn grown(whole: &[String], k: usize) -> Vec<String> {
    let foot = whole.len() - 4;
    let mut rows = whole[k..foot].to_vec();
    rows.extend((0..k).map(|i| format!("\u{23FA} streamed line {i}")));
    rows.extend_from_slice(&whole[foot..]);
    rows
}

/// THE BANNER OUTLASTS THE CARD OVER THE SLOT'S ROWS: as the transcript grows and the header
/// scrolls up, the banner stays over the startup announcement while any of its rows stand,
/// through the step the card's last row leaves and on as the slot's own rows go off the top,
/// its hidden-count row last, and an idle frame keeps it; the step its last row goes, nothing is
/// covered. The terminal's text stays the vendor's throughout.
#[test]
fn the_banner_outlasts_the_card_over_the_slots_rows() {
    for (name, cols) in [
        ("claude-2.1.283-api-error-enotfound-80col-measured.txt", 80),
        ("claude-2.1.281-box-rm-auto-deny.txt", 120),
    ] {
        let slot = measured_slot(name);
        let span = slot.len();
        let whole = slot_screen(cols, &slot);
        let rung = ANNOUNCEMENT
            .words_within(cols - 1 - LINE_LEAD)
            .expect("a rung fits");
        let words = format!("{}{}", rung.lead, rung.link);
        let (mut app, wid, session) = slot_glass(cols, &slot);
        let host = supervise(&mut app, session);
        frame(&mut app, wid);
        let strip = strip_rows(&app, wid);
        for k in 0..=SLOT_TOP + span {
            let rows = grown(&whole, k);
            feed(&app, session, &rows);
            let cut = k.saturating_sub(SLOT_TOP);
            let top = SLOT_TOP.saturating_sub(k);
            assert_eq!(
                rows[top..top + span - cut.min(span)],
                slot[cut.min(span)..],
                "{name} {k}: CONTROL: the slot's rows on screen"
            );
            if k == SLOT_TOP - 1 {
                assert!(
                    rows[0].is_empty() && rows[top].starts_with('\u{258E}'),
                    "{name} {k}: CONTROL: the card's last row gone, the blank row under it on top"
                );
            }
            for pass in ["drawn", "idle"] {
                frame(&mut app, wid);
                let hits = hits_of(&app, wid, GlassLink::Announcement);
                if cut == span {
                    assert_eq!(
                        hits,
                        [],
                        "{name} {k} {pass}: the slot gone, nothing covered"
                    );
                    for (r, row) in rows.iter().enumerate().take(SLOT_TOP + span) {
                        assert_eq!(
                            glass_text(&app, wid, strip + r, 0..row.chars().count()),
                            *row,
                            "{name} {k} {pass}: row {r} the vendor's"
                        );
                    }
                    continue;
                }
                let row = strip + top;
                assert_eq!(
                    glass_text(&app, wid, row, LINE_LEAD..LINE_LEAD + rung.cells()),
                    words,
                    "{name} {k} {pass}: ALab's announcement over the slot's first row standing"
                );
                for r in 1..span - cut {
                    assert_eq!(
                        padded_text(&app, wid, row + r, 0..cols),
                        " ".repeat(cols),
                        "{name} {k} {pass}: slot row {} blanked",
                        cut + r
                    );
                }
                assert!(
                    hits.iter().any(|h| h.1 == row && h.3),
                    "{name} {k} {pass}: the link on the banner's row"
                );
                assert_eq!(
                    hits.iter()
                        .filter(|h| !h.3)
                        .map(|h| h.1)
                        .collect::<Vec<_>>(),
                    (row..row + span - cut).collect::<Vec<_>>(),
                    "{name} {k} {pass}: the slot's rows standing covered, no other"
                );
            }
            assert!(
                engine_rows(&app, session)
                    .iter()
                    .zip(&rows)
                    .all(|(drawn, fed)| drawn.starts_with(fed.as_str())),
                "{name} {k}: GLASS ONLY"
            );
        }
        host.shutdown_and_join();
    }
}

/// NEVER OVER ANOTHER ROW: once the card has scrolled off, a row where the slot's stood that
/// reads otherwise is never covered, and the slot stays the vendor's after; nor is it covered
/// with the master switch off.
#[test]
fn a_scrolled_slot_is_covered_only_while_its_rows_read_the_same() {
    let slot = measured_slot("claude-2.1.283-api-error-enotfound-80col-measured.txt");
    let whole = slot_screen(80, &slot);
    let bare = |app: &App, wid: WindowId, rows: &[String]| {
        let strip = strip_rows(app, wid);
        hits_of(app, wid, GlassLink::Announcement).is_empty()
            && (0..SLOT_TOP + slot.len())
                .all(|r| glass_text(app, wid, strip + r, 0..rows[r].chars().count()) == rows[r])
    };
    let window = || {
        let (mut app, wid, session) = slot_glass(80, &slot);
        let host = supervise(&mut app, session);
        frame(&mut app, wid);
        assert!(!bare(&app, wid, &whole), "CONTROL: the whole card's banner");
        (app, wid, session, host)
    };
    let (mut app, wid, session, host) = window();
    let mut other = grown(&whole, SLOT_TOP);
    assert_eq!(other[0], slot[0], "CONTROL: the slot's first row on top");
    feed(&app, session, &grown(&whole, SLOT_TOP - 1));
    frame(&mut app, wid);
    assert!(
        !bare(&app, wid, &grown(&whole, SLOT_TOP - 1)),
        "CONTROL: the card gone, the banner stays"
    );
    other[0] = "\u{258E} Claude Max gives you more usage at the same price.".to_string();
    feed(&app, session, &other);
    frame(&mut app, wid);
    assert!(
        bare(&app, wid, &other),
        "NEGATIVE CONTROL: another row where the slot's first stood"
    );
    let rows = grown(&whole, SLOT_TOP + 1);
    feed(&app, session, &rows);
    frame(&mut app, wid);
    assert!(
        bare(&app, wid, &rows),
        "a slot once gone stays the vendor's"
    );
    host.shutdown_and_join();
    let (mut app, wid, session, host) = window();
    let mut moved = grown(&whole, SLOT_TOP + 1);
    moved[1] = "\u{23FA} Auto mode lets Claude handle permission prompts.".to_string();
    feed(&app, session, &moved);
    frame(&mut app, wid);
    assert!(
        bare(&app, wid, &moved),
        "NEGATIVE CONTROL: a slot row below the top reads otherwise"
    );
    host.shutdown_and_join();
    let (mut app, wid, session, host) = window();
    app.harness_policy = crate::harness_policy_cell::HarnessPolicyCell::new(
        crate::app_config::HarnessPolicy::read("[harness]\nenabled = false\n"),
    );
    let rows = grown(&whole, SLOT_TOP);
    feed(&app, session, &rows);
    frame(&mut app, wid);
    assert!(bare(&app, wid, &rows), "master switch off");
    host.shutdown_and_join();
}
