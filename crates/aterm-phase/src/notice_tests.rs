// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the update notice's reader. The measured notices are the
//! recorded screens'; every other notice here is built from the anchors
//! (the drift canary holds the literal ones to the store binary), never
//! typed as prose.

use super::*;
use crate::anchors::anchor_text;
use crate::prompt::fixtures::{rows, screen};
use crate::turn::{launch_card_place, launch_card_scan, launch_card_scrolled, launch_card_words};

/// The width the built screens here are read at: wider than any of their
/// rows, as the captures they copy were.
const WIDE: usize = 120;

/// The notice's summary for a build of `changes` (`Updated to latest. Got
/// …`), as 2.1.304's `ty` composes it.
fn summary(changes: &str) -> String {
    format!("{}{changes}", anchor_text("notice.updated"))
}

/// A notice row at the Box's column.
fn boxed(text: &str) -> String {
    format!("{INDENT}{text}")
}

/// A card's three rows, as the 2.1.283 captures draw it at row 1 (a blank
/// row over it), then `under` row for row.
fn card_over(under: &[String]) -> Vec<String> {
    let mut r = rows(&[
        "",
        " \u{2590}\u{259B}\u{2588}\u{2588}\u{2588}\u{259B}\u{2588}   Claude Code v2.1.283",
        "\u{259D}\u{259C}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2580}  Haiku 4.5 \u{b7} Claude Max",
        " \u{259D}\u{259D}   \u{259D}\u{259D}   ~/aterm",
    ]);
    r.extend(under.iter().cloned());
    r
}

/// The measured notice's two rows (the 2.1.283 captures'), at the Box's
/// column.
fn measured_notice() -> [String; 2] {
    [
        boxed(&summary(
            " Got 33 features, 240 bugfixes, and 103 other changes.",
        )),
        boxed(anchor_text("notice.updated_details")),
    ]
}

/// The notice on `screen`, under its placed card, the pane [`WIDE`].
fn notice_on(screen: &[String]) -> Option<NoticePlace> {
    notice_at(screen, WIDE)
}

/// The notice on `screen`, under its placed card, the pane `cols` wide.
fn notice_at(screen: &[String], cols: usize) -> Option<NoticePlace> {
    let card = launch_card_place(screen).expect("a placed card");
    update_notice_scan(screen, &card, cols).place
}

/// The pane's width a capture's header names (`… headless aterm (50x40,
/// …`), `0` where it names none: no row of such a screen is read as filled
/// to the pane's edge, so none takes a hard break.
fn header_cols(text: &str) -> usize {
    let head = text.lines().next().unwrap_or_default();
    head.split('(')
        .filter_map(|part| {
            let (w, rest) = part.split_once('x')?;
            let h: String = rest.chars().take_while(char::is_ascii_digit).collect();
            (!w.is_empty() && w.bytes().all(|b| b.is_ascii_digit()) && !h.is_empty())
                .then(|| w.parse().ok())
                .flatten()
        })
        .next()
        .unwrap_or(0)
}

/// Every screen the reader is held to, as `(its name, its rows, its pane's
/// width)`: this crate's fixtures, and the supervisor's 2.1.282 captures
/// that draw the notice (`aterm-agent`'s `cap-decline-*`).
fn corpus() -> Vec<(String, Vec<String>, usize)> {
    let mut screens: Vec<(String, Vec<String>, usize)> = crate::text::fixture_files()
        .into_iter()
        .map(|(name, text)| {
            let cols = header_cols(&text);
            (name, screen(&text), cols)
        })
        .collect();
    let agent = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../aterm-agent/src/supervise/policy/fixtures");
    let mut paths: Vec<_> = std::fs::read_dir(&agent)
        .expect("aterm-agent's policy fixtures")
        .map(|e| e.expect("a fixture").path())
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("cap-decline-"))
        })
        .collect();
    paths.sort();
    for path in paths {
        let text = std::fs::read_to_string(&path).expect("a fixture");
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let cols = header_cols(&text);
        screens.push((name, screen(&text), cols));
    }
    screens
}

/// Where each recorded notice sits, `(its screen's name, top, rows, end)`:
/// the three 2.1.283 screens that draw it, the supervisor's four 2.1.282
/// `cap-decline-*` screens, the MEASURED 2.1.304 lab captures — the five
/// notice captures (fullscreen at 120, 50 and 44 columns: the summary at row
/// 5 under a row of top air, wrapped onto two rows at 50 and 44, the details
/// row wrapped too at 44; inline under the workspace card at 120 and the
/// plain one at 50: the summary at row 4), four later screens of the same
/// launch (the notice stays under the card over the conversation) and the
/// seven under the NARROW four-row card (`*-narrow-*`: the summary a row
/// lower, at 6 fullscreen and 5 inline; at 34, 33 and 31 columns the summary
/// on three rows and the details row's link broken inside itself, filling
/// the row to the pane's edge) — and the one SYNTHETIC screen no capture
/// covers (its provenance line says what was measured): a one-shot notice
/// between the card and the summary.
const PLACED: [(&str, usize, usize, usize); 24] = [
    ("claude-2.1.283-effort-switch.txt", 5, 2, 74),
    ("claude-2.1.283-model-switch.txt", 5, 2, 74),
    ("claude-2.1.283-model-switch-hook.txt", 5, 2, 74),
    ("cap-decline-1-box.txt", 5, 2, 73),
    ("cap-decline-2-focus-no.txt", 5, 2, 73),
    ("cap-decline-3-amend-open.txt", 5, 2, 73),
    ("cap-decline-4-amend-typed.txt", 5, 2, 73),
    (
        "claude-2.1.304-update-notice-fullscreen-120col.txt",
        5,
        2,
        74,
    ),
    (
        "claude-2.1.304-update-notice-fullscreen-50col.txt",
        5,
        3,
        47,
    ),
    (
        "claude-2.1.304-update-notice-fullscreen-44col.txt",
        5,
        4,
        41,
    ),
    (
        "claude-2.1.304-update-notice-inline-workspace-120col.txt",
        4,
        2,
        74,
    ),
    ("claude-2.1.304-update-notice-inline-50col.txt", 4, 3, 47),
    (
        "claude-2.1.304-update-notice-narrow-workspace-fullscreen-50col.txt",
        6,
        3,
        47,
    ),
    (
        "claude-2.1.304-update-notice-narrow-workspace-fullscreen-34col.txt",
        6,
        5,
        34,
    ),
    (
        "claude-2.1.304-update-notice-narrow-workspace-fullscreen-33col.txt",
        6,
        5,
        33,
    ),
    (
        "claude-2.1.304-update-notice-narrow-workspace-inline-50col.txt",
        5,
        3,
        47,
    ),
    (
        "claude-2.1.304-update-notice-narrow-workspace-inline-33col.txt",
        5,
        5,
        33,
    ),
    (
        "claude-2.1.304-update-notice-narrow-xhigh-fullscreen-50col.txt",
        6,
        3,
        47,
    ),
    (
        "claude-2.1.304-update-notice-narrow-xhigh-fullscreen-31col.txt",
        6,
        5,
        31,
    ),
    ("claude-2.1.304-top-effort-slider-120col.txt", 5, 2, 74),
    ("claude-2.1.304-top-model-picker-120col.txt", 5, 2, 74),
    ("claude-2.1.304-top-off-120col.txt", 5, 2, 74),
    (
        "claude-2.1.304-top-refused-unavailable-120col.txt",
        5,
        2,
        74,
    ),
    ("claude-2.1.304-update-notice-oneshot-120col.txt", 7, 2, 71),
];

/// The recorded screens that show the notice's summary with no card placed
/// over it: the conversation has scrolled the card's top row off the screen
/// (the lab's shell-mode capture, the notice at rows 0 and 1, the card gone
/// whole; the `*-scrolled-*` captures, its model and working-directory rows
/// still at the screen's top at 29 rows, the working directory's alone at
/// 28, none at 27 — the notice there is anchored only on a card a caller
/// kept, [`a_scrolled_card_anchors_its_notice`]), or the pane is so narrow
/// the vendor wraps the banner itself (`Claude Code` / `v2.1.304`, at 32
/// columns with the wand and 30 without it), a card nothing places.
const UNPLACED: [&str; 6] = [
    "claude-2.1.304-top-shell-mode-120col.txt",
    "claude-2.1.304-update-notice-narrow-workspace-fullscreen-32col.txt",
    "claude-2.1.304-update-notice-narrow-xhigh-fullscreen-30col.txt",
    "claude-2.1.304-update-notice-scrolled-27rows-fullscreen-120col.txt",
    "claude-2.1.304-update-notice-scrolled-28rows-fullscreen-120col.txt",
    "claude-2.1.304-update-notice-scrolled-29rows-fullscreen-120col.txt",
];

/// THE NOTICE'S PLACE, over the whole corpus, each screen read at its
/// pane's width: each of [`PLACED`] where it was recorded, its text at
/// column 2 — the 44-column capture's wrapped details row and the narrow
/// card's hard-broken link among them — and every other screen with a
/// placed card has none. NONE ELSEWHERE: every screen with a row at the
/// Box's column that opens with the summary is one of [`PLACED`], or one of
/// [`UNPLACED`], with no card placed.
#[test]
fn every_recorded_update_notice_is_placed_under_its_card() {
    let updated = format!("{INDENT}{}", anchor_text("notice.updated"));
    let (mut placed, mut bare, mut unplaced) = (0, 0, 0);
    for (name, screen, cols) in corpus() {
        let shown = screen.iter().any(|row| row.starts_with(&updated));
        let listed = PLACED.iter().any(|(n, ..)| *n == name);
        let Some(card) = launch_card_place(&screen) else {
            assert!(
                !shown || UNPLACED.contains(&name.as_str()),
                "{name}: a notice with no card over it"
            );
            assert!(!listed, "{name}: no card to read the notice under");
            unplaced += usize::from(shown);
            continue;
        };
        assert_eq!(
            shown, listed,
            "{name}: the summary shown is the summary placed"
        );
        let found = update_notice_scan(&screen, &card, cols).place;
        let want = PLACED
            .iter()
            .find(|(n, ..)| *n == name)
            .map(|&(_, top, rows, end)| NoticePlace {
                top,
                rows,
                text_col: 2,
                end,
            });
        assert_eq!(found, want, "{name}");
        if want.is_some() {
            placed += 1;
        } else {
            bare += 1;
        }
    }
    assert!(
        placed == PLACED.len() && bare >= 140 && unplaced == UNPLACED.len(),
        "the corpus must exercise the reader: {placed} placed, {bare} bare, {unplaced} unplaced"
    );
}

/// THE HARD BREAK, MEASURED. On the narrow card's captures at 34, 33 and 31
/// columns the details row's link (33 cells) is longer than the Box (the
/// pane less its padding), and the vendor breaks it inside itself on a row
/// filled to the pane's edge: read at the pane's own width, the notice is
/// the vendor's whole. NEGATIVE CONTROLS, each leaving it bare: the same
/// rows read on a pane one column wider (the broken row then stops short
/// of the edge — a break wrap-ansi never makes), a link broken short of the
/// edge, and a break inside a word that would fit on a row of its own.
#[test]
fn a_link_broken_at_the_panes_edge_is_the_vendors_wrap() {
    for (name, cols) in [
        (
            "claude-2.1.304-update-notice-narrow-workspace-fullscreen-34col.txt",
            34,
        ),
        (
            "claude-2.1.304-update-notice-narrow-workspace-fullscreen-33col.txt",
            33,
        ),
        (
            "claude-2.1.304-update-notice-narrow-xhigh-fullscreen-31col.txt",
            31,
        ),
    ] {
        let (_, screen, header) = corpus()
            .into_iter()
            .find(|(n, ..)| n == name)
            .expect("the capture");
        assert_eq!(header, cols, "{name}: its header's width");
        let place = notice_at(&screen, cols).expect("the vendor's notice");
        assert_eq!((place.rows, place.end), (5, cols), "{name}");
        assert_eq!(notice_at(&screen, cols + 1), None, "{name}: one wider");
    }
    let details = anchor_text("notice.updated_details");
    let (link, words) = details.split_once(" for").expect("the link");
    let cols = INDENT.len() + link.len() - 1;
    let broken = |at: usize, cols: usize| {
        let screen = card_over(&[
            String::new(),
            boxed(anchor_text("notice.updated")),
            boxed(&link[..at]),
            boxed(&format!("{} for{words}", &link[at..])),
        ]);
        notice_at(&screen, cols)
    };
    // The vendor's: the link filling the Box, one cell over it on the next.
    assert_eq!(broken(link.len() - 1, cols).map(|p| p.rows), Some(3));
    // Broken short of the pane's edge.
    assert_eq!(broken(link.len() - 2, cols), None);
    // A link that fits the Box is wrapped whole, never broken.
    assert_eq!(broken(link.len() - 1, cols + 1), None);
    assert_eq!(broken(link.len() - 1, 0), None);
}

/// THE VENDOR'S SHAPE, OR NOTHING. A bare `Updated to latest.`, a summary
/// wrapped onto two rows and a details row word-wrapped onto two (`…changelog
/// for` / `details`, as a narrow pane wraps it) are read; NEGATIVE CONTROLS,
/// each of which leaves the vendor's notice bare: the details row reworded,
/// the details row broken inside a word, its wrapped tail reworded or off
/// the Box's column, the details row spread over three rows, a wrap cut by
/// the screen's bottom edge, the summary quoted in a person's message (`❯`),
/// the notice under something said (`⏺`: the walk stops there), the notice
/// past the reach, a notice cut by the screen's bottom edge, the summary off
/// the Box's column, and a summary with no details row under it.
#[test]
fn a_notice_is_refused_unless_it_is_the_vendors_shape() {
    let details = anchor_text("notice.updated_details");
    let [head, tail] = measured_notice();
    // The measured shape, one row under the card.
    let measured = card_over(&[String::new(), head.clone(), tail.clone()]);
    assert_eq!(
        notice_on(&measured),
        Some(NoticePlace {
            top: 5,
            rows: 2,
            text_col: 2,
            end: 74
        })
    );
    // A bare summary.
    let bare = card_over(&[
        String::new(),
        boxed(anchor_text("notice.updated")),
        tail.clone(),
    ]);
    assert_eq!(
        notice_on(&bare),
        Some(NoticePlace {
            top: 5,
            rows: 2,
            text_col: 2,
            end: 2 + details.len()
        })
    );
    // A summary wrapped onto two rows at the Box's column: the notice ends
    // past its widest row, the summary's first.
    let first = boxed(&summary(" Got 33 features, 240 bugfixes, and 103"));
    let wrapped = card_over(&[
        String::new(),
        first.clone(),
        boxed("other changes."),
        tail.clone(),
    ]);
    assert!(first.len() > tail.len());
    assert_eq!(
        notice_on(&wrapped),
        Some(NoticePlace {
            top: 5,
            rows: 3,
            text_col: 2,
            end: first.len()
        })
    );
    // Right at the card's last row, and trailing blanks after each row.
    let tight = card_over(&[format!("{head}   "), format!("{tail}  ")]);
    assert_eq!(notice_on(&tight).map(|p| (p.top, p.end)), Some((4, 74)));
    let (link, words) = details
        .split_once(" for")
        .expect("the details row's link and its words");
    let words = crate::text::trim(words);
    // The details row word-wrapped onto two rows under a wrapped summary, as
    // the 44-column capture draws it: the notice spans all four.
    let narrow = card_over(&[
        String::new(),
        first.clone(),
        boxed("other changes."),
        boxed(&format!("{link} for")),
        boxed(words),
        String::new(),
    ]);
    assert_eq!(
        notice_on(&narrow),
        Some(NoticePlace {
            top: 5,
            rows: 4,
            text_col: 2,
            end: first.len()
        })
    );
    // Wrapped after the link, its widest row the link's.
    let after_link = card_over(&[
        String::new(),
        boxed(anchor_text("notice.updated")),
        boxed(link),
        boxed(&format!("for {words}")),
    ]);
    assert_eq!(
        notice_on(&after_link),
        Some(NoticePlace {
            top: 5,
            rows: 3,
            text_col: 2,
            end: 2 + link.len()
        })
    );
    let (link_head, link_tail) = link.split_at(link.len() - 3);
    for (why, screen) in [
        (
            "the details row reworded",
            card_over(&[
                String::new(),
                head.clone(),
                boxed(&format!("{link} for more")),
            ]),
        ),
        (
            "the details row broken inside a word",
            card_over(&[
                String::new(),
                head.clone(),
                boxed(link_head),
                boxed(&format!("{link_tail} for {words}")),
            ]),
        ),
        (
            "the wrapped tail reworded",
            card_over(&[
                String::new(),
                head.clone(),
                boxed(&format!("{link} for")),
                boxed("more"),
            ]),
        ),
        (
            "the wrapped tail off the Box's column",
            card_over(&[
                String::new(),
                head.clone(),
                boxed(&format!("{link} for")),
                format!(" {}", boxed(words)),
            ]),
        ),
        (
            "the details row over three rows",
            card_over(&[
                String::new(),
                head.clone(),
                boxed(link),
                boxed("for"),
                boxed(words),
            ]),
        ),
        (
            "a wrap cut by the bottom edge",
            card_over(&[String::new(), head.clone(), boxed(&format!("{link} for"))]),
        ),
        (
            "the summary quoted by a person",
            card_over(&[
                String::new(),
                format!("\u{276F} {}", summary(" Got 1 feature.")),
                tail.clone(),
            ]),
        ),
        (
            "the notice under something said",
            card_over(&[
                String::new(),
                "\u{23FA} agents-md: loaded".into(),
                head.clone(),
                tail.clone(),
            ]),
        ),
        (
            "the notice past the reach",
            card_over(
                &std::iter::repeat_n(String::new(), NOTICE_REACH)
                    .chain([head.clone(), tail.clone()])
                    .collect::<Vec<_>>(),
            ),
        ),
        (
            "the notice cut by the bottom edge",
            card_over(&[String::new(), head.clone()]),
        ),
        (
            "the summary off the Box's column",
            card_over(&[String::new(), format!(" {head}"), tail.clone()]),
        ),
        (
            "no details row",
            card_over(&[String::new(), head.clone(), String::new(), tail.clone()]),
        ),
        (
            "the details row under a person's message",
            card_over(&[
                String::new(),
                head.clone(),
                "\u{276F} hi".into(),
                tail.clone(),
            ]),
        ),
    ] {
        assert_eq!(notice_on(&screen), None, "{why}: {screen:#?}");
    }
    // Garbage never panics.
    let card = launch_card_place(&measured).expect("the card");
    for screen in [Vec::new(), rows(&[""]), measured[..2].to_vec()] {
        for cols in [0, 1, WIDE, usize::MAX] {
            let _ = update_notice_scan(&screen, &card, cols);
        }
    }
    let far = CardPlace {
        top: usize::MAX,
        ..card
    };
    assert_eq!(update_notice_scan(&measured, &far, WIDE).place, None);
}

/// A READING KEPT WITHOUT A SCAN IS THE SCAN'S (`update_notice_unmoved`):
/// over every recorded screen with a placed card, each row in turn
/// rewritten — blanked, a summary's head, the details row, a person's
/// words, a streamed `⏺` row, a wrapped continuation — whenever the check
/// keeps the old reading, scanning the new rows under the same card gives
/// exactly it. Not vacuous: most rewrites keep the reading (rows above the
/// card's foot and below the scan's last row, a stop row streamed over),
/// and a rewrite inside what the scan read does not. CONTROL: dropping the
/// stop row's glyph test, or the bound at the scan's last row, keeps
/// readings the scan disagrees with (mutation-checked).
#[test]
fn a_kept_notice_scan_is_the_scans() {
    let [head, tail] = measured_notice();
    let rewrites: [String; 6] = [
        String::new(),
        head,
        tail,
        "\u{276F} what changed?".into(),
        "\u{23FA} streamed".into(),
        boxed("other changes."),
    ];
    let (mut kept, mut moved, mut screens, mut stopped) = (0, 0, 0, 0);
    for (name, before, cols) in corpus() {
        let Some(card) = launch_card_place(&before) else {
            continue;
        };
        screens += 1;
        let reading = update_notice_scan(&before, &card, cols);
        for at in 0..before.len() {
            for rewrite in &rewrites {
                let mut after = before.clone();
                after[at].clone_from(rewrite);
                let changed: Vec<usize> = (0..after.len())
                    .filter(|&i| after[i] != before[i])
                    .collect();
                if update_notice_unmoved(&after, &reading, &changed) {
                    assert_eq!(
                        update_notice_scan(&after, &card, cols),
                        reading,
                        "{name}: row {at} as {rewrite:?}"
                    );
                    kept += 1;
                    if reading.stop && Some(at) == reading.read_to && !changed.is_empty() {
                        stopped += 1;
                    }
                } else {
                    moved += 1;
                }
            }
        }
    }
    assert!(
        screens >= 140,
        "the corpus must exercise the check: {screens}"
    );
    assert!(
        kept > moved && moved > 0 && stopped > 0,
        "most rewrites keep the reading: {kept} kept, {moved} moved, {stopped} over a stop row"
    );
    // No row read: nothing moves it.
    let card = launch_card_place(&card_over(&[])).expect("the card");
    let edge = update_notice_scan(&card_over(&[]), &card, WIDE);
    assert_eq!((edge.read_to, edge.place), (None, None));
    assert!(update_notice_unmoved(&card_over(&[]), &edge, &[0, 3]));
    // Garbage indices never panic.
    let _ = update_notice_unmoved(&[], &edge, &[usize::MAX]);
}

/// One recorded screen of the corpus: its rows and its pane's width.
fn recorded(name: &str) -> (Vec<String>, usize) {
    corpus()
        .into_iter()
        .find(|(n, ..)| n == name)
        .map(|(_, rows, cols)| (rows, cols))
        .unwrap_or_else(|| panic!("{name}: no such capture"))
}

/// The `*-scrolled-*` captures: the conversation scrolled the card's top
/// off, at 29, 28 and 27 rows (the card's last two rows left, its last one,
/// none).
fn scrolled(rows: usize) -> (Vec<String>, usize) {
    recorded(&format!(
        "claude-2.1.304-update-notice-scrolled-{rows}rows-fullscreen-120col.txt"
    ))
}

/// The card the 29-row capture cut, WHOLE — DERIVED: its banner row put back
/// over its model row, as the pane one row taller drew it (the fullscreen
/// renderer keeps the transcript's foot on the composer, so a row more
/// shows one more at the top) — placed, with its words.
fn whole_scrolled_card() -> (Vec<String>, CardPlace, Vec<String>) {
    let (cut, _) = scrolled(29);
    let banner = format!(
        " \u{2590}\u{259B}\u{2588}\u{2588}\u{2588}\u{259B}\u{2588}   {}2.1.304",
        anchor_text("launch.banner")
    );
    let mut whole = vec![banner];
    whole.extend(cut);
    let place = launch_card_place(&whole).expect("the whole card");
    let words = launch_card_words(&whole, &place)
        .expect("its words")
        .as_slice()
        .iter()
        .map(|w| (*w).to_owned())
        .collect();
    (whole, place, words)
}

/// THE NOTICE UNDER A SCROLLED CARD: on the captures where the conversation
/// scrolled the card's top off the screen, the card a caller kept — placed
/// whole a row before — is known again by its last rows at the screen's top
/// (`launch_card_scrolled`): its model and working-directory rows at 29
/// rows, its working directory's alone at 28 (from the whole card, and from
/// the scrolled one kept in turn), and the notice is read under them,
/// whole. With none of the card left (27 rows) nothing is: the vendor's
/// notice goes bare. NEGATIVE CONTROLS, each reading nothing: a reply of
/// Claude's quoting the notice with no card over it, the card's rows quoted
/// at a message's indent over the quoted notice, another card's last rows
/// (another working directory), the kept card scrolled back whole (a banner
/// on the screen), the card's rows with no blank row under them, and a kept
/// place at another text column.
#[test]
fn a_scrolled_card_anchors_its_notice() {
    let (whole, kept, words) = whole_scrolled_card();
    let words: Vec<&str> = words.iter().map(String::as_str).collect();
    assert_eq!((kept.top, kept.rows, kept.text_col), (0, 3, 11));
    let (at29, cols) = scrolled(29);
    let scan = launch_card_scan(&at29);
    assert_eq!(scan, (None, None), "no banner left on the screen");
    let two = launch_card_scrolled(&at29, scan, &kept, &words).expect("its last two rows");
    assert_eq!((two.top, two.rows, two.text_col), (0, 2, 11));
    assert_eq!(
        update_notice_scan(&at29, &two, cols).place,
        Some(NoticePlace {
            top: 3,
            rows: 2,
            text_col: 2,
            end: 74
        })
    );
    let (at28, _) = scrolled(28);
    for (kept, words) in [(&kept, &words[..]), (&two, &words[1..])] {
        let one = launch_card_scrolled(&at28, launch_card_scan(&at28), kept, words)
            .expect("its last row");
        assert_eq!((one.top, one.rows), (0, 1));
        assert_eq!(
            update_notice_scan(&at28, &one, cols)
                .place
                .map(|p| (p.top, p.rows)),
            Some((2, 2))
        );
    }
    let (at27, _) = scrolled(27);
    assert!(
        at27[1].starts_with(&boxed(anchor_text("notice.updated"))),
        "CONTROL: the notice is on the 27-row screen: {at27:#?}"
    );
    assert_eq!(
        launch_card_scrolled(&at27, launch_card_scan(&at27), &kept, &words),
        None,
        "no row of the card left"
    );
    let replaced = |top: &[String]| {
        let mut r = top.to_vec();
        r.extend(at29[2..].iter().cloned());
        r
    };
    let mut other = at29[..2].to_vec();
    other[1] = other[1].replace("at.r3cj", "at.x0x0");
    let mut no_margin = at29.clone();
    no_margin[2].clone_from(&at29[1]);
    for (why, screen) in [
        (
            "Claude's reply quoting the notice",
            replaced(&rows(&["\u{23FA} The notice read:", ""])),
        ),
        (
            "the card's rows quoted at a message's indent",
            replaced(&[format!("  {}", at29[0]), format!("  {}", at29[1])]),
        ),
        ("another card's last rows", replaced(&other)),
        ("the card scrolled back", whole.clone()),
        ("no blank row under the card's rows", no_margin),
    ] {
        assert_eq!(
            launch_card_scrolled(&screen, launch_card_scan(&screen), &kept, &words),
            None,
            "{why}: {screen:#?}"
        );
    }
    let off = CardPlace {
        text_col: 13,
        ..kept
    };
    assert_eq!(
        launch_card_scrolled(&at29, scan, &off, &words),
        None,
        "another text column"
    );
}

/// A SCROLLED CARD KEPT WITHOUT A READ IS THE READ'S
/// (`launch_card_scrolled_unmoved`): on the 29-row capture, each row in
/// turn rewritten — blanked, a banner row, a person's words, another card
/// row — whenever the check keeps the scrolled card, reading the rows again
/// finds exactly it. Not vacuous: rewrites under the card's rows and the
/// blank row under them keep it, and rewrites of those rows, or a banner
/// drawn anywhere, do not.
#[test]
fn a_kept_scrolled_card_is_the_reads() {
    let (_, kept, words) = whole_scrolled_card();
    let words: Vec<&str> = words.iter().map(String::as_str).collect();
    let (before, _) = scrolled(29);
    let cut = launch_card_scrolled(&before, launch_card_scan(&before), &kept, &words)
        .expect("the scrolled card");
    let banner = format!(
        " \u{2590}\u{259B}\u{2588}\u{2588}\u{2588}\u{259B}\u{2588}   {}2.1.305",
        anchor_text("launch.banner")
    );
    let rewrites = [
        String::new(),
        banner,
        "\u{276F} what changed?".into(),
        before[1].clone(),
    ];
    let (mut held, mut moved) = (0, 0);
    for at in 0..before.len() {
        for rewrite in &rewrites {
            let mut after = before.clone();
            after[at].clone_from(rewrite);
            let changed: Vec<usize> = (0..after.len())
                .filter(|&i| after[i] != before[i])
                .collect();
            if crate::turn::launch_card_scrolled_unmoved(&after, &cut, &changed) {
                assert_eq!(
                    launch_card_scrolled(&after, launch_card_scan(&after), &kept, &words),
                    Some(cut),
                    "row {at} as {rewrite:?}"
                );
                held += 1;
            } else {
                moved += 1;
            }
        }
    }
    assert!(held > moved && moved > 0, "{held} held, {moved} moved");
}
