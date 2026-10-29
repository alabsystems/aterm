// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the startup announcement's reader.

use super::*;
use crate::prompt::fixtures::{rows, screen};
use crate::turn::launch_card_place;

fn corpus() -> Vec<(String, Vec<String>)> {
    crate::text::fixture_files()
        .into_iter()
        .map(|(name, text)| (name, screen(&text)))
        .collect()
}

fn recorded(name: &str) -> Vec<String> {
    corpus()
        .into_iter()
        .find(|(n, _)| n == name)
        .map(|(_, rows)| rows)
        .unwrap_or_else(|| panic!("{name}: no such capture"))
}

fn card_over(under: &[String]) -> Vec<String> {
    let mut r = rows(&[
        "",
        " \u{2590}\u{259B}\u{2588}\u{2588}\u{2588}\u{259B}\u{2588}   Claude Code v2.1.311",
        "\u{259D}\u{259C}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2580}  Opus 5.5 \u{b7} Claude Max",
        " \u{259D}\u{259D}   \u{259D}\u{259D}   ~/aterm",
    ]);
    r.extend(under.iter().cloned());
    r
}

fn barred(text: &str) -> String {
    format!("{BAR} {text}")
}

/// A row at the indent: a notice's row with its bar off, or a warning's hint.
fn bare(text: &str) -> String {
    format!("{INDENT}{text}")
}

/// 2.1.280's launch news, drawn with its accent bar off.
const NEWS: &str = "Get to finished work sooner with Opus 5.5. Switch anytime with /model.";

/// A warning over the slot (a status glyph at column 0).
const WARNING: &str =
    "\u{26A0} Prompt caching off (DISABLE_PROMPT_CACHING), requests will be slower";

fn hidden_row(n: usize) -> String {
    let s = if n == 1 { "" } else { "s" };
    format!(
        "{INDENT}{n}{}{s}{}",
        crate::anchor!("notice.more"),
        crate::anchor!("notice.hidden")
    )
}

/// The slot the reader finds on `screen`, read under its card and any update notice on it.
fn slot_on(screen: &[String]) -> Option<AnnouncementPlace> {
    let card = launch_card_place(screen).expect("a placed card");
    let notice = crate::notice::update_notice_scan(screen, &card, 120).place;
    announcement_scan(screen, &card, notice.as_ref()).place
}

const fn place(top: usize, rows: usize, end: usize, hidden: Option<usize>) -> AnnouncementPlace {
    AnnouncementPlace {
        top,
        rows,
        text_col: 2,
        end,
        hidden,
    }
}

/// 2.1.280–2.1.283's launch news with its accent bar off, one row at the indent: `  Get to
/// finished work sooner with Opus 5.5. Switch anytime with /model.`
const fn news(top: usize, hidden: Option<usize>) -> AnnouncementPlace {
    place(top, 1, 72, hidden)
}

/// Every capture whose card the reader places, and the slot it reads under it: the auto-mode
/// notice wrapped at 144 and 80 columns, 2.1.308's debug-mode notice, the launch news with its
/// bar off (with its hidden count, and under the update notice), and 2.1.312's measured
/// notices: news with its bar on and off, under a warning, three warnings or the emergency tip
/// (each left showing: the news with its bar off is read with the tip over it, which the frame's
/// inks leave), with and without a hidden count, after the first turn, and the tip alone.
/// 2.1.312's warning alone and its launch with no notices place none.
const PLACED: [(&str, AnnouncementPlace); 58] = [
    (
        "claude-2.1.283-api-error-enotfound-measured.txt",
        place(7, 4, 139, None),
    ),
    (
        "claude-2.1.283-api-error-enotfound-80col-measured.txt",
        place(4, 5, 78, None),
    ),
    (
        "claude-2.1.284-footer-auto-effort-hint-measured.txt",
        place(4, 4, 139, None),
    ),
    (
        "claude-2.1.284-fast-refused-busy-under-composer-measured.txt",
        place(7, 4, 139, None),
    ),
    (
        "claude-2.1.308-usage-dismissed-fullscreen.txt",
        place(5, 2, 33, None),
    ),
    (
        "claude-2.1.308-usage-menu-fullscreen.txt",
        place(5, 2, 33, None),
    ),
    (
        "claude-2.1.308-usage-dismissed-inline.txt",
        place(4, 2, 33, None),
    ),
    ("claude-2.1.280-box-bash-touch-blink.txt", news(5, Some(6))),
    ("claude-2.1.280-box-bash-touch.txt", news(5, Some(6))),
    ("claude-2.1.282-question-afk.txt", news(5, None)),
    ("claude-2.1.282-question-chat-focused.txt", news(5, None)),
    ("claude-2.1.282-question-declined-cancel.txt", news(5, None)),
    ("claude-2.1.282-question-declined-esc.txt", news(5, None)),
    ("claude-2.1.282-question-deferred.txt", news(5, None)),
    (
        "claude-2.1.282-question-do-you-want-live-geometry.txt",
        news(5, None),
    ),
    (
        "claude-2.1.282-question-free-text-focused.txt",
        news(5, None),
    ),
    (
        "claude-2.1.282-question-free-text-typed-away.txt",
        news(5, None),
    ),
    ("claude-2.1.282-question-free-text-typed.txt", news(5, None)),
    (
        "claude-2.1.282-question-long-label-180col.txt",
        news(5, None),
    ),
    (
        "claude-2.1.282-question-long-label-80col.txt",
        news(5, None),
    ),
    (
        "claude-2.1.282-question-multiselect-alone.txt",
        news(5, None),
    ),
    (
        "claude-2.1.282-question-multiselect-checked.txt",
        news(5, None),
    ),
    (
        "claude-2.1.282-question-multiselect-next.txt",
        news(5, None),
    ),
    (
        "claude-2.1.282-question-multiselect-typed.txt",
        news(5, None),
    ),
    ("claude-2.1.282-question-multiselect.txt", news(5, None)),
    (
        "claude-2.1.282-question-no-recommendation.txt",
        news(5, None),
    ),
    ("claude-2.1.282-question-one.txt", news(5, None)),
    ("claude-2.1.282-question-panel-waiting.txt", news(5, None)),
    ("claude-2.1.282-question-plugin-notice.txt", news(5, None)),
    (
        "claude-2.1.282-question-preview-long-label.txt",
        news(5, None),
    ),
    (
        "claude-2.1.282-question-preview-revisited.txt",
        news(5, None),
    ),
    ("claude-2.1.282-question-preview.txt", news(5, None)),
    (
        "claude-2.1.282-question-recommended-second.txt",
        news(5, None),
    ),
    (
        "claude-2.1.282-question-review-unanswered.txt",
        news(5, None),
    ),
    ("claude-2.1.282-question-review.txt", news(5, None)),
    ("claude-2.1.282-question-tabs-fourth.txt", news(5, None)),
    ("claude-2.1.282-question-tabs-incident.txt", news(5, None)),
    ("claude-2.1.282-question-tabs-second.txt", news(5, None)),
    ("claude-2.1.282-question-tabs-third.txt", news(5, None)),
    (
        "claude-2.1.283-box-rm-subagent-fresh-pane.txt",
        news(5, None),
    ),
    ("claude-2.1.283-effort-switch.txt", news(8, None)),
    ("claude-2.1.283-model-switch-hook.txt", news(8, None)),
    ("claude-2.1.283-model-switch-inline.txt", news(5, None)),
    ("claude-2.1.283-model-switch.txt", news(8, None)),
    (
        "claude-2.1.312-startup-news-bar-off-60col-measured.txt",
        place(4, 4, 57, None),
    ),
    (
        "claude-2.1.312-startup-news-bar-off-hidden-measured.txt",
        place(4, 4, 92, Some(8)),
    ),
    (
        "claude-2.1.312-startup-news-bar-off-measured.txt",
        place(4, 4, 92, None),
    ),
    (
        "claude-2.1.312-startup-news-bar-off-two-entries-measured.txt",
        place(4, 2, 37, None),
    ),
    (
        "claude-2.1.312-startup-three-warnings-over-news-measured.txt",
        place(11, 3, 100, Some(14)),
    ),
    (
        "claude-2.1.312-startup-tip-alone-measured.txt",
        place(4, 1, 88, None),
    ),
    (
        "claude-2.1.312-startup-tip-over-news-bar-off-measured.txt",
        place(4, 6, 92, Some(10)),
    ),
    (
        "claude-2.1.312-startup-tip-over-news-measured.txt",
        place(6, 3, 100, None),
    ),
    (
        "claude-2.1.312-startup-warning-over-company-hidden-measured.txt",
        place(7, 1, 91, Some(8)),
    ),
    (
        "claude-2.1.312-startup-warning-over-news-60col-measured.txt",
        place(7, 4, 56, None),
    ),
    (
        "claude-2.1.312-startup-warning-over-news-bar-off-60col-measured.txt",
        place(7, 4, 57, Some(11)),
    ),
    (
        "claude-2.1.312-startup-warning-over-news-bar-off-after-turn-measured.txt",
        place(7, 4, 92, Some(11)),
    ),
    (
        "claude-2.1.312-startup-warning-over-news-bar-off-measured.txt",
        place(7, 4, 92, Some(11)),
    ),
    (
        "claude-2.1.312-startup-warning-over-news-measured.txt",
        place(7, 3, 100, None),
    ),
];

#[test]
fn every_recorded_announcement_is_placed_under_its_card() {
    let (mut placed, mut bare) = (Vec::new(), 0);
    for (name, screen) in corpus() {
        let Some(card) = launch_card_place(&screen) else {
            continue;
        };
        let notice = crate::notice::update_notice_scan(&screen, &card, 120).place;
        let found = announcement_scan(&screen, &card, notice.as_ref()).place;
        let want = PLACED.iter().find(|(n, _)| *n == name).map(|&(_, p)| p);
        assert_eq!(found, want, "{name}");
        if want.is_some() {
            placed.push(name);
        } else {
            bare += 1;
        }
    }
    let unplaced: Vec<&str> = PLACED
        .iter()
        .map(|&(n, _)| n)
        .filter(|n| !placed.iter().any(|p| p == n))
        .collect();
    assert!(
        unplaced.is_empty() && bare >= 140,
        "the corpus must exercise the reader: {} placed, {bare} bare, no card placed in {unplaced:?}",
        placed.len()
    );
}

#[test]
fn a_slot_under_a_scrolled_card_keeps_its_hidden_count() {
    let screen = recorded("claude-2.1.281-box-rm-auto-deny.txt");
    assert_eq!(
        launch_card_place(&screen),
        None,
        "CONTROL: the banner scrolled off"
    );
    let card = CardPlace {
        top: 0,
        rows: 2,
        mascot_row: 0,
        text_col: 11,
        mascot_end: 9,
        ends: [52, 59, 59],
    };
    let scan = announcement_scan(&screen, &card, None);
    assert_eq!(scan.place, Some(place(2, 1, 106, Some(3))));
    assert_eq!(scan.place.map(|p| p.span()), Some(2));
    assert_eq!((scan.from, scan.read_to, scan.stop), (2, Some(3), false));
}

#[test]
fn the_slot_is_read_under_the_update_notice() {
    let notice = [
        format!(
            "{INDENT}{} Got 3 features.",
            crate::anchor!("notice.updated")
        ),
        format!("{INDENT}{}", crate::anchor!("notice.updated_details")),
    ];
    let offer = barred("Share Claude Code and earn $10 in usage credits \u{b7} /passes");
    let screen = card_over(&[
        String::new(),
        notice[0].clone(),
        notice[1].clone(),
        String::new(),
        offer.clone(),
        hidden_row(2),
        String::new(),
        "\u{276F} ".into(),
    ]);
    let card = launch_card_place(&screen).expect("the card");
    let placed = crate::notice::update_notice_scan(&screen, &card, 120)
        .place
        .expect("CONTROL: the update notice");
    let scan = announcement_scan(&screen, &card, Some(&placed));
    assert_eq!(scan.from, 7, "under the notice");
    let end = offer.chars().count().max(hidden_row(2).chars().count());
    assert_eq!(scan.place, Some(place(8, 1, end, Some(9))));
    let under_card = announcement_scan(&screen, &card, None);
    assert_eq!(
        (under_card.from, under_card.place),
        (4, Some(place(8, 1, end, Some(9)))),
        "read from under the card, the notice's rows are a block over the slot, never the slot"
    );
}

#[test]
fn an_update_notice_its_reader_refuses_is_never_the_slot() {
    let updated = crate::anchor!("notice.updated");
    let details = format!("{INDENT}{}", crate::anchor!("notice.updated_details"));
    let refused = [
        vec![
            format!("{INDENT}{updated} Got 3 features."),
            format!("{INDENT}code.claude.com/docs/en/changelog for the full details"),
        ],
        vec![
            format!("{INDENT}{updated} Got features one,"),
            format!("{INDENT}two, three"),
            format!("{INDENT}and four,"),
            format!("{INDENT}and five."),
            details,
        ],
    ];
    for notice in refused {
        let mut under = vec![String::new()];
        under.extend(notice.iter().cloned());
        under.extend([String::new(), "\u{2500}".repeat(40)]);
        let screen = card_over(&under);
        let card = launch_card_place(&screen).expect("the card");
        assert_eq!(
            crate::notice::update_notice_scan(&screen, &card, 120).place,
            None,
            "CONTROL: the update reader refuses {notice:?}"
        );
        assert_eq!(
            announcement_scan(&screen, &card, None).place,
            None,
            "{notice:?}"
        );
        let mut news = under.clone();
        news[1] = bare(NEWS);
        let screen = card_over(&news);
        assert!(
            announcement_scan(&screen, &card, None).place.is_some(),
            "CONTROL: the same rows opening on news are the slot: {news:?}"
        );
    }
}

#[test]
fn the_slot_is_read_under_blocks_of_warnings() {
    let auth = "\u{26A0} Auth conflict: both a token and an API key are set";
    let barred_news = barred("Share Claude Code and earn $10 in usage credits \u{b7} /passes");
    for (slot, place_of) in [
        (
            barred_news.clone(),
            place(9, 1, barred_news.chars().count(), None),
        ),
        (bare(NEWS), news(9, None)),
    ] {
        let screen = card_over(&[
            String::new(),
            WARNING.into(),
            String::new(),
            auth.into(),
            String::new(),
            slot.clone(),
            String::new(),
            "\u{276F} ".into(),
        ]);
        let card = launch_card_place(&screen).expect("the card");
        assert_eq!(
            announcement_scan(&screen, &card, None).place,
            Some(place_of),
            "two blocks of warnings over {slot:?}"
        );
    }
    let bare_warnings = card_over(&[
        String::new(),
        WARNING.into(),
        String::new(),
        auth.into(),
        String::new(),
        "\u{276F} ".into(),
    ]);
    let card = launch_card_place(&bare_warnings).expect("the card");
    assert_eq!(
        announcement_scan(&bare_warnings, &card, None).place,
        None,
        "warnings alone are no slot"
    );
}

#[test]
fn a_slot_with_its_bar_off_is_read_in_each_shape_the_vendor_draws() {
    let rule = "\u{2500}".repeat(80);
    let fullscreen = |over: &[String]| {
        let mut under = vec![String::new()];
        under.extend(over.iter().cloned());
        under.extend(std::iter::repeat_n(String::new(), 27));
        under.extend([rule.clone(), "\u{276F} ".into(), rule.clone()]);
        card_over(&under)
    };
    let paragraphs = [
        bare("Lab news title"),
        bare("A first paragraph of the news."),
        String::new(),
        bare("A second paragraph of it."),
        bare("Lab footer: learn more at example.invalid"),
    ];
    let w = |rows: &[String]| rows.iter().map(|r| r.chars().count()).max().unwrap_or(0);
    let tip = bare("Lab emergency tip: Claude Code is degraded; see status.claude.com");
    let updated = [
        bare("Updated to latest. Got 66 features, 424 bugfixes, and 126 other changes. And more"),
        bare("code.claude.com/docs/en/changelog for details"),
    ];
    let hook = bare("\u{23BF}  SessionStart:startup says: Loaded 3 project memories");
    let tall = |n| std::iter::repeat_n(bare(NEWS), n).collect::<Vec<_>>();
    let tip_over = |n| {
        let mut under = vec![String::new(), tip.clone(), String::new()];
        under.extend(tall(n));
        under.extend([String::new(), "\u{276F} ".into()]);
        card_over(&under)
    };
    for (why, screen, want) in [
        (
            "fullscreen: blank rows down to the composer at the foot",
            fullscreen(&[bare(NEWS), hidden_row(1)]),
            news(5, Some(6)),
        ),
        (
            "fullscreen, under a warning",
            fullscreen(&[WARNING.into(), String::new(), bare(NEWS)]),
            news(7, None),
        ),
        (
            "paragraphs, a blank row between them",
            card_over(
                &std::iter::once(String::new())
                    .chain(paragraphs.iter().cloned())
                    .chain([String::new(), rule.clone()])
                    .collect::<Vec<_>>(),
            ),
            place(5, 5, w(&paragraphs), None),
        ),
        (
            "paragraphs under a warning, at the screen's foot",
            fullscreen(
                &[WARNING.to_owned(), String::new()]
                    .into_iter()
                    .chain(paragraphs.iter().cloned())
                    .collect::<Vec<_>>(),
            ),
            place(7, 5, w(&paragraphs), None),
        ),
        (
            "over a hook's message straight under it, as 2.1.312 draws one",
            fullscreen(&[bare(NEWS), hook.clone()]),
            news(5, None),
        ),
        (
            "over a hook's message under its hidden count, inline",
            card_over(&[
                String::new(),
                bare(NEWS),
                hidden_row(1),
                hook.clone(),
                String::new(),
                "\u{276F} ".into(),
            ]),
            news(5, Some(6)),
        ),
        (
            "over a hook's message wrapped over two rows",
            fullscreen(&[
                bare(NEWS),
                hook.clone(),
                bare("   and the rest of the hook's message, wrapped"),
            ]),
            news(5, None),
        ),
        (
            "over a hook's message, rows at the indent under it",
            fullscreen(&[
                bare(NEWS),
                hook.clone(),
                bare("Words at the indent under the hook's message"),
            ]),
            news(5, None),
        ),
        (
            "over a hook's message, a blank row between",
            fullscreen(&[bare(NEWS), String::new(), hook.clone()]),
            news(5, None),
        ),
        (
            "over a shell command, the first turn",
            card_over(&[
                String::new(),
                bare(NEWS),
                String::new(),
                "! git status".into(),
            ]),
            news(5, None),
        ),
        (
            "the tip read with the news under it",
            card_over(&[
                String::new(),
                tip.clone(),
                String::new(),
                bare(NEWS),
                String::new(),
                "\u{276F} ".into(),
            ]),
            place(5, 3, w(&[tip.clone(), bare(NEWS)]), None),
        ),
        (
            "an update notice its reader refuses, the tip, then the news",
            card_over(
                &std::iter::once(String::new())
                    .chain(updated.iter().cloned())
                    .chain([String::new(), tip.clone(), String::new(), bare(NEWS)])
                    .chain([String::new(), "\u{276F} ".into()])
                    .collect::<Vec<_>>(),
            ),
            place(8, 3, w(&[tip.clone(), bare(NEWS)]), None),
        ),
        (
            "the tallest news under the tip",
            tip_over(ANNOUNCEMENT_ROWS),
            place(
                5,
                ANNOUNCEMENT_ROWS + 2,
                w(&[tip.clone(), bare(NEWS)]),
                None,
            ),
        ),
    ] {
        let card = launch_card_place(&screen).expect("the card");
        let scan = announcement_scan(&screen, &card, None);
        assert_eq!(scan.place, Some(want), "{why}: {screen:#?}");
        assert!(scan.stop, "{why}: read down to the edge");
    }
    for (why, screen) in [
        (
            "news taller than any under the tip",
            tip_over(ANNOUNCEMENT_ROWS + 1),
        ),
        (
            "fullscreen, nothing at the foot",
            card_over(
                &[String::new(), bare(NEWS)]
                    .into_iter()
                    .chain(std::iter::repeat_n(String::new(), 27))
                    .collect::<Vec<_>>(),
            ),
        ),
        (
            "the update notice alone",
            card_over(
                &std::iter::once(String::new())
                    .chain(updated.iter().cloned())
                    .chain([String::new(), "\u{276F} ".into()])
                    .collect::<Vec<_>>(),
            ),
        ),
        (
            "a hook's message straight under a warning",
            fullscreen(&[WARNING.into(), hook.clone()]),
        ),
        (
            "a shell command's words at the indent",
            card_over(&[
                String::new(),
                bare(NEWS),
                String::new(),
                "!git status".into(),
            ]),
        ),
    ] {
        let card = launch_card_place(&screen).expect("the card");
        assert_eq!(
            announcement_scan(&screen, &card, None).place,
            None,
            "{why}: {screen:#?}"
        );
    }
}

#[test]
fn a_block_that_is_no_warning_hides_nothing_under_it() {
    let restored = [
        bare("\u{23BF}  Read src/main.rs (120 lines)"),
        bare("\u{23BF}  Referenced file docs/notes.md"),
        bare("\u{23BF}  Skills restored (rust-in-aterm)"),
    ];
    let screen = |first: &str, files: &[String]| {
        let mut under = vec![
            String::new(),
            String::new(),
            first.to_owned(),
            String::new(),
        ];
        under.extend(files.iter().cloned());
        under.extend([String::new(), "\u{23FA} Done.".to_owned()]);
        card_over(&under)
    };
    let compacted = screen(
        "\u{273B} Conversation compacted (ctrl+o for history)",
        &restored,
    );
    let card = launch_card_place(&compacted).expect("the card");
    assert_eq!(
        announcement_scan(&compacted, &card, None).place,
        None,
        "a compaction's row over the files it restored"
    );
    assert_eq!(
        announcement_scan(&screen(WARNING, &restored), &card, None).place,
        None,
        "nor are they read under a warning: a `\u{23BF}` row is the transcript's"
    );
    let words: Vec<String> = restored
        .iter()
        .map(|row| row.replace("\u{23BF}  ", ""))
        .collect();
    assert_eq!(
        announcement_scan(
            &screen(
                "\u{273B} Conversation compacted (ctrl+o for history)",
                &words
            ),
            &card,
            None
        )
        .place,
        None,
        "rows at the indent under the compaction's row"
    );
    assert!(
        announcement_scan(&screen(WARNING, &words), &card, None)
            .place
            .is_some(),
        "CONTROL: under a warning, the same rows at the indent are read as the slot"
    );
}

#[test]
fn a_slot_is_refused_unless_it_is_the_vendors_shape() {
    let offer = barred("\u{203B} You've been granted a $250 bonus credit \u{b7} /claim-credit");
    let wrapped = [
        barred("Auto mode is now Claude Code's default permission mode."),
        barred("Auto mode lets Claude handle permission prompts automatically."),
        barred("https://code.claude.com/docs/en/permission-modes"),
    ];
    let w = |rows: &[String]| rows.iter().map(|r| r.chars().count()).max().unwrap_or(0);
    for (why, screen, want) in [
        (
            "one row and its hidden count",
            card_over(&[String::new(), offer.clone(), hidden_row(1)]),
            place(5, 1, offer.chars().count(), Some(6)),
        ),
        (
            "two hidden",
            card_over(&[String::new(), offer.clone(), hidden_row(2), String::new()]),
            place(5, 1, offer.chars().count(), Some(6)),
        ),
        (
            "a wrapped notice",
            card_over(
                &std::iter::once(String::new())
                    .chain(wrapped.iter().cloned())
                    .chain([String::new()])
                    .collect::<Vec<_>>(),
            ),
            place(5, 3, w(&wrapped), None),
        ),
        (
            "a blank line inside the notice",
            card_over(&[
                String::new(),
                wrapped[0].clone(),
                BAR.to_string(),
                wrapped[2].clone(),
                String::new(),
            ]),
            place(5, 3, w(&[wrapped[0].clone(), wrapped[2].clone()]), None),
        ),
        (
            "no blank row under the card",
            card_over(&[offer.clone(), String::new()]),
            place(4, 1, offer.chars().count(), None),
        ),
        (
            "the bar off, its hidden count, the prompt",
            card_over(&[
                String::new(),
                bare(NEWS),
                hidden_row(1),
                String::new(),
                "\u{276F} ".into(),
            ]),
            place(5, 1, bare(NEWS).chars().count(), Some(6)),
        ),
        (
            "the bar off, wrapped, over the composer's rule",
            card_over(&[
                String::new(),
                bare(NEWS),
                bare("Terms apply"),
                String::new(),
                "\u{2500}".repeat(80),
            ]),
            place(5, 2, bare(NEWS).chars().count(), None),
        ),
        (
            "a warning over the slot",
            card_over(&[
                String::new(),
                WARNING.into(),
                String::new(),
                offer.clone(),
                hidden_row(1),
                String::new(),
            ]),
            place(7, 1, offer.chars().count(), Some(8)),
        ),
        (
            "a warning and its hint over the slot with its bar off",
            card_over(&[
                String::new(),
                WARNING.into(),
                bare("ctrl+x to re-enable"),
                String::new(),
                bare(NEWS),
                hidden_row(2),
                String::new(),
                "\u{276F} ".into(),
            ]),
            place(8, 1, bare(NEWS).chars().count(), Some(9)),
        ),
        (
            "the bar off two blank rows over the prompt",
            card_over(&[
                String::new(),
                bare(NEWS),
                String::new(),
                String::new(),
                "\u{276F} ".into(),
            ]),
            news(5, None),
        ),
        (
            "the emergency tip at the indent over the slot",
            card_over(&[
                String::new(),
                bare("Claude Code is degraded; see status.claude.com"),
                String::new(),
                offer.clone(),
                String::new(),
            ]),
            place(7, 1, offer.chars().count(), None),
        ),
    ] {
        assert_eq!(slot_on(&screen), Some(want), "{why}: {screen:#?}");
    }
    for (why, screen) in [
        (
            "a card with no slot",
            card_over(&[String::new(), "\u{2500}".repeat(80), "\u{276F} ".into()]),
        ),
        (
            "the bar off and nothing under it",
            card_over(&[String::new(), bare(NEWS), hidden_row(1), String::new()]),
        ),
        (
            "the bar off over model output",
            card_over(&[
                String::new(),
                bare(NEWS),
                String::new(),
                "words at column 0".into(),
            ]),
        ),
        (
            "the bar off with no blank row over the prompt",
            card_over(&[String::new(), bare(NEWS), "\u{276F} ".into()]),
        ),
        (
            "rows off the indent",
            card_over(&[
                String::new(),
                format!(" {}", bare(NEWS)),
                String::new(),
                "\u{276F} ".into(),
            ]),
        ),
        (
            "a warning alone over the prompt",
            card_over(&[
                String::new(),
                WARNING.into(),
                String::new(),
                "\u{276F} ".into(),
            ]),
        ),
        (
            "a warning and its hint alone over the composer",
            card_over(&[
                String::new(),
                WARNING.into(),
                bare("unset it to re-enable"),
                String::new(),
                "\u{2500}".repeat(80),
            ]),
        ),
        (
            "the bar off, taller than any notice",
            card_over(
                &std::iter::once(String::new())
                    .chain(std::iter::repeat_n(bare(NEWS), ANNOUNCEMENT_ROWS + 1))
                    .chain([String::new(), "\u{276F} ".into()])
                    .collect::<Vec<_>>(),
            ),
        ),
        (
            "a quote further down the transcript",
            card_over(&[
                String::new(),
                "\u{276F} hi".into(),
                String::new(),
                offer.clone(),
                String::new(),
            ]),
        ),
        (
            "a quoted block in model output under the composer",
            card_over(&[
                String::new(),
                "\u{2500}".repeat(80),
                "\u{276F} ".into(),
                "\u{2500}".repeat(80),
                offer.clone(),
                String::new(),
            ]),
        ),
        (
            "a quote at a message's indent",
            card_over(&[String::new(), format!("{INDENT}{offer}"), String::new()]),
        ),
        (
            "the slot past the reach",
            card_over(
                &std::iter::repeat_n(String::new(), ANNOUNCEMENT_REACH)
                    .chain([offer.clone(), String::new()])
                    .collect::<Vec<_>>(),
            ),
        ),
        (
            "the slot cut by the bottom edge",
            card_over(&[String::new(), offer.clone()]),
        ),
        (
            "a bar with no words",
            card_over(&[String::new(), BAR.to_string(), String::new()]),
        ),
        (
            "a bar glued to its words",
            card_over(&[String::new(), format!("{BAR}offer"), String::new()]),
        ),
        (
            "taller than any notice",
            card_over(
                &std::iter::once(String::new())
                    .chain(std::iter::repeat_n(offer.clone(), ANNOUNCEMENT_ROWS + 1))
                    .chain([String::new()])
                    .collect::<Vec<_>>(),
            ),
        ),
    ] {
        assert_eq!(slot_on(&screen), None, "{why}: {screen:#?}");
    }
    let tallest = card_over(
        &std::iter::once(String::new())
            .chain(std::iter::repeat_n(offer.clone(), ANNOUNCEMENT_ROWS))
            .chain([String::new()])
            .collect::<Vec<_>>(),
    );
    assert_eq!(
        slot_on(&tallest).map(|p| p.rows),
        Some(ANNOUNCEMENT_ROWS),
        "CONTROL: the tallest slot is placed whole"
    );
    let tallest_bare = card_over(
        &std::iter::once(String::new())
            .chain(std::iter::repeat_n(bare(NEWS), ANNOUNCEMENT_ROWS))
            .chain([hidden_row(1), String::new(), "\u{276F} ".into()])
            .collect::<Vec<_>>(),
    );
    assert_eq!(
        slot_on(&tallest_bare).map(|p| (p.rows, p.hidden)),
        Some((ANNOUNCEMENT_ROWS, Some(5 + ANNOUNCEMENT_ROWS))),
        "CONTROL: the tallest slot with its bar off is placed whole, with its hidden count"
    );
    for (why, row) in [
        ("no count", format!("{INDENT} more notice hidden")),
        (
            "another word",
            format!("{INDENT}1{} shown", crate::anchor!("notice.more")),
        ),
        ("off the indent", format!(" {}", hidden_row(1))),
        ("words after it", format!("{} today", hidden_row(1))),
    ] {
        let screen = card_over(&[String::new(), offer.clone(), row, String::new()]);
        assert_eq!(
            slot_on(&screen).map(|p| p.hidden),
            Some(None),
            "{why}: the slot without a hidden count"
        );
    }
    let card = launch_card_place(&card_over(&[])).expect("the card");
    for screen in [Vec::new(), rows(&[""]), card_over(&[String::new()])] {
        let _ = announcement_scan(&screen, &card, None);
    }
    let far = CardPlace {
        top: usize::MAX,
        ..card
    };
    let screen = card_over(&[String::new(), offer, String::new()]);
    assert_eq!(announcement_scan(&screen, &far, None).place, None);
}

#[test]
fn a_kept_announcement_scan_is_the_scans() {
    let rewrites: [String; 8] = [
        bare("\u{23BF}  SessionStart:startup says: Loaded 3 project memories"),
        String::new(),
        barred("Your Max plan includes $200 a month in API credits"),
        hidden_row(3),
        "\u{276F} what changed?".into(),
        "! git status".into(),
        "\u{23FA} streamed".into(),
        "\u{2500}".repeat(40),
    ];
    // Another hook's message than the rewrite's, so each rewrite changes the stop row straight
    // under the news.
    let hook = bare("\u{23BF}  PostToolUse:Bash says: ok");
    let mut screens_read = corpus();
    screens_read.extend([
        (
            "news over a hook's message, inline".to_owned(),
            card_over(&[
                String::new(),
                bare(NEWS),
                hook.clone(),
                String::new(),
                "\u{276F} ".into(),
            ]),
        ),
        (
            "news over a hook's message, fullscreen".to_owned(),
            card_over(
                &[String::new(), bare(NEWS), hook]
                    .into_iter()
                    .chain(std::iter::repeat_n(String::new(), 27))
                    .chain(["\u{2500}".repeat(80), "\u{276F} ".into()])
                    .collect::<Vec<_>>(),
            ),
        ),
    ]);
    let (mut kept, mut moved, mut screens, mut stopped) = (0, 0, 0, 0);
    for (name, before) in screens_read {
        let Some(card) = launch_card_place(&before) else {
            continue;
        };
        screens += 1;
        let notice = crate::notice::update_notice_scan(&before, &card, 120).place;
        let reading = announcement_scan(&before, &card, notice.as_ref());
        for at in 0..before.len() {
            for rewrite in &rewrites {
                let mut after = before.clone();
                after[at].clone_from(rewrite);
                let changed: Vec<usize> = (0..after.len())
                    .filter(|&i| after[i] != before[i])
                    .collect();
                if announcement_unmoved(&after, &reading, &changed) {
                    assert_eq!(
                        announcement_scan(&after, &card, notice.as_ref()),
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
    let card = launch_card_place(&card_over(&[])).expect("the card");
    let edge = announcement_scan(&card_over(&[]), &card, None);
    assert_eq!((edge.read_to, edge.place), (None, None));
    assert!(announcement_unmoved(&card_over(&[]), &edge, &[0, 3]));
    let _ = announcement_unmoved(&[], &edge, &[usize::MAX]);
}

#[test]
fn a_slot_is_tracked_after_its_card_scrolls_off_until_its_last_row_goes() {
    let slot = [
        barred("Auto mode is now Claude Code's default permission mode."),
        barred("Auto mode lets Claude handle permission prompts automatically."),
        BAR.to_string(),
        barred("https://code.claude.com/docs/en/permission-modes"),
        hidden_row(2),
    ];
    let transcript = rows(&["", "\u{276F} hi", "", "\u{23FA} Hi!", ""]);
    let screen = card_over(
        &std::iter::once(String::new())
            .chain(slot.iter().cloned())
            .chain(transcript.iter().cloned())
            .collect::<Vec<_>>(),
    );
    let card = launch_card_place(&screen).expect("the card");
    let scan = announcement_scan(&screen, &card, None);
    let read = scan.place.expect("CONTROL: the slot under the card");
    assert_eq!((read.top, read.rows, read.hidden), (5, 4, Some(9)));
    let from = card.top + card.rows;
    let kept = screen[from..read.top + read.span()].to_vec();
    let at = AnnouncementPlace {
        top: read.top - from,
        hidden: read.hidden.map(|h| h - from),
        ..read
    };
    assert_eq!(
        kept.len(),
        1 + slot.len(),
        "the blank row under the card, then the slot"
    );
    for gone in 0..=kept.len() {
        let mut now = kept[gone..].to_vec();
        now.extend(transcript.iter().cloned());
        let cut = gone.saturating_sub(1);
        let want = (gone < kept.len()).then(|| AnnouncementPlace {
            top: 1usize.saturating_sub(gone),
            rows: 4usize.saturating_sub(cut),
            hidden: Some(1usize.saturating_sub(gone) + 4usize.saturating_sub(cut)),
            ..read
        });
        assert_eq!(
            announcement_scrolled(&now, &kept, &at),
            want,
            "{gone} gone: {now:#?}"
        );
        if let Some(place) = want {
            assert_eq!(place.span(), slot.len() - cut, "{gone} gone: the rows left");
        }
    }
    for (why, at_row, text) in [
        (
            "another row at the slot's top",
            0,
            barred("Claude Max gives you more usage."),
        ),
        ("its hidden count moved on", 3, hidden_row(3)),
        (
            "a reply where the slot stood",
            0,
            "\u{23FA} Auto mode is on.".to_string(),
        ),
    ] {
        let mut now = kept[2..].to_vec();
        now.extend(transcript.iter().cloned());
        now[at_row].clone_from(&text);
        assert_eq!(
            announcement_scrolled(&now, &kept, &at),
            None,
            "NEGATIVE CONTROL: {why}"
        );
    }
    let mut now = kept[1..].to_vec();
    now.extend(transcript.iter().cloned());
    assert!(
        announcement_scrolled(&now, &kept, &at).is_some(),
        "CONTROL: the slot at the top"
    );
    assert_eq!(
        announcement_scrolled(&now[..3], &kept, &at),
        None,
        "the slot cut by the bottom edge"
    );
    let off = AnnouncementPlace { top: 2, ..at };
    assert_eq!(
        announcement_scrolled(&now, &kept, &off),
        None,
        "a slot that is not the kept rows' end"
    );
    let _ = announcement_scrolled(&[], &[], &at);
}
