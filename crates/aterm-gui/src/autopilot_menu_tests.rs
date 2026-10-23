// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Autopilot menu (`docs/DESIGN-harness-autopilot-2026-10-15.md` §9.3.3, §9.3.8): the model's rows under each posture, the geometry, the hit test and keys, the paint, the accessibility node, `controls autopilot-menu`, and the menu on a real footer — opened from its chip, acting, and closing at the first frame whose glass no longer shows it.

use std::time::Instant;

use aterm_agent::harness::footer::FooterFacts;

use super::*;
use crate::claude_lights::{Clock, LandAtGlass, Weekday};
use crate::{App, WindowId, term_lock};

/// `[harness] land_at` offered at `hh:mm`, on `day` where that is not today.
fn offer(hh: u16, mm: u16, day: Option<Weekday>) -> Option<LandAtGlass> {
    Some(LandAtGlass {
        clock: Clock::of_minute(hh * 60 + mm),
        other_day: day,
    })
}

fn facts(view: AutopilotView) -> MenuFacts {
    MenuFacts {
        title: "Autopilot \u{b7} tab 3".to_string(),
        name: Some("fix the footer".to_string()),
        room: MAX_CONTENT,
        spoken: "Autopilot, tab 3, fix the footer".to_string(),
        view,
        carries: view.mode().is_some(),
        land_at: None,
        armed_at_ms: None,
        armed: None,
        set_by: None,
    }
}

/// Every row's stable key, in order (`None` for a header, note or rule).
fn keys(rows: &[MenuRow]) -> Vec<Option<String>> {
    rows.iter()
        .map(|r| match r {
            MenuRow::Item { act, .. } => Some(act.key()),
            _ => None,
        })
        .collect()
}

fn item(rows: &[MenuRow], key: &str) -> MenuRow {
    rows.iter()
        .find(|r| matches!(r, MenuRow::Item { act, .. } if act.key() == key))
        .cloned()
        .unwrap_or_else(|| panic!("no row {key}"))
}

/// THE MODEL UNDER EACH POSTURE (§9.3.3). Carrying a mode: the three modes,
/// the effective one checked; 1 h, 2 h and 4 h; until `[harness] land_at`
/// only where it is set; the cancel only while a timer is armed, naming its
/// time (and its weekday on another day); Settings — every choice on.
#[test]
fn the_menus_rows_follow_the_posture_the_offer_and_the_timer() {
    let rows = rows(&facts(AutopilotView::Cruise));
    assert_eq!(
        keys(&rows),
        [
            None,
            Some("autopilot-menu/mode/cruise".into()),
            Some("autopilot-menu/mode/land".into()),
            Some("autopilot-menu/mode/off".into()),
            None,
            Some("autopilot-menu/land-in/60".into()),
            Some("autopilot-menu/land-in/120".into()),
            Some("autopilot-menu/land-in/240".into()),
            None,
            Some("autopilot-menu/settings".into()),
        ]
    );
    assert_eq!(
        rows[0],
        MenuRow::Header("Autopilot \u{b7} tab 3 \u{201c}fix the footer\u{201d}".into())
    );
    assert!(
        rows.iter()
            .all(|r| !matches!(r, MenuRow::Item { enabled: false, .. }))
    );
    let checked: Vec<String> = rows
        .iter()
        .filter_map(|r| match r {
            MenuRow::Item {
                checked: true, act, ..
            } => Some(act.key()),
            _ => None,
        })
        .collect();
    assert_eq!(checked, ["autopilot-menu/mode/cruise"]);
    for (key, text) in [
        ("autopilot-menu/mode/cruise", "\u{221e}  Cruise"),
        ("autopilot-menu/mode/land", "\u{2913}  Land now"),
        ("autopilot-menu/mode/off", "\u{25cb}  Off"),
        (
            "autopilot-menu/land-in/60",
            "\u{221e}\u{2913} Cruise 1 hour, then land",
        ),
        (
            "autopilot-menu/land-in/120",
            "\u{221e}\u{2913} Cruise 2 hours, then land",
        ),
        (
            "autopilot-menu/land-in/240",
            "\u{221e}\u{2913} Cruise 4 hours, then land",
        ),
    ] {
        assert_eq!(item(&rows, key).text(), text);
    }

    let mut f = facts(AutopilotView::Land);
    f.land_at = offer(18, 0, None);
    f.armed_at_ms = Some(1_792_177_200_000);
    f.armed = Some(crate::claude_lights::LandAtGlass {
        clock: Clock::of_minute(16 * 60),
        other_day: Some(Weekday::Sat),
    });
    let rows = super::rows(&f);
    assert!(matches!(
        item(&rows, "autopilot-menu/mode/land"),
        MenuRow::Item { checked: true, .. }
    ));
    assert_eq!(
        item(&rows, "autopilot-menu/land-at").text(),
        "\u{221e}\u{2913} Cruise until 18:00, then land"
    );
    let mut later = f.clone();
    later.land_at = offer(18, 0, Some(Weekday::Sat));
    let MenuRow::Item { label, spoken, .. } = item(&super::rows(&later), "autopilot-menu/land-at")
    else {
        unreachable!()
    };
    assert_eq!(
        (label.as_str(), spoken.as_str()),
        (
            "Cruise until Sat 18:00, then land",
            "Cruise until Saturday 18:00, then land"
        )
    );
    let MenuRow::Item { label, spoken, .. } = item(&rows, "autopilot-menu/land-cancel") else {
        unreachable!()
    };
    assert_eq!(label, "Cancel landing at Sat 16:00");
    assert_eq!(spoken, "Cancel landing at Saturday 16:00");
    let order = keys(&rows);
    let at = |k: &str| order.iter().position(|o| o.as_deref() == Some(k)).unwrap();
    assert!(
        at("autopilot-menu/land-in/240") < at("autopilot-menu/land-at")
            && at("autopilot-menu/land-at") < at("autopilot-menu/land-cancel")
            && at("autopilot-menu/land-cancel") < at("autopilot-menu/settings")
    );

    for view in [AutopilotView::MasterOff, AutopilotView::Unhosted] {
        let mut f = facts(view);
        f.land_at = offer(18, 0, None);
        let rows = super::rows(&f);
        assert_eq!(
            rows[1],
            MenuRow::Note("harness off in Settings".into()),
            "{view:?}"
        );
        for row in &rows {
            if let MenuRow::Item {
                act,
                enabled,
                checked,
                ..
            } = row
            {
                assert_eq!(*enabled, *act == MenuAct::Settings, "{view:?} {act:?}");
                assert!(!checked, "{view:?} {act:?}");
            }
        }
    }
    for row in super::rows(&f) {
        if let MenuRow::Item { spoken, .. } = row {
            assert!(
                spoken
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || " .:,".contains(c)),
                "{spoken:?}"
            );
        }
    }
}

/// UNDER THE AGENT'S OWN SWITCH (§10.2.1, §10.2.7): the menu says why its
/// rows are off as the chip's title does, by the switch's Settings label —
/// `Supervise Claude Code sessions is off` (a codex-alab tab's `Watch
/// codex-alab sessions is off`) — never the master switch's words, and,
/// as under the master switch, only Settings acts and nothing is checked.
#[test]
fn under_an_agents_own_switch_the_menu_names_that_switch() {
    use aterm_phase::Program;
    for (agent, why) in [
        (Program::Codex, "Watch codex-alab sessions is off"),
        (Program::Claude, "Supervise Claude Code sessions is off"),
        (Program::Gemini, "Supervise Gemini CLI sessions is off"),
    ] {
        let view = AutopilotView::AgentOff(agent);
        let mut f = facts(view);
        f.land_at = offer(18, 0, None);
        let rows = rows(&f);
        assert_eq!(rows[1], MenuRow::Note(why.into()), "{agent:?}");
        assert_eq!(
            rows.iter()
                .filter(|r| matches!(r, MenuRow::Note(_)))
                .count(),
            1,
            "{agent:?}"
        );
        for row in &rows {
            if let MenuRow::Item {
                act,
                enabled,
                checked,
                ..
            } = row
            {
                assert_eq!(*enabled, *act == MenuAct::Settings, "{agent:?} {act:?}");
                assert!(!checked, "{agent:?} {act:?}");
            }
        }
    }
    for view in [AutopilotView::MasterOff, AutopilotView::Unhosted] {
        assert_eq!(
            rows(&facts(view))[1],
            MenuRow::Note("harness off in Settings".into()),
            "{view:?}"
        );
    }
}

/// THE PLACES KEPT (lifecycle-1, pure): rows built again under an open menu
/// are laid into the places the shown rows hold.
#[test]
fn rows_built_again_keep_their_places_and_a_row_that_left_leaves_a_gap() {
    let armed = MenuFacts {
        armed_at_ms: Some(1),
        armed: offer(18, 36, None),
        ..facts(AutopilotView::Cruise)
    };
    let shown = rows(&armed);
    let fresh = rows(&facts(AutopilotView::Land));
    let cancel = keys(&shown)
        .iter()
        .position(|k| k.as_deref() == Some("autopilot-menu/land-cancel"))
        .expect("armed: the cancel");
    let laid = keep_places(&shown, &fresh).expect("only a row left");
    assert_eq!(laid.len(), shown.len());
    assert_eq!(laid[cancel], MenuRow::Gap(super::row_width(&shown[cancel])));
    for (i, (was, now)) in keys(&shown).iter().zip(keys(&laid)).enumerate() {
        if i != cancel {
            assert_eq!(*was, now, "row {i} kept its place");
        }
    }
    assert!(!laid[cancel].selectable());
    assert_eq!(super::content_width(&laid), super::content_width(&shown));
    assert_eq!(
        place(&laid, (30, 4), (0..40, 0..120), (120, 40), 1),
        place(&shown, (30, 4), (0..40, 0..120), (120, 40), 1)
    );
    assert!(
        (0..laid.len()).all(|from| step(&laid, Some(from), true, laid.len()) != Some(cancel)),
        "the keys skip the gap"
    );
    assert_eq!(keep_places(&shown, &shown).as_deref(), Some(&shown[..]));
    assert_eq!(keep_places(&laid, &fresh).as_deref(), Some(&laid[..]));
    assert_eq!(keep_places(&fresh, &shown), None, "a row arrived");
    assert_eq!(keep_places(&laid, &shown), None, "a gap is no place");
}

/// THE GEOMETRY: above the chip with its left edge at the chip's column;
/// shifted left only to stay whole; below only where it is not whole above;
/// over the chip's row, whole, where neither side holds it; its first rows
/// where the frame is shorter than it; none where no card fits.
#[test]
fn the_card_hangs_above_the_chip_and_opens_down_only_where_fewer_rows_are_above() {
    let rows = rows(&facts(AutopilotView::Cruise));
    let h = rows.len() + 2;
    let rect = place(&rows, (30, 10), (0..40, 0..120), (120, 40), 1).unwrap();
    assert_eq!(
        (rect.row, rect.col, rect.h),
        (30 - h, 10, h),
        "above, at the chip"
    );
    assert!(rect.w >= "\u{221e}\u{2913} Cruise 4 hours, then land".chars().count());
    let right = place(&rows, (30, 115), (0..40, 0..120), (120, 40), 1).unwrap();
    assert_eq!(right.col + right.w, 120, "shifted left, whole");
    let low = place(&rows, (3, 10), (0..40, 0..120), (120, 40), 1).unwrap();
    assert_eq!((low.row, low.h), (4, h), "fewer rows above: below the chip");
    let middle = place(&rows, (10, 0), (0..20, 0..120), (120, 20), 1).unwrap();
    assert_eq!(
        (middle.row, middle.h),
        (1, h),
        "neither side holds it: whole, over the chip's row"
    );
    let short = place(&rows, (6, 0), (0..10, 0..120), (120, 10), 1).unwrap();
    assert_eq!(
        (short.row, short.h),
        (1, 9),
        "shorter than the card: a prefix"
    );
    assert_eq!(row_at(short, 1), None, "the top border");
    assert_eq!(row_at(short, 2), Some(0));
    assert_eq!(row_at(short, 8), Some(6));
    assert_eq!(row_at(short, 9), None, "the bottom border");
    let ruled = place(&rows, (6, 0), (0..8, 0..120), (120, 8), 1).unwrap();
    assert_eq!(
        (ruled.row, ruled.h),
        (1, 6),
        "the modes, not the rule under them"
    );
    assert_eq!(
        place(&rows, (30, 0), (0..40, 0..3), (3, 40), 1),
        None,
        "narrower than a border pair"
    );
    assert_eq!(
        place(&rows, (1, 0), (0..3, 0..120), (120, 3), 1),
        None,
        "no room either side"
    );
    assert_eq!(place(&[], (30, 0), (0..40, 0..120), (120, 40), 1), None);
}

/// THE CARD STAYS IN ITS PANE, UP AND DOWN AND ACROSS (the review of
/// v3/polish-page): within the pane's rows it hangs above
/// the chip, opens downward only where it is not whole above, and covers
/// the chip's row to stay whole where neither side of the pane holds it —
/// never reaching into a pane above or below; a pane shorter than the card
/// shows its first rows; a pane too short for the header and the three
/// modes lends it the frame.
#[test]
fn the_card_keeps_to_its_panes_rows_and_gives_up_its_padding_first() {
    let rows = rows(&facts(AutopilotView::Cruise));
    let want = rows.len() + 2;
    let top = place(&rows, (10, 4), (1..12, 0..100), (100, 30), 1).unwrap();
    assert_eq!((top.row, top.h), (1, 10), "its pane's rows, a prefix");
    let frame = place(&rows, (10, 4), (0..30, 0..100), (100, 30), 1).unwrap();
    assert_eq!(frame.row, 11, "control: the frame alone opens it downward");
    let bottom = place(&rows, (24, 4), (17..30, 0..100), (100, 30), 1).unwrap();
    assert_eq!((bottom.row, bottom.h), (17, want), "whole in its pane");
    let frame = place(&rows, (24, 4), (0..30, 0..100), (100, 30), 1).unwrap();
    assert_eq!(
        frame.row,
        24 - want,
        "control: the frame alone, over the pane above"
    );
    let low = place(&rows, (3, 4), (1..30, 0..100), (100, 30), 1).unwrap();
    assert_eq!((low.row, low.h), (4, want), "below, whole");
    let lower = place(&rows, (5, 4), (1..16, 0..100), (100, 30), 1).unwrap();
    assert_eq!(
        (lower.row, lower.h),
        (16 - want, want),
        "over the chip, from below"
    );
    assert!(
        lower.row <= 5 && 5 < lower.row + lower.h,
        "{lower:?} covers the chip"
    );
    let lent = place(&rows, (28, 4), (25..30, 0..100), (100, 30), 1).unwrap();
    assert_eq!((lent.row, lent.h), (28 - want, want), "lent the frame");
    let named = |cols: usize| MenuFacts {
        title: "Autopilot \u{b7} tab 1, right pane".to_string(),
        name: Some("\u{2733} Claude Code".to_string()),
        room: content_room(cols),
        ..facts(AutopilotView::Cruise)
    };
    let theme = aterm_render::Theme {
        fg: 0x00E0_E0E0,
        bg: 0x0018_1818,
        cursor: 0x00FF_FFFF,
        selection: 0x0044_4466,
    };
    let text = |r: &[RenderCell]| r.iter().map(|c| c.ch).collect::<String>();
    let tight = super::rows(&named(31));
    let rect = place(&tight, (28, 40), (1..30, 33..64), (64, 30), 1).unwrap();
    assert_eq!((rect.col, rect.w), (33, 31), "as wide as its pane");
    let painted: Vec<String> = paint(rect, &tight, None, theme)
        .iter()
        .map(|r| text(r))
        .collect();
    assert_eq!(
        painted[1],
        "\u{2502}Autopilot \u{b7} tab 1, right pane\u{2502}"
    );
    assert!(
        painted
            .iter()
            .any(|row| row.contains("\u{221e}\u{2913} Cruise 4 hours, then land")),
        "{painted:#?}"
    );
    let tighter = super::rows(&named(29));
    assert_eq!(
        tighter[..3],
        [
            MenuRow::Header("Autopilot \u{b7} tab 1,".to_string()),
            MenuRow::Header("right pane".to_string()),
            MenuRow::Name("\u{2733} Claude Code".to_string()),
        ]
    );
    let wide = super::rows(&named(100));
    assert_eq!(
        wide[0],
        MenuRow::Header("Autopilot \u{b7} tab 1, right pane".to_string())
    );
    let rect = place(&wide, (28, 40), (1..30, 0..100), (100, 30), 1).unwrap();
    let painted = paint(rect, &wide, None, theme);
    assert_eq!(text(&painted[1]).chars().nth(1), Some(' '), "padded");
}

/// The hit test chooses only a shown, enabled choice; the keys step over
/// headers, rules and disabled rows, wrapping; the key table.
#[test]
fn the_hit_test_and_the_keys_land_only_on_live_choices() {
    let rows = rows(&facts(AutopilotView::Cruise));
    let rect = place(&rows, (30, 10), (0..40, 0..120), (120, 40), 1).unwrap();
    let at = |i: usize| item_at(rect, &rows, rect.row + 1 + i, rect.col + 3);
    assert_eq!(at(0), None, "the header");
    assert_eq!(at(1), Some(1), "Cruise");
    assert_eq!(at(4), None, "a rule");
    assert_eq!(
        item_at(rect, &rows, rect.row, rect.col + 3),
        None,
        "a border"
    );
    assert_eq!(
        item_at(rect, &rows, rect.row + 2, rect.col + rect.w),
        None,
        "off the card"
    );
    let n = rows.len();
    assert_eq!(step(&rows, None, true, n), Some(1));
    assert_eq!(step(&rows, Some(3), true, n), Some(5), "over the rule");
    assert_eq!(step(&rows, Some(n - 1), true, n), Some(1), "wraps");
    assert_eq!(step(&rows, Some(1), false, n), Some(n - 1), "wraps back");
    let off = super::rows(&facts(AutopilotView::MasterOff));
    let settings = off.len() - 1;
    assert_eq!(
        step(&off, None, true, off.len()),
        Some(settings),
        "only Settings is live"
    );
    assert_eq!(step(&off, Some(settings), true, off.len()), Some(settings));
    use aterm_types::keyboard::{Key, NamedKey as Nk};
    for (key, want) in [
        (Key::Named(Nk::ArrowUp), MenuKey::Up),
        (Key::Named(Nk::ArrowDown), MenuKey::Down),
        (Key::Named(Nk::Enter), MenuKey::Choose),
        (Key::Named(Nk::Escape), MenuKey::Close),
        (Key::Named(Nk::ShiftLeft), MenuKey::Inert),
        (Key::Character('a'), MenuKey::PassThrough),
        (Key::Character(' '), MenuKey::PassThrough),
        (Key::Named(Nk::Tab), MenuKey::PassThrough),
    ] {
        assert_eq!(key_of(&key), want, "{key:?}");
    }
}

/// THE PAINT: exactly the rect; the checked row's `●` flush right; a disabled
/// row in the disabled ink; the highlighted row on its own ground.
#[test]
fn the_card_paints_its_rect_its_check_and_its_disabled_rows() {
    let theme = aterm_render::Theme {
        fg: 0x00E0_E0E0,
        bg: 0x0018_1818,
        cursor: 0x00FF_FFFF,
        selection: 0x0044_4466,
    };
    let rows = rows(&facts(AutopilotView::Cruise));
    let rect = place(&rows, (30, 4), (0..40, 0..120), (120, 40), 1).unwrap();
    let painted = paint(rect, &rows, Some(2), theme);
    assert_eq!(painted.len(), rect.h);
    assert!(painted.iter().all(|r| r.len() == rect.w));
    let text = |r: &[RenderCell]| r.iter().map(|c| c.ch).collect::<String>();
    assert!(
        text(&painted[2]).contains("Cruise"),
        "{}",
        text(&painted[2])
    );
    assert_eq!(
        painted[2][rect.w - 3].ch,
        '\u{25cf}',
        "the check, flush right"
    );
    assert_ne!(painted[3][3].bg, painted[2][3].bg, "the highlight's ground");
    let off = super::rows(&facts(AutopilotView::MasterOff));
    let rect = place(&off, (30, 4), (0..40, 0..120), (120, 40), 1).unwrap();
    let painted = paint(rect, &off, None, theme);
    let cruise = 1 + 2;
    let settings = off.len();
    assert_ne!(
        painted[cruise][3].fg, painted[settings][3].fg,
        "a disabled row reads as one"
    );
}

/// WHO SET IT, UNDER THE HEADER (design §11.7.1, §11.7.3, item 6): the note
/// is one inert row under the header in the note's ink — never chosen, never
/// lit — where it is whole in the card's room; broken at its comma onto two
/// rows where only its halves are whole (`… from Settings ▸ Harness,` then
/// `every session`); absent where no half is; and absent with no setter.
#[test]
fn the_setters_note_is_whole_or_absent_and_gives_way_first() {
    let with = |set_by: &str, room: usize| {
        rows(&MenuFacts {
            set_by: Some(set_by.to_string()),
            room,
            ..facts(AutopilotView::Off)
        })
    };
    let notes = |rows: &[MenuRow]| -> Vec<String> {
        rows.iter()
            .filter_map(|r| match r {
                MenuRow::SetBy { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect()
    };
    let plain = rows(&facts(AutopilotView::Off));
    let noted = with("set 14:05 over aterm ctl", MAX_CONTENT);
    assert_eq!(notes(&plain), Vec::<String>::new(), "no setter, no note");
    assert_eq!(
        noted[1],
        MenuRow::SetBy {
            text: "set 14:05 over aterm ctl".into(),
            floor: 0
        },
        "under the header"
    );
    assert_eq!(noted.len(), plain.len() + 1);
    assert_eq!(keys(&noted[2..]), keys(&plain[1..]), "every choice kept");
    assert!(!noted[1].selectable());
    assert_eq!(
        step(&noted, None, true, noted.len()),
        Some(2),
        "the first choice is lit, never the note"
    );
    let bulk = "set 14:05 from Settings \u{25b8} Harness, every session";
    assert!(display_cells(bulk) > MAX_CONTENT);
    assert_eq!(
        notes(&with(bulk, MAX_CONTENT)),
        ["set 14:05 from Settings \u{25b8} Harness,", "every session"]
    );
    assert_eq!(notes(&with(bulk, 24)), Vec::<String>::new());
    assert_eq!(
        notes(&with("set 14:05 over aterm ctl", 20)),
        Vec::<String>::new()
    );

    let fit = |frame_rows: usize, chip_row: usize| {
        let pane = (0..frame_rows, 0..120);
        let fitted = fit_rows(
            noted.clone(),
            (chip_row, 4),
            pane.clone(),
            (120, frame_rows),
            1,
        );
        let rect = place(&fitted, (chip_row, 4), pane, (120, frame_rows), 1).expect("a card");
        (fitted, rect)
    };
    let (kept, rect) = fit(24, 23);
    assert_eq!(kept, noted, "room for it: kept");
    assert_eq!(rect.h, noted.len() + 2, "whole");
    assert!(rect.row + rect.h <= 23, "above its chip");
    let (fitted, rect) = fit(15, 13);
    assert_eq!(fitted, plain, "the note gave way");
    assert_eq!(rect.h, plain.len() + 2, "every choice whole");
    assert!(rect.row + rect.h <= 13, "still above its chip");
    let (fitted, rect) = fit(13, 12);
    assert_eq!(fitted, plain);
    assert_eq!(rect.h, plain.len() + 2, "whole");
    let cut = place(&noted, (12, 4), (0..13, 0..120), (120, 13), 1).expect("a card");
    assert!(
        cut.h < noted.len() + 2,
        "CONTROL: with the note this pane shows a prefix ({cut:?})"
    );
}

/// A headless App with a host, its session a Claude Code pane drawn with its
/// footer (the chips' fixture in `claude_lights`): 120 columns, the facts
/// published, the composer between its two rules.
fn claude_app() -> (App, WindowId, u64, crate::harness_host::HostHandle) {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert!(app.apply_term_resize(wid, 24, 120));
    let session = app.focused_session_id(wid).expect("the front session");
    let host = crate::harness_host::HostHandle::for_supervised_test(
        Vec::new(),
        std::sync::Arc::new(|_| crate::harness_host::BodyEnd::Stopped),
    );
    app.harness = Some(host.clone());
    {
        let entry = app.pool.get(session).expect("session");
        let mut timeline = entry.ctx.timeline.lock().unwrap();
        timeline.note_foreground_group(4242);
        assert!(timeline.set_claude_footer(
            4242,
            None,
            Some(FooterFacts {
                model: Some("Opus 5.5".into()),
                effort: Some("xhigh".into()),
                path: Some("~/aterm".into()),
                branch: Some("main".into()),
                ..FooterFacts::default()
            }),
        ));
    }
    draw_composer(&app, session);
    (app, wid, session, host)
}

fn feed(app: &App, session: u64, bytes: &[u8]) {
    let term = app.pool.get(session).expect("session").term.clone();
    term_lock(&term).process(bytes);
}

/// Claude Code's bottom block: the composer between two pane-wide rules.
fn draw_composer(app: &App, session: u64) {
    let rule = "\u{2500}".repeat(120);
    let screen = format!(
        "\x1b[?2004h\x1b[2J\x1b[19;1H\u{25CF} done\x1b[21;1H{rule}\x1b[22;1H\u{276F} \
         \x1b[23;1H{rule}\x1b[24;1H  \u{23F5}\u{23F5} bypass permissions on (shift+tab to cycle)\x1b[22;3H"
    );
    feed(app, session, screen.as_bytes());
}

/// One frame through the capture route's splices, the menu's last.
fn frame(app: &mut App, wid: WindowId) {
    frame_with(app, wid, |_| {});
}

/// [`frame`], with `before_menu` run over the frame just before the menu's
/// splice: where the overlays this frame published (the caret, the pet)
/// stand when the card is painted over them.
fn frame_with(app: &mut App, wid: WindowId, before_menu: impl FnOnce(&mut App)) {
    if let Some((plan, crate::VisibleContentRoute::Heterogeneous)) =
        app.active_visible_frame_layout(wid)
    {
        assert!(app.prepare_heterogeneous_input_scratch(wid).is_some());
        app.splice_claude_footer(wid, &plan, crate::VisibleContentRoute::Heterogeneous);
        before_menu(app);
        app.splice_autopilot_menu(wid);
        return;
    }
    let prepared = app.prepare_terminal_capture_grid_with_cursor_fx_and_plan_outcome(
        wid,
        crate::app_render::ComposedCursorFxClock::Advance(Instant::now()),
    );
    let crate::app_render::CapturePreparation::Ready((grid, plan)) = prepared else {
        panic!("the headless capture must produce a frame");
    };
    app.splice_tab_strip(wid);
    app.splice_claude_footer(
        wid,
        &plan,
        crate::VisibleContentRoute::Terminal {
            composed: grid.composed,
        },
    );
    before_menu(app);
    app.splice_autopilot_menu(wid);
}

/// The frame row's text.
fn row_text(app: &App, wid: WindowId, row: usize) -> String {
    app.windows[&wid].input_scratch.cells[row]
        .iter()
        .map(|c| c.ch)
        .collect()
}

/// THE MENU ON THE GLASS: opened from the chip (`open autopilot-menu`, as `↓`
/// on the selected chip), it paints above it and reads back through
/// `controls autopilot-menu` (its accessibility node:
/// `the_open_menu_is_published_in_words_its_highlighted_row_focused`); `↓`/`↑` move the
/// highlight and Return chooses (a timed Cruise armed, one frame each);
/// a right-click on the chip opens it; Esc closes it.
#[test]
fn the_menu_opens_from_its_chip_acts_and_closes_with_the_chip() {
    let (mut app, wid, session, host) = claude_app();
    let mono = Instant::now();
    let wall = 1_792_177_200_000_i64;
    crate::autopilot_timers::set_fake_clocks(Some((mono, wall)));
    frame(&mut app, wid);
    assert_eq!(app.autopilot_menu_controls(), ["autopilot-menu open=false"]);
    assert_eq!(
        super::fingerprint(app.autopilot_menu.as_ref(), wid),
        0,
        "0 at rest"
    );
    app.open_front_autopilot_menu()
        .expect("the chip is on the glass");
    let anchor = app.autopilot_menu.as_ref().unwrap().anchor;
    let open_fp = super::fingerprint(app.autopilot_menu.as_ref(), wid);
    assert_ne!(open_fp, 0, "the open repaints");
    frame(&mut app, wid);
    let rect = app
        .autopilot_menu
        .as_ref()
        .and_then(|m| m.rect)
        .expect("painted");
    assert_eq!(
        (rect.row + rect.h, rect.col),
        anchor,
        "hung above the chip, at its column"
    );
    assert!(row_text(&app, wid, rect.row + 2).contains("Cruise"));
    frame(&mut app, wid);
    assert!(app.autopilot_menu.is_some(), "the chip is there: it stays");
    let lines = app.autopilot_menu_controls();
    assert!(
        lines[0].starts_with("autopilot-menu open=true session=s-"),
        "{lines:?}"
    );
    assert!(
        lines.iter().any(|l| l
            .contains("key=autopilot-menu/mode/cruise enabled=true checked=true selected=true")),
        "{lines:?}"
    );
    for _ in 0..4 {
        assert!(app.autopilot_menu_nav(wid, MenuKey::Down));
    }
    assert_ne!(
        super::fingerprint(app.autopilot_menu.as_ref(), wid),
        open_fp,
        "a step repaints"
    );
    assert!(app.autopilot_menu_nav(wid, MenuKey::Up));
    assert!(app.autopilot_menu_nav(wid, MenuKey::Choose));
    assert!(app.autopilot_menu.is_none(), "chosen and closed");
    let land_at = app
        .pool
        .get(session)
        .unwrap()
        .ctx
        .meta
        .lock()
        .unwrap()
        .land_at;
    assert_eq!(land_at, Some(wall + 3_600_000), "Cruise 1 hour, then land");
    assert!(app.chip_glass(session).land_at.is_some());

    frame(&mut app, wid);
    let (x, y) = app.frame_cell_centre(wid, anchor);
    assert!(app.open_autopilot_menu_at(wid, x, y));
    assert!(
        app.autopilot_menu_controls()
            .iter()
            .any(|l| l.contains("key=autopilot-menu/land-cancel enabled=true")),
    );
    assert_eq!(
        app.autopilot_menu.as_ref().unwrap().highlight,
        None,
        "a pointer lights none"
    );
    assert!(app.autopilot_menu_nav(wid, MenuKey::Close));
    assert!(app.autopilot_menu.is_none());
    assert!(!app.autopilot_menu_nav(wid, MenuKey::Close), "nothing open");

    app.open_front_autopilot_menu().unwrap();
    assert!(!app.autopilot_menu_nav(wid, MenuKey::PassThrough));
    assert!(app.autopilot_menu.is_none());

    app.open_front_autopilot_menu().unwrap();
    frame(&mut app, wid);
    assert!(app.autopilot_menu.is_some(), "the same focus: it stays");
    app.autopilot_menu.as_mut().unwrap().focus = Some(session + 1);
    frame(&mut app, wid);
    assert!(app.autopilot_menu.is_none(), "the focus moved: closed");

    app.open_front_autopilot_menu().unwrap();
    frame(&mut app, wid);
    assert!(app.autopilot_menu.is_some());
    feed(
        &app,
        session,
        "\x1b[2J\x1b[18;1H Bash command\x1b[20;1H Do you want to proceed?\x1b[21;1H \u{276F} 1. Yes\x1b[22;1H   2. No"
            .as_bytes(),
    );
    frame(&mut app, wid);
    assert!(app.autopilot_menu.is_none(), "closed with its chip");
    assert_eq!(super::fingerprint(app.autopilot_menu.as_ref(), wid), 0);
    crate::autopilot_timers::set_fake_clocks(None);
    host.shutdown_and_join();
}

/// The menu closes with its window's focus (design §9.3.3) on the controller's
/// `focus out` too, which drives `App::on_focus` with no `WindowEvent::Focused`, as
/// every headless drive does; a focus the window already had keeps it.
#[test]
fn a_controller_focus_out_closes_the_menu_as_the_os_edge_does() {
    let (mut app, wid, _session, host) = claude_app();
    crate::autopilot_timers::set_fake_clocks(None);
    frame(&mut app, wid);
    app.open_front_autopilot_menu()
        .expect("the chip is on the glass");
    app.on_focus(wid, true);
    assert!(app.autopilot_menu.is_some(), "the same focus: it stays");
    app.on_focus(wid, false);
    assert!(app.autopilot_menu.is_none(), "focus out: closed");
    host.shutdown_and_join();
}

/// A CLICK ON A ROW acts for the menu's session: a mode is written as its
/// own word (the timer dropped, as every hand write drops it), Settings
/// opens Settings ▸ Harness; a press on the card off a choice keeps it
/// open; a press off the card closes it, spent.
#[test]
fn a_click_on_a_row_acts_and_a_click_off_the_card_closes_it() {
    let (mut app, wid, session, host) = claude_app();
    frame(&mut app, wid);
    crate::autopilot_timers::set_fake_clocks(None);
    assert!(app.choose_autopilot_timer(
        session,
        crate::autopilot_timers::TimerChoice::In(120),
        crate::autopilot::Surface::Menu
    ));
    frame(&mut app, wid);
    let press_row = |app: &mut App, key: &str| {
        let menu = app.autopilot_menu.as_ref().unwrap();
        let rect = menu.rect.unwrap();
        let i = menu
            .rows
            .iter()
            .position(|r| matches!(r, MenuRow::Item { act, .. } if act.key() == key))
            .unwrap();
        let (x, y) = app.frame_cell_centre(wid, (rect.row + 1 + i, rect.col + 3));
        app.autopilot_menu_press(wid, true, x, y)
    };
    app.open_front_autopilot_menu().unwrap();
    frame(&mut app, wid);
    let rect = app.autopilot_menu.as_ref().unwrap().rect.unwrap();
    let (x, y) = app.frame_cell_centre(wid, (rect.row + 1, rect.col + 3));
    assert!(app.autopilot_menu_press(wid, true, x, y));
    assert!(app.autopilot_menu.is_some());
    assert!(press_row(&mut app, "autopilot-menu/mode/off"));
    assert!(app.autopilot_menu.is_none());
    let meta = app
        .pool
        .get(session)
        .unwrap()
        .ctx
        .meta
        .lock()
        .unwrap()
        .clone();
    assert_eq!(
        (meta.autopilot.as_deref(), meta.land_at),
        (Some("off"), None)
    );
    app.open_front_autopilot_menu().unwrap();
    frame(&mut app, wid);
    let (x, y) = app.frame_cell_centre(wid, (0, 0));
    assert!(app.autopilot_menu_press(wid, true, x, y));
    assert!(app.autopilot_menu.is_none());
    app.harness_policy = crate::harness_policy_cell::HarnessPolicyCell::new(
        crate::app_config::HarnessPolicy::read("[harness]\nenabled = false\n"),
    );
    frame(&mut app, wid);
    app.open_front_autopilot_menu().unwrap();
    frame(&mut app, wid);
    assert!(
        app.autopilot_menu_controls()
            .iter()
            .any(|l| l.contains("text=\"harness off in Settings\""))
    );
    assert!(press_row(&mut app, "autopilot-menu/mode/cruise"));
    assert!(
        app.autopilot_menu.is_some(),
        "a disabled row keeps the card"
    );
    assert_eq!(
        app.pool
            .get(session)
            .unwrap()
            .ctx
            .meta
            .lock()
            .unwrap()
            .autopilot
            .as_deref(),
        Some("off"),
        "nothing written"
    );
    assert!(press_row(&mut app, "autopilot-menu/settings"));
    assert!(app.autopilot_menu.is_none());
    host.shutdown_and_join();
}

/// THE `land_at` ROW SAYS THE DAY IT WOULD ARM (§9.3.3; the Together run's
/// D6): at 18:49 local on a Friday, `[harness] land_at = "18:36"` has passed
/// today, so the row reads `Cruise until Sat 18:36, then land` (the day in
/// the zone this runs in), and choosing it arms a timer whose face says the
/// same words the state line does; `"18:55"` is still ahead, reads `Cruise
/// until 18:55, then land` and arms today.
#[test]
fn the_land_at_row_names_the_day_it_would_arm_as_the_state_line_does() {
    let (mut app, wid, session, host) = claude_app();
    let zone = crate::autopilot_timers::local_offset_at;
    let fri = 1_792_177_200_i64;
    let off = zone(fri).expect("the zone reads");
    let day = (fri + off).div_euclid(86_400);
    let now = day * 86_400 + 18 * 3_600 + 49 * 60 - off;
    assert_eq!(zone(now), Some(off), "no clock change between");
    crate::autopilot_timers::set_fake_clocks(Some((Instant::now(), now * 1000)));
    let tomorrow = Weekday::of_days(day + 1).short();
    let press_row = |app: &mut App, key: &str| {
        let menu = app.autopilot_menu.as_ref().unwrap();
        let rect = menu.rect.unwrap();
        let i = menu
            .rows
            .iter()
            .position(|r| matches!(r, MenuRow::Item { act, .. } if act.key() == key))
            .unwrap();
        let (x, y) = app.frame_cell_centre(wid, (rect.row + 1 + i, rect.col + 3));
        app.autopilot_menu_press(wid, true, x, y)
    };
    for (land_at, words, other_day) in [
        ("18:36", format!("{tomorrow} 18:36"), true),
        ("18:55", "18:55".to_string(), false),
    ] {
        app.harness_policy = crate::harness_policy_cell::HarnessPolicyCell::new(
            crate::app_config::HarnessPolicy::read(&format!(
                "[harness]\nland_at = \"{land_at}\"\n"
            )),
        );
        frame(&mut app, wid);
        app.open_front_autopilot_menu()
            .expect("the chip is on the glass");
        frame(&mut app, wid);
        let row = format!(
            "key=autopilot-menu/land-at enabled=true checked=false selected=false \
             text=\"\u{221e}\u{2913} Cruise until {words}, then land\""
        );
        let lines = app.autopilot_menu_controls();
        assert!(lines.iter().any(|l| l.ends_with(&row)), "{row}\n{lines:?}");
        assert!(press_row(&mut app, "autopilot-menu/land-at"));
        assert!(app.autopilot_menu.is_none(), "chosen and closed");
        let armed = app.chip_glass(session).land_at.expect("armed");
        assert_eq!(super::when(armed), words, "the state line's words");
        assert_eq!(armed.other_day.is_some(), other_day);
    }
    crate::autopilot_timers::set_fake_clocks(None);
    host.shutdown_and_join();
}

/// A MENU HELD OPEN ACROSS THE MINUTE SAYS THE DAY FROM THAT MINUTE ON
/// (§9.3.3; the review of the Together run's D6): opened at 18:30 on a
/// Friday with `land_at = "18:36"`, the row reads `18:36` and owes ONE wake,
/// at 18:36; there its words turn to `Sat 18:36` (what a press arms now), and
/// the next wake is local midnight, where they turn back to `18:36` (today is
/// Saturday).
#[test]
fn a_menu_held_open_across_the_minute_names_the_day_from_then_on() {
    let (mut app, wid, _session, host) = claude_app();
    let zone = crate::autopilot_timers::local_offset_at;
    let fri = 1_792_177_200_i64;
    let off = zone(fri).expect("the zone reads");
    let day = (fri + off).div_euclid(86_400);
    let local = |days: i64, hh: i64, mm: i64| (days * 86_400 + hh * 3_600 + mm * 60 - off) * 1000;
    let opened = local(day, 18, 30);
    assert_eq!(
        zone(local(day + 1, 18, 36) / 1000),
        Some(off),
        "no clock change between"
    );
    let t0 = Instant::now();
    let mono = |wall: i64| {
        t0 + std::time::Duration::from_millis(u64::try_from(wall - opened).expect("ahead"))
    };
    let clocks = |wall: i64| crate::autopilot_timers::set_fake_clocks(Some((mono(wall), wall)));
    clocks(opened);
    app.harness_policy = crate::harness_policy_cell::HarnessPolicyCell::new(
        crate::app_config::HarnessPolicy::read("[harness]\nland_at = \"18:36\"\n"),
    );
    let row = |app: &App| {
        app.autopilot_menu_controls()
            .into_iter()
            .find(|l| l.contains("key=autopilot-menu/land-at"))
            .expect("the land_at row")
    };
    let says = |words: &str| format!("text=\"\u{221e}\u{2913} Cruise until {words}, then land\"");
    frame(&mut app, wid);
    app.open_front_autopilot_menu()
        .expect("the chip is on the glass");
    frame(&mut app, wid);
    assert!(row(&app).ends_with(&says("18:36")), "{}", row(&app));
    assert_eq!(
        app.autopilot_offer_deadline(),
        Some(mono(local(day, 18, 36))),
        "one wake, at 18:36"
    );

    clocks(local(day, 18, 36) - 1);
    app.expire_autopilot_offer();
    assert!(row(&app).ends_with(&says("18:36")), "short of the turn");

    clocks(local(day, 18, 36));
    app.expire_autopilot_offer();
    frame(&mut app, wid);
    let sat = Weekday::of_days(day + 1).short();
    assert!(
        row(&app).ends_with(&says(&format!("{sat} 18:36"))),
        "{}",
        row(&app)
    );
    assert_eq!(
        app.autopilot_offer_deadline(),
        Some(mono(local(day + 1, 0, 0))),
        "then midnight"
    );

    clocks(local(day + 1, 0, 0));
    app.expire_autopilot_offer();
    frame(&mut app, wid);
    assert!(row(&app).ends_with(&says("18:36")), "{}", row(&app));

    assert!(app.close_autopilot_menu());
    assert_eq!(
        app.autopilot_offer_deadline(),
        None,
        "no wake for a closed menu"
    );
    app.expire_autopilot_offer();
    assert_eq!(app.autopilot_timers.offer_due(), None, "the note dropped");
    crate::autopilot_timers::set_fake_clocks(None);
    host.shutdown_and_join();
}

/// THE MENU'S NAME DROPS THE AGENT'S SPINNER FRAME (the Together run's
/// D7): built once at the open, it names the tab by the agent's title
/// without the frame it froze (`⠦ Add tested …` reads `Add tested …`).
#[test]
fn the_menus_name_drops_the_agents_spinner_frame() {
    let (mut app, wid, session, host) = claude_app();
    frame(&mut app, wid);
    for (title, name) in [
        (
            "\u{2826} Add tested calculator functions",
            "Add tested calculator functions",
        ),
        (
            "\u{2733} Calc.py small tested module",
            "\u{2733} Calc.py small tested module",
        ),
    ] {
        app.pool
            .get(session)
            .expect("the agent's pane")
            .term
            .lock()
            .unwrap()
            .process(format!("\x1b]2;{title}\x07").as_bytes());
        app.open_front_autopilot_menu()
            .expect("the chip is on the glass");
        let labels: Vec<String> = app
            .autopilot_menu_controls()
            .into_iter()
            .filter(|l| l.starts_with("autopilot-menu label"))
            .collect();
        assert!(
            labels
                .iter()
                .take(2)
                .any(|l| l.ends_with(&format!("\u{201c}{name}\u{201d}\""))),
            "{title:?}: {labels:?}"
        );
        assert!(app.autopilot_menu_nav(wid, MenuKey::Close));
    }
    host.shutdown_and_join();
}

/// A synthetic winit key press and release, as a person types it.
#[cfg(target_os = "macos")]
fn tap(
    app: &mut App,
    wid: WindowId,
    key: aterm_window::keyboard::Key,
    code: aterm_window::keyboard::KeyCode,
    text: Option<&str>,
) {
    use aterm_window::event::ElementState;
    use aterm_window::keyboard::{KeyLocation, PhysicalKey, SmolStr};
    for state in [ElementState::Pressed, ElementState::Released] {
        app.on_key(
            wid,
            aterm_window::event::KeyEvent::synthetic_for_test(
                PhysicalKey::Code(code),
                key.clone(),
                text.map(SmolStr::new),
                KeyLocation::Standard,
                state,
                false,
            ),
        );
    }
}

/// The chip's first painted cell, scanned from this frame's glass.
fn chip_cell(app: &App, wid: WindowId, session: u64) -> (usize, usize) {
    let ws = &app.windows[&wid];
    (0..ws.input_scratch.cells.len())
        .flat_map(|row| (0..ws.input_scratch.cols).map(move |col| (row, col)))
        .find(|&cell| app.shows_autopilot_chip(wid, session, cell))
        .expect("the Autopilot chip is on the glass")
}

/// THE POINTER, through the real mouse path (`on_cursor_moved`,
/// `on_mouse_input`): a RIGHT press on the Autopilot chip opens the menu hung
/// from it, no row lit, and cycles nothing; the pointer over a row lights it;
/// a left press there chooses it (Off, written as the session's word) and
/// closes; a right press reopens it, and a press off the card closes it and
/// acts on nothing under it.
#[test]
fn the_pointer_opens_the_menu_from_the_chip_and_chooses_a_row() {
    use aterm_window::event::{ElementState, MouseButton};
    let (mut app, wid, session, host) = claude_app();
    frame(&mut app, wid);
    let chip = chip_cell(&app, wid, session);
    let (x, y) = app.frame_cell_centre(wid, chip);
    let word = |app: &App| {
        app.pool
            .get(session)
            .unwrap()
            .ctx
            .meta
            .lock()
            .unwrap()
            .autopilot
            .clone()
    };
    app.on_cursor_moved(wid, x, y);
    app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Right);
    app.on_mouse_input(wid, ElementState::Released, MouseButton::Right);
    let menu = app.autopilot_menu.as_ref().expect("a right press opens it");
    assert_eq!(
        (menu.session, menu.anchor, menu.highlight),
        (session, chip, None)
    );
    assert_eq!(word(&app), None, "the right press cycles nothing");
    frame(&mut app, wid);
    let rect = app.autopilot_menu.as_ref().unwrap().rect.expect("painted");
    let off_row = rect.row + 1 + 3;
    let (ox, oy) = app.frame_cell_centre(wid, (off_row, rect.col + 3));
    app.on_cursor_moved(wid, ox, oy);
    assert_eq!(
        app.autopilot_menu.as_ref().unwrap().highlight,
        Some(3),
        "Off lit"
    );
    app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
    app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
    assert!(app.autopilot_menu.is_none(), "chosen and closed");
    assert_eq!(word(&app).as_deref(), Some("off"));
    frame(&mut app, wid);
    let chip = chip_cell(&app, wid, session);
    let (x, y) = app.frame_cell_centre(wid, chip);
    app.on_cursor_moved(wid, x, y);
    app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Right);
    app.on_mouse_input(wid, ElementState::Released, MouseButton::Right);
    assert!(app.autopilot_menu.is_some());
    frame(&mut app, wid);
    app.on_cursor_moved(wid, x, y);
    app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
    app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
    assert!(
        app.autopilot_menu.is_none(),
        "a press off the card closes it"
    );
    assert_eq!(word(&app).as_deref(), Some("off"), "and is spent on that");
    frame(&mut app, wid);
    app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
    app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
    assert!(app.autopilot_menu.is_none(), "a click opens no menu");
    assert_eq!(word(&app).as_deref(), Some("cruise"), "off → cruise");
    host.shutdown_and_join();
}

/// THE KEYBOARD (macOS: `ctrl+shift+tab` selects the chips): with the
/// Autopilot chip selected, `↓` opens the menu, its first choice lit; `↓`
/// steps, Return chooses; shift+F10 opens it too; Esc closes it; a key it
/// does not own closes it and goes on to the pane.
#[cfg(target_os = "macos")]
#[test]
fn the_keyboard_opens_the_menu_from_the_selected_chip() {
    use aterm_window::keyboard::{Key, KeyCode, ModifiersState, NamedKey};
    let (mut app, wid, session, host) = claude_app();
    frame(&mut app, wid);
    let down = |app: &mut App| {
        tap(
            app,
            wid,
            Key::Named(NamedKey::ArrowDown),
            KeyCode::ArrowDown,
            None,
        );
    };
    down(&mut app);
    assert!(app.autopilot_menu.is_none(), "no chip selected: no menu");
    let select = |app: &mut App| {
        app.windows.get_mut(&wid).unwrap().mods = ModifiersState::CONTROL | ModifiersState::SHIFT;
        tap(
            app,
            wid,
            Key::Named(NamedKey::Tab),
            KeyCode::Tab,
            Some("\t"),
        );
        app.windows.get_mut(&wid).unwrap().mods = ModifiersState::empty();
    };
    select(&mut app);
    frame(&mut app, wid);
    down(&mut app);
    let menu = app.autopilot_menu.as_ref().expect("↓ opens it");
    assert_eq!(
        (menu.session, menu.highlight),
        (session, Some(1)),
        "Cruise lit"
    );
    frame(&mut app, wid);
    down(&mut app);
    assert_eq!(app.autopilot_menu.as_ref().unwrap().highlight, Some(2));
    tap(
        &mut app,
        wid,
        Key::Named(NamedKey::Escape),
        KeyCode::Escape,
        None,
    );
    assert!(app.autopilot_menu.is_none(), "Esc closes it");
    select(&mut app);
    frame(&mut app, wid);
    app.windows.get_mut(&wid).unwrap().mods = ModifiersState::SHIFT;
    tap(&mut app, wid, Key::Named(NamedKey::F10), KeyCode::F10, None);
    app.windows.get_mut(&wid).unwrap().mods = ModifiersState::empty();
    assert!(app.autopilot_menu.is_some(), "shift+F10 opens it");
    tap(
        &mut app,
        wid,
        Key::Character("x".into()),
        KeyCode::KeyX,
        Some("x"),
    );
    assert!(app.autopilot_menu.is_none(), "a letter closes it");
    app.open_front_autopilot_menu().unwrap();
    frame(&mut app, wid);
    let key = |name: aterm_types::keyboard::NamedKey| crate::input::InputEvent::Key {
        key: aterm_types::keyboard::Key::Named(name),
        mods: aterm_types::keyboard::Modifiers::empty(),
        base_layout: None,
        layout_text: None,
        event_type: aterm_types::keyboard::KeyEventType::Press,
    };
    assert!(app.autopilot_menu_input_event(wid, &key(aterm_types::keyboard::NamedKey::ArrowDown)));
    assert!(app.autopilot_menu_input_event(wid, &key(aterm_types::keyboard::NamedKey::Enter)));
    assert!(app.autopilot_menu.is_none());
    assert_eq!(
        app.pool
            .get(session)
            .unwrap()
            .ctx
            .meta
            .lock()
            .unwrap()
            .autopilot
            .as_deref(),
        Some("land"),
        "Land now"
    );
    host.shutdown_and_join();
}

/// The stored pair of `session`: its word and its timer.
fn pair(app: &App, session: u64) -> (Option<String>, Option<i64>) {
    let meta = app.pool.get(session).unwrap().ctx.meta.lock().unwrap();
    (meta.autopilot.clone(), meta.land_at)
}

/// `meta <rest>` over the wire on `session`, as the dispatch arm runs it:
/// stored, its `MetaChanged` still in flight (not applied).
fn wire(app: &App, session: u64, rest: &str) {
    let entry = app.pool.get(session).unwrap();
    let (term, ctx) = (entry.term.clone(), entry.ctx.clone());
    let (reply, _) = crate::control::cmd_meta(&term, &app.store, session, &ctx, rest);
    assert!(reply.starts_with("OK"), "{rest}: {reply}");
}

/// The open menu's row index for the choice `key`.
fn row_of(app: &App, key: &str) -> Option<usize> {
    app.autopilot_menu.as_ref().and_then(|menu| {
        menu.rows
            .iter()
            .position(|r| matches!(r, MenuRow::Item { act, .. } if act.key() == key))
    })
}

/// AN OPEN MENU NEVER GOES STALE, AND ITS CANCEL NEVER WRITES OVER A HAND
/// (review of slice B).
#[test]
fn an_open_menu_follows_its_session_and_its_cancel_never_writes_over_a_hand() {
    let (mut app, wid, session, host) = claude_app();
    let mono = Instant::now();
    let wall = 1_792_177_200_000_i64;
    crate::autopilot_timers::set_fake_clocks(Some((mono, wall)));
    frame(&mut app, wid);
    let cancel = "autopilot-menu/land-cancel";
    let lines = |app: &App| app.autopilot_menu_controls().join("\n");

    assert!(app.choose_autopilot_timer(
        session,
        crate::autopilot_timers::TimerChoice::In(120),
        crate::autopilot::Surface::Menu
    ));
    frame(&mut app, wid);
    app.open_front_autopilot_menu().unwrap();
    frame(&mut app, wid);
    let at = row_of(&app, cancel).expect("the cancel is offered while armed");
    app.autopilot_menu.as_mut().unwrap().highlight = Some(at);
    wire(&app, session, "set autopilot off");
    app.on_meta_changed(session);
    let shown = lines(&app);
    assert!(app.autopilot_menu.is_some(), "still open: {shown}");
    assert!(
        shown.contains("key=autopilot-menu/mode/off enabled=true checked=true"),
        "{shown}"
    );
    assert!(
        shown.contains("key=autopilot-menu/mode/cruise enabled=true checked=false"),
        "{shown}"
    );
    assert!(!shown.contains(cancel), "no timer, no cancel: {shown}");
    assert_eq!(
        app.autopilot_menu.as_ref().unwrap().highlight,
        None,
        "the lit cancel went dark"
    );
    assert!(app.autopilot_menu_nav(wid, MenuKey::Choose));
    assert!(app.autopilot_menu.is_none(), "Return closed it");
    assert_eq!(
        pair(&app, session),
        (Some("off".into()), None),
        "and acted on nothing"
    );

    assert!(app.choose_autopilot_timer(
        session,
        crate::autopilot_timers::TimerChoice::In(120),
        crate::autopilot::Surface::Menu
    ));
    frame(&mut app, wid);
    app.open_front_autopilot_menu().unwrap();
    let at = row_of(&app, cancel).expect("offered");
    wire(&app, session, "set autopilot off");
    app.choose_autopilot_menu_row(at);
    assert!(app.autopilot_menu.is_none());
    assert_eq!(
        pair(&app, session),
        (Some("off".into()), None),
        "the hand wins"
    );

    assert!(app.choose_autopilot_timer(
        session,
        crate::autopilot_timers::TimerChoice::In(60),
        crate::autopilot::Surface::Menu
    ));
    frame(&mut app, wid);
    app.open_front_autopilot_menu().unwrap();
    let at = row_of(&app, cancel).expect("offered");
    wire(&app, session, "set autopilot cruise until=+4h");
    app.choose_autopilot_menu_row(at);
    assert_eq!(
        pair(&app, session),
        (Some("cruise".into()), Some(wall + 4 * 3_600_000)),
        "the new arm stands"
    );
    app.on_meta_changed(session);
    frame(&mut app, wid);
    app.open_front_autopilot_menu().unwrap();
    let at = row_of(&app, cancel).expect("offered");
    app.choose_autopilot_menu_row(at);
    assert_eq!(
        pair(&app, session),
        (Some("cruise".into()), None),
        "cancelled"
    );

    assert!(app.choose_autopilot_timer(
        session,
        crate::autopilot_timers::TimerChoice::In(60),
        crate::autopilot::Surface::Menu
    ));
    frame(&mut app, wid);
    app.open_front_autopilot_menu().unwrap();
    assert!(row_of(&app, cancel).is_some());
    crate::autopilot_timers::set_fake_clocks(Some((
        mono + std::time::Duration::from_secs(3_600),
        wall + 3_600_000,
    )));
    app.expire_autopilot_timers();
    assert_eq!(pair(&app, session), (Some("land".into()), None), "landed");
    let shown = lines(&app);
    assert!(
        shown.contains("key=autopilot-menu/mode/land enabled=true checked=true"),
        "{shown}"
    );
    assert!(!shown.contains(cancel), "{shown}");
    let cancel_now = |app: &mut App| {
        app.choose_autopilot_timer(
            session,
            crate::autopilot_timers::TimerChoice::Cancel,
            crate::autopilot::Surface::Menu,
        )
    };
    assert!(!cancel_now(&mut app));
    assert_eq!(pair(&app, session), (Some("land".into()), None));
    assert!(app.set_session_autopilot(
        session,
        Some(Autopilot::Off),
        crate::autopilot::Surface::Menu
    ));
    assert!(!cancel_now(&mut app));
    assert_eq!(pair(&app, session), (Some("off".into()), None));
    assert!(app.set_session_autopilot(
        session,
        Some(Autopilot::Cruise),
        crate::autopilot::Surface::Menu
    ));
    assert!(!cancel_now(&mut app));
    assert_eq!(pair(&app, session), (Some("cruise".into()), None));
    crate::autopilot_timers::set_fake_clocks(None);
    host.shutdown_and_join();
}

/// THE OPEN MENU IS PUBLISHED IN WORDS, ITS HIGHLIGHTED ROW FOCUSED (design
/// §9.2.1, §9.3.3): the tree the window publishes (`App::grid_a11y_tree`,
/// what `push_a11y_tree` hands the adapter) holds, once a frame has painted
/// the menu, one `Role::Menu` named in words (`Autopilot, tab 1…`) with one
/// item per choice the card shows, each in words — Cruise, Land now and Off
/// radios, the session's mode checked; the timed Cruises and Harness
/// settings plain items — over the card's cells, and the tree's focus on the
/// highlighted row, which follows `↓`.
#[cfg(a11y_tree)]
#[test]
fn the_open_menu_is_published_in_words_its_highlighted_row_focused() {
    use aterm_a11y::model::{Action, ActionRequest, NodeId, Role, Toggled, TreeId, TreeUpdate};
    let menu_nodes = |update: &TreeUpdate| -> Vec<NodeId> {
        update
            .nodes
            .iter()
            .filter(|(_, n)| matches!(n.role(), Role::Menu | Role::MenuItem | Role::MenuItemRadio))
            .map(|(id, _)| *id)
            .collect()
    };
    let node = |update: &TreeUpdate, id: NodeId| {
        update
            .nodes
            .iter()
            .find(|(n, _)| *n == id)
            .map(|(_, n)| n.clone())
            .expect("the node")
    };
    let words = |text: &str| {
        text.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '.' | ':' | ','))
    };
    let (mut app, wid, session, host) = claude_app();
    let _host = MenuTestHost(host);
    frame(&mut app, wid);
    let bare = app.grid_a11y_tree(wid, None).expect("the grid's tree");
    assert!(menu_nodes(&bare).is_empty(), "no menu, no menu node");
    assert_eq!(crate::a11y_tree::menu_item_for(bare.focus), None);
    app.open_front_autopilot_menu().unwrap();
    let unpainted = app.grid_a11y_tree(wid, None).unwrap();
    assert!(
        menu_nodes(&unpainted).is_empty(),
        "not on the glass yet: not in the tree"
    );
    frame(&mut app, wid);
    let rect = app.autopilot_menu.as_ref().unwrap().rect.expect("painted");
    let tree = app.grid_a11y_tree(wid, None).unwrap();
    let menu = tree
        .nodes
        .iter()
        .find(|(_, n)| n.role() == Role::Menu)
        .map(|(_, n)| n.clone())
        .expect("the menu is published");
    let label = menu.label().expect("named");
    assert!(label.starts_with("Autopilot, tab 1"), "{label}");
    assert!(
        !label.contains('\u{b7}') && !label.contains('\u{201c}'),
        "{label}"
    );
    let items: Vec<_> = menu
        .children()
        .iter()
        .map(|id| (*id, node(&tree, *id)))
        .collect();
    let said: Vec<&str> = items.iter().map(|(_, n)| n.label().unwrap()).collect();
    assert_eq!(
        said,
        [
            "Cruise",
            "Land now",
            "Off",
            "Cruise for 1 hour, then land",
            "Cruise for 2 hours, then land",
            "Cruise for 4 hours, then land",
            "Harness settings",
        ]
    );
    for (i, (id, n)) in items.iter().enumerate() {
        let label = n.label().unwrap();
        assert!(words(label), "in words: {label}");
        assert_eq!(
            crate::a11y_tree::menu_item_for(*id).map(|(_, item)| item),
            Some(i)
        );
        assert!(
            !n.is_disabled() && n.supports_action(Action::Click),
            "{label}"
        );
        if i < 3 {
            assert_eq!(n.role(), Role::MenuItemRadio, "{label}");
            let want = if i == 0 {
                Toggled::True
            } else {
                Toggled::False
            };
            assert_eq!(
                n.toggled(),
                Some(want),
                "{label}: the session's mode checked"
            );
        } else {
            assert_eq!(n.role(), Role::MenuItem, "{label}");
            assert_eq!(n.toggled(), None, "{label}");
        }
    }
    if let (Some(menu_box), Some(first)) = (menu.bounds(), items[0].1.bounds()) {
        let cell_h = (menu_box.y1 - menu_box.y0) / rect.h as f64;
        assert!(
            (first.y0 - (menu_box.y0 + cell_h * 2.0)).abs() < 0.5,
            "Cruise is the card's third row"
        );
    }
    assert_eq!(tree.focus, items[0].0, "the highlighted row: Cruise");
    assert!(app.autopilot_menu_nav(wid, MenuKey::Down));
    frame(&mut app, wid);
    let stepped = app.grid_a11y_tree(wid, None).unwrap();
    assert_eq!(stepped.focus, items[1].0, "↓: Land now");
    app.on_accessibility_action(
        wid,
        ActionRequest {
            context: None,
            action: Action::Click,
            target_tree: TreeId::ROOT,
            target_node: items[1].0,
            data: None,
        },
    );
    assert!(app.autopilot_menu.is_none(), "chosen and closed");
    assert_eq!(pair(&app, session).0.as_deref(), Some("land"), "Land now");
    frame(&mut app, wid);
    let closed = app.grid_a11y_tree(wid, None).unwrap();
    assert!(menu_nodes(&closed).is_empty(), "closed: no menu node");
    assert_eq!(crate::a11y_tree::menu_item_for(closed.focus), None);
    app.harness_policy = crate::harness_policy_cell::HarnessPolicyCell::new(
        crate::app_config::HarnessPolicy::read("[harness]\nenabled = false\n"),
    );
    frame(&mut app, wid);
    app.open_front_autopilot_menu().unwrap();
    frame(&mut app, wid);
    let off = app.grid_a11y_tree(wid, None).unwrap();
    let menu = off
        .nodes
        .iter()
        .find(|(_, n)| n.role() == Role::Menu)
        .map(|(_, n)| n.clone())
        .expect("the menu is published");
    assert!(
        menu.label().unwrap().ends_with(", harness off in Settings"),
        "{:?}",
        menu.label()
    );
    let items: Vec<_> = menu
        .children()
        .iter()
        .map(|id| (*id, node(&off, *id)))
        .collect();
    let (settings, rest) = items.split_last().unwrap();
    assert_eq!(settings.1.label(), Some("Harness settings"));
    assert!(!settings.1.is_disabled());
    for (_, n) in rest {
        assert!(n.is_disabled(), "{:?}", n.label());
        assert!(!n.supports_action(Action::Click), "{:?}", n.label());
        assert_ne!(n.toggled(), Some(Toggled::True), "nothing checked");
    }
    app.on_accessibility_action(
        wid,
        ActionRequest {
            context: None,
            action: Action::Click,
            target_tree: TreeId::ROOT,
            target_node: rest[0].0,
            data: None,
        },
    );
    assert!(
        app.autopilot_menu.is_some(),
        "a disabled row keeps the card"
    );
    assert_eq!(
        pair(&app, session).0.as_deref(),
        Some("land"),
        "nothing written"
    );
}

/// The open menu's items as window `wid` publishes them (`App::grid_a11y_tree`):
/// each item's id and its words, top down; empty with no menu node.
#[cfg(a11y_tree)]
fn published_items(app: &App, wid: WindowId) -> Vec<(aterm_a11y::model::NodeId, String)> {
    let Some(tree) = app.grid_a11y_tree(wid, None) else {
        return Vec::new();
    };
    let label = |id: &aterm_a11y::model::NodeId| {
        tree.nodes
            .iter()
            .find(|(n, _)| n == id)
            .and_then(|(_, n)| n.label())
            .unwrap_or_default()
            .to_string()
    };
    tree.nodes
        .iter()
        .find(|(_, n)| n.role() == aterm_a11y::model::Role::Menu)
        .map(|(_, menu)| menu.children().iter().map(|id| (*id, label(id))).collect())
        .unwrap_or_default()
}

/// A screen reader's `action` on node `node` of window `wid`'s tree.
#[cfg(a11y_tree)]
fn reader(
    app: &mut App,
    wid: WindowId,
    action: aterm_a11y::model::Action,
    node: aterm_a11y::model::NodeId,
) {
    app.on_accessibility_action(
        wid,
        aterm_a11y::model::ActionRequest {
            context: None,
            action,
            target_tree: aterm_a11y::model::TreeId::ROOT,
            target_node: node,
            data: None,
        },
    );
}

/// A READER'S CLICK HEARD FROM ROWS THAT MOVED CHOOSES NOTHING.
#[cfg(a11y_tree)]
#[test]
fn a_readers_click_heard_from_rows_that_moved_chooses_nothing() {
    use aterm_a11y::model::Action;
    let (mut app, wid, session, host) = claude_app();
    let _host = MenuTestHost(host);
    crate::autopilot_timers::set_fake_clocks(Some((Instant::now(), 1_792_177_200_000)));
    let named = |items: &[(aterm_a11y::model::NodeId, String)], prefix: &str| {
        items
            .iter()
            .find(|(_, label)| label.starts_with(prefix))
            .map(|(id, _)| *id)
            .unwrap_or_else(|| panic!("no item {prefix}: {items:?}"))
    };
    frame(&mut app, wid);
    app.open_front_autopilot_menu().unwrap();
    frame(&mut app, wid);
    let heard = published_items(&app, wid);
    let settings = named(&heard, "Harness settings");
    assert_eq!(heard.last().map(|(id, _)| *id), Some(settings), "{heard:?}");

    wire(&app, session, "set autopilot cruise until=+4h");
    app.on_meta_changed(session);
    let armed = pair(&app, session);
    assert!(
        armed.0.as_deref() == Some("cruise") && armed.1.is_some(),
        "{armed:?}"
    );
    assert!(
        row_of(&app, "autopilot-menu/land-cancel").is_some(),
        "rebuilt"
    );
    reader(&mut app, wid, Action::Click, settings);
    assert!(
        app.autopilot_menu.is_some(),
        "unpainted rows: nothing chosen"
    );
    assert_eq!(pair(&app, session), armed, "the timer stands");
    frame(&mut app, wid);
    let now = published_items(&app, wid);
    assert_eq!(now.len(), heard.len() + 1, "{now:?}");
    assert!(
        now[heard.len() - 1].1.starts_with("Cancel landing"),
        "the cancel took Settings' place: {now:?}"
    );
    assert!(
        now.iter()
            .all(|(id, _)| heard.iter().all(|(old, _)| old != id)),
        "every id names the new rows"
    );
    reader(&mut app, wid, Action::Click, settings);
    assert!(
        app.autopilot_menu.is_some(),
        "a click heard from the old rows is refused"
    );
    assert_eq!(pair(&app, session), armed, "the timer someone armed stands");

    let cancel = named(&now, "Cancel landing");
    let other = WindowId(wid.0 + 1);
    assert!(app.autopilot_grid_menu(other).is_none());
    reader(&mut app, other, Action::Click, cancel);
    assert!(app.autopilot_menu.is_some(), "another window's: nothing");
    assert_eq!(pair(&app, session), armed);
    {
        let menu = app.autopilot_menu.as_mut().unwrap();
        let rect = menu.rect.expect("painted");
        menu.rect = Some(MenuRect { h: 2 + 3, ..rect });
    }
    reader(&mut app, wid, Action::Click, cancel);
    assert!(
        app.autopilot_menu.is_some(),
        "a row it does not show: refused"
    );
    assert_eq!(pair(&app, session), armed);

    frame(&mut app, wid);
    assert_eq!(
        published_items(&app, wid),
        now,
        "the same rows, the same ids"
    );
    reader(&mut app, wid, Action::Click, cancel);
    assert!(app.autopilot_menu.is_none(), "chosen and closed");
    assert_eq!(
        pair(&app, session),
        (Some("cruise".into()), None),
        "cancelled"
    );
}

/// A READER OPENS THE MENU FROM THE CHIP (design §9.3.3):
/// the Autopilot
/// chip's node says it has a menu (`HasPopup::Menu`) and offers
/// `ShowContextMenu`, which opens it as the keyboard does — its first
/// choice lit — so once a frame has painted it the tree's focus is Cruise.
#[cfg(a11y_tree)]
#[test]
fn a_reader_opens_the_menu_from_the_autopilot_chip() {
    use crate::a11y_tree::{HarnessChip, chip_for};
    use aterm_a11y::model::{Action, HasPopup};
    let (mut app, wid, session, host) = claude_app();
    let _host = MenuTestHost(host);
    frame(&mut app, wid);
    let chip = |app: &App, which: HarnessChip| {
        let tree = app.grid_a11y_tree(wid, None).expect("the grid's tree");
        tree.nodes
            .iter()
            .find(|(id, _)| chip_for(*id) == Some((session, which)))
            .map(|(id, node)| (*id, node.clone()))
            .expect("the chip is published")
    };
    let (autopilot, node) = chip(&app, HarnessChip::Autopilot);
    assert_eq!(node.has_popup(), Some(HasPopup::Menu));
    assert!(node.supports_action(Action::ShowContextMenu));
    assert!(node.supports_action(Action::Click));

    let before = pair(&app, session).0;
    reader(&mut app, wid, Action::Click, autopilot);
    assert!(app.autopilot_menu.is_none(), "a click opens no menu");
    let after = pair(&app, session).0;
    assert!(after.is_some() && after != before, "{before:?} → {after:?}");
    frame(&mut app, wid);

    reader(&mut app, wid, Action::ShowContextMenu, autopilot);
    let menu = app.autopilot_menu.as_ref().expect("opened");
    assert_eq!(menu.session, session);
    assert!(
        matches!(menu.rows.get(1), Some(MenuRow::SetBy { text, .. }) if text.ends_with(" from the chip")),
        "{:?}",
        menu.rows.get(1)
    );
    assert_eq!(
        menu.highlight,
        Some(2),
        "the first choice lit, as a keyboard open"
    );
    frame(&mut app, wid);
    let tree = app.grid_a11y_tree(wid, None).unwrap();
    let items = published_items(&app, wid);
    assert_eq!(items.first().map(|(_, l)| l.as_str()), Some("Cruise"));
    assert_eq!(tree.focus, items[0].0, "the focus is the lit row");
    assert_eq!(pair(&app, session).0, after, "opening writes nothing");
    assert!(app.autopilot_menu_nav(wid, MenuKey::Close));

    feed(
        &app,
        session,
        "\x1b[2J\x1b[18;1H Bash command\x1b[20;1H Do you want to proceed?\x1b[21;1H \u{276F} 1. \
         Yes\x1b[22;1H   2. No"
            .as_bytes(),
    );
    frame(&mut app, wid);
    reader(&mut app, wid, Action::ShowContextMenu, autopilot);
    assert!(
        app.autopilot_menu.is_none(),
        "no chip on the glass: no menu"
    );
}

/// Each new fixture owns its inert host and injected clocks on assertion unwind.
struct MenuTestHost(crate::harness_host::HostHandle);

impl Drop for MenuTestHost {
    fn drop(&mut self) {
        crate::autopilot_timers::set_fake_clocks(None);
        self.0.shutdown_and_join();
    }
}

/// THE MIXED TAB: Settings split beside a stub terminal pane that runs
/// Claude Code's composer (its footer facts, its bottom block), framed once
/// on the heterogeneous route.
fn beside_settings(app: &mut App, wid: WindowId) -> u64 {
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    assert!(app.apply_term_resize(wid, 60, 120));
    let (session, _) =
        app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Vertical);
    let entry = app.pool.get(session).expect("the new stub terminal");
    let mut timeline = entry.ctx.timeline.lock().unwrap();
    timeline.note_foreground_group(4242);
    assert!(timeline.set_claude_footer(
        4242,
        None,
        Some(FooterFacts {
            model: Some("Opus 5.5".into()),
            effort: Some("xhigh".into()),
            path: Some("~/aterm".into()),
            branch: Some("main".into()),
            ..FooterFacts::default()
        }),
    ));
    drop(timeline);
    let (rows, cols) = {
        let term = term_lock(&entry.term);
        (usize::from(term.rows()), usize::from(term.cols()))
    };
    assert!(
        rows >= 8 && cols >= 80,
        "the real terminal pane fits the composer"
    );
    let rule = "\u{2500}".repeat(cols);
    let screen = format!(
        "\x1b[?2004h\x1b[2J\x1b[{};1H\u{25CF} done\x1b[{};1H{rule}\x1b[{};1H\u{276F} \
         \x1b[{};1H{rule}\x1b[{};1H  \u{23F5}\u{23F5} bypass permissions on (shift+tab to cycle)\x1b[{};3H",
        rows - 5,
        rows - 3,
        rows - 2,
        rows - 1,
        rows,
        rows - 2,
    );
    feed(app, session, screen.as_bytes());
    frame(app, wid);
    assert!(matches!(
        app.active_visible_frame_layout(wid),
        Some((_, crate::VisibleContentRoute::Heterogeneous))
    ));
    session
}

/// macOS secondary activation has the same meaning on terminal-only and
/// heterogeneous tabs; an ordinary press still cycles, and a non-chip does not
/// open the menu.
#[cfg(target_os = "macos")]
#[test]
fn a_control_click_opens_the_menu_in_terminal_and_mixed_tabs() {
    use aterm_window::event::{ElementState, MouseButton};
    use aterm_window::keyboard::ModifiersState;
    for mixed in [false, true] {
        let (mut app, wid, mut session, host) = claude_app();
        let _host = MenuTestHost(host);
        if mixed {
            session = beside_settings(&mut app, wid);
        } else {
            frame(&mut app, wid);
        }
        eprintln!("Ctrl-click fixture: mixed={mixed}, initial composed chip");
        let chip = chip_cell(&app, wid, session);
        let (x, y) = app.frame_cell_centre(wid, chip);
        app.windows.get_mut(&wid).unwrap().mods = ModifiersState::empty();
        let before = pair(&app, session);
        app.on_cursor_moved(wid, x, y);
        app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
        app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
        assert!(
            app.autopilot_menu.is_none(),
            "an ordinary press cycles: mixed={mixed}"
        );
        assert_ne!(
            pair(&app, session),
            before,
            "the real chip acts: mixed={mixed}"
        );
        assert!(app.set_session_autopilot(session, None, crate::autopilot::Surface::Menu));
        frame(&mut app, wid);
        eprintln!("Ctrl-click fixture: mixed={mixed}, restored composed chip");
        let chip = chip_cell(&app, wid, session);
        let (x, y) = app.frame_cell_centre(wid, chip);

        let off_chip = app.frame_cell_centre(wid, (chip.0 - 3, chip.1));
        assert!(app.claude_chips_at(wid, off_chip.0, off_chip.1).is_none());
        app.windows.get_mut(&wid).unwrap().mods = ModifiersState::CONTROL;
        app.on_cursor_moved(wid, off_chip.0, off_chip.1);
        app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
        app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
        assert!(
            app.autopilot_menu.is_none(),
            "a non-chip does not open it: mixed={mixed}"
        );
        assert_eq!(pair(&app, session), (None, None));

        app.on_cursor_moved(wid, x, y);
        app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
        app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
        let menu = app
            .autopilot_menu
            .as_ref()
            .expect("a Ctrl-click opens the chip's menu");
        assert_eq!(
            (menu.session, menu.anchor, menu.highlight),
            (session, chip, None)
        );
        assert_eq!(
            pair(&app, session),
            (None, None),
            "secondary activation never cycles"
        );
    }
}

/// An elapsed duration is valid even when its timezone face cannot be read.
#[test]
fn an_armed_timer_can_be_cancelled_without_a_local_time_face() {
    use crate::autopilot_timers::{TimerChoice, When, resolve};
    let (mut app, wid, session, host) = claude_app();
    let _host = MenuTestHost(host);
    let mono = Instant::now();
    let wall = 1_792_177_200_000_i64;
    crate::autopilot_timers::set_fake_clocks(Some((mono, wall)));
    frame(&mut app, wid);
    assert!(app.choose_autopilot_timer(
        session,
        TimerChoice::In(60),
        crate::autopilot::Surface::Menu
    ));
    app.open_front_autopilot_menu().unwrap();
    assert!(row_of(&app, "autopilot-menu/land-cancel").is_some());
    app.close_autopilot_menu();
    assert!(app.choose_autopilot_timer(
        session,
        TimerChoice::Cancel,
        crate::autopilot::Surface::Menu
    ));
    let resolved = resolve(When::In(120), wall, &|_| None).expect("elapsed duration needs no zone");
    assert_eq!(resolved.offset_s, None);
    assert!(app.set_session_autopilot_timed(
        session,
        Some(Autopilot::Cruise),
        Some(resolved.at_ms),
        crate::autopilot::Surface::Menu
    ));
    app.autopilot_timers
        .arm(session, resolved.at_ms, mono, wall, &|_| None, None);
    assert_eq!(
        pair(&app, session),
        (Some("cruise".into()), Some(resolved.at_ms))
    );
    assert_eq!(app.autopilot_timers.glass(session), None);
    app.open_front_autopilot_menu().unwrap();
    let cancel = row_of(&app, "autopilot-menu/land-cancel")
        .expect("armed without a face still offers Cancel");
    assert_eq!(
        app.autopilot_menu.as_ref().unwrap().cancels,
        Some(resolved.at_ms)
    );
    wire(&app, session, "set autopilot cruise until=+4h");
    app.choose_autopilot_menu_row(cancel);
    assert_eq!(
        pair(&app, session),
        (Some("cruise".into()), Some(wall + 4 * 3_600_000))
    );
    app.autopilot_timers
        .arm(session, wall + 4 * 3_600_000, mono, wall, &|_| None, None);
    app.open_front_autopilot_menu().unwrap();
    let cancel = row_of(&app, "autopilot-menu/land-cancel").expect("the current arm is offered");
    app.choose_autopilot_menu_row(cancel);
    assert_eq!(pair(&app, session), (Some("cruise".into()), None));
    app.open_front_autopilot_menu().unwrap();
    assert!(
        row_of(&app, "autopilot-menu/land-cancel").is_none(),
        "no timer means no Cancel"
    );
}

/// A mode offered under an earlier policy is not permission to write after the
/// master switch is turned off.
#[test]
fn a_menu_opened_before_the_master_switch_went_off_cannot_write_a_mode() {
    let (mut app, wid, session, host) = claude_app();
    let _host = MenuTestHost(host);
    frame(&mut app, wid);
    app.open_front_autopilot_menu().unwrap();
    let land = row_of(&app, "autopilot-menu/mode/land").unwrap();
    app.choose_autopilot_menu_row(land);
    assert_eq!(pair(&app, session), (Some("land".into()), None));
    assert!(app.set_session_autopilot(session, None, crate::autopilot::Surface::Menu));
    frame(&mut app, wid);
    app.open_front_autopilot_menu().unwrap();
    let land = row_of(&app, "autopilot-menu/mode/land").unwrap();
    let mut stamps = crate::harness_policy_cell::HarnessStampSource::new();
    app.adopt_harness_policy(stamps.stamp(crate::app_config::HarnessPolicy::read(
        "[harness]\nenabled = false\n",
    )));
    assert_eq!(
        app.harness_posture(),
        crate::autopilot::HarnessPosture::MasterOff
    );
    assert_eq!(
        pair(&app, session),
        (None, None),
        "adoption changes no session word"
    );
    app.choose_autopilot_menu_row(land);
    assert_eq!(
        pair(&app, session),
        (None, None),
        "a stale enabled row cannot write under master Off"
    );
}

/// A cancel's reader identity follows its exact stored instant, even when
/// the local face cannot be read or two instants share one HH:MM face.
#[cfg(a11y_tree)]
#[test]
fn a_readers_cancel_identity_tracks_its_exact_timer_when_the_face_stays() {
    const FIRST: i64 = 1_792_177_200_001;
    const SECOND: i64 = FIRST + 1;
    let face = crate::autopilot_timers::glass_of(FIRST, FIRST - 60_000, &|_| Some(0))
        .expect("the supplied UTC offset has a face");
    assert_eq!(
        crate::autopilot_timers::glass_of(SECOND, FIRST - 60_000, &|_| Some(0)),
        Some(face),
        "distinct instants in the same minute have the same face"
    );
    let mut changed = Vec::new();
    for shown in [None, Some(face)] {
        let mut f = facts(AutopilotView::Cruise);
        f.armed_at_ms = Some(FIRST);
        f.armed = shown;
        let rect = MenuRect {
            row: 0,
            col: 0,
            w: 40,
            h: 20,
        };
        let mut menu = AutopilotMenu {
            owner: MenuOwner::Autopilot,
            wid: WindowId(0),
            session: 7,
            focus: Some(7),
            anchor: (20, 0),
            rows: rows(&f),
            spoken: f.spoken.clone(),
            cancels: f.armed_at_ms,
            highlight: Some(1),
            rect: Some(rect),
            laid: None,
            generation: 0,
            rest: PointerRest::at_open(None),
        };
        let heard = menu.a11y_epoch();
        menu.highlight = Some(2);
        assert_eq!(
            menu.a11y_epoch(),
            heard,
            "highlight keeps the item identities"
        );
        assert!(!menu.refresh_rows(rows(&f), f.spoken.clone(), f.armed_at_ms));
        assert_eq!(
            menu.a11y_epoch(),
            heard,
            "the same timer keeps its identity"
        );
        assert_eq!(menu.rect, Some(rect), "equal rows remain painted");

        let mut ordinary = menu.clone();
        let mut land = f.clone();
        land.view = AutopilotView::Land;
        let mut changed_rows = rows(&land);
        changed_rows.push(MenuRow::Note("a row the card did not have".to_string()));
        assert!(ordinary.refresh_rows(changed_rows, land.spoken, land.armed_at_ms));
        assert_eq!(ordinary.rect, None);
        assert_ne!(ordinary.a11y_epoch(), heard, "ordinary row-change control");

        f.armed_at_ms = Some(SECOND);
        let fresh = rows(&f);
        assert_eq!(fresh, menu.rows, "neither the labels nor choices moved");
        assert!(!menu.refresh_rows(fresh, f.spoken.clone(), f.armed_at_ms));
        assert_eq!(menu.cancels, Some(SECOND), "the replacement is current");
        assert_eq!(menu.rect, Some(rect), "the equal rows remain painted");
        changed.push((shown.is_some(), menu.a11y_epoch() != heard));
    }
    assert_eq!(
        changed,
        [(false, true), (true, true)],
        "reader cancel identity must change with the exact timer"
    );
}

/// THE MENU'S HEADER NAMES ITS PANE (hunt surfaces-F1, design §10.3.1):
/// two Claude Code panes of one tab open menus whose headers read apart —
/// `Autopilot · tab 1, left pane` and `…, right pane` — in the words the
/// SESSIONS rows use, read headless from the session's own place.
#[test]
fn the_menus_header_names_its_pane_where_two_agents_share_a_tab() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let right = app.split_active_stub_tab(wid);
    let claude = |app: &App, session: u64| {
        let ctx = app.pool.get(session).expect("session").ctx.clone();
        let mut tl = ctx.timeline.lock().unwrap();
        tl.note_foreground_group(7);
        tl.set_program(7, Some("claude".into()));
    };
    claude(&app, 0);
    let alone = app.autopilot_menu_facts(0);
    assert!(!alone.title.contains("pane"), "{}", alone.title);
    claude(&app, right);
    let (left, right) = (app.autopilot_menu_facts(0), app.autopilot_menu_facts(right));
    assert!(
        left.title.starts_with("Autopilot \u{b7} tab 1, left pane"),
        "{}",
        left.title
    );
    assert!(
        right
            .title
            .starts_with("Autopilot \u{b7} tab 1, right pane"),
        "{}",
        right.title
    );
    assert!(left.spoken.contains("left pane"), "{}", left.spoken);
    assert_ne!(left.title, right.title);
}

/// A Claude Code | Claude Code split of one tab (side by side, or one above
/// the other: `dir`) in a window `rows` × `cols`, both panes drawn with their
/// composer and their footer's facts and named by the agent's own title
/// (`✳ Claude Code`), the second pane focused: the app, the window, the
/// first and second sessions and the host.
fn claude_split(
    (rows, cols): (u16, u16),
    dir: crate::pane::SplitDir,
) -> (App, WindowId, u64, u64, crate::harness_host::HostHandle) {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.apply_term_resize(wid, rows, cols);
    assert_eq!(
        app.windows.get(&wid).map(|ws| (ws.rows, ws.cols)),
        Some((rows, cols))
    );
    let first = app.focused_session_id(wid).expect("the front session");
    let second = app.split_active_stub_tab_dir(wid, dir);
    let host = crate::harness_host::HostHandle::for_supervised_test(
        Vec::new(),
        std::sync::Arc::new(|_| crate::harness_host::BodyEnd::Stopped),
    );
    app.harness = Some(host.clone());
    for session in [first, second] {
        {
            let entry = app.pool.get(session).expect("session");
            let mut timeline = entry.ctx.timeline.lock().unwrap();
            timeline.note_foreground_group(4242);
            timeline.set_program(4242, Some("claude".into()));
            assert!(timeline.set_claude_footer(
                4242,
                None,
                Some(FooterFacts {
                    model: Some("Opus 5.5".into()),
                    ..FooterFacts::default()
                }),
            ));
        }
        let (_, _, rows, width) = app
            .visible_session_rect(wid, session)
            .expect("both panes are shown");
        let (rows, width) = (usize::from(rows), usize::from(width));
        let rule = "\u{2500}".repeat(width);
        let screen = format!(
            "\x1b]2;\u{2733} Claude Code\x07\x1b[?2004h\x1b[2J\x1b[{};1H{rule}\x1b[{};1H\u{276F} \
             \x1b[{};1H{rule}\x1b[{};3H",
            rows - 3,
            rows - 2,
            rows - 1,
            rows - 2,
        );
        feed(&app, session, screen.as_bytes());
    }
    assert_eq!(app.focused_session_id(wid), Some(second));
    (app, wid, first, second, host)
}

/// THE MENU KEEPS ITS SESSION'S NAME WHOLE AND STAYS IN ITS PANE (behaviour-3 and owner-eye-5):
/// in a
/// Claude Code | Claude Code split, side by side or one above the other,
/// each pane's menu names its pane — `tab 1, right pane` whole, broken at
/// its comma where the pane cannot hold it on one row — and `“✳ Claude
/// Code”` whole, on the row under the place where the card cannot hold both
/// on one.
#[test]
fn the_menu_keeps_its_name_whole_and_stays_in_its_pane() {
    use crate::pane::SplitDir;
    let cells = |app: &App, wid: WindowId, row: usize, span: std::ops::Range<usize>| -> String {
        row_text(app, wid, row)
            .chars()
            .skip(span.start)
            .take(span.len())
            .collect()
    };
    for (size, dir) in [
        ((30, 48), SplitDir::Vertical),
        ((30, 52), SplitDir::Vertical),
        ((30, 60), SplitDir::Vertical),
        ((30, 64), SplitDir::Vertical),
        ((30, 80), SplitDir::Vertical),
        ((30, 144), SplitDir::Vertical),
        ((30, 200), SplitDir::Vertical),
        ((30, 100), SplitDir::Horizontal),
        ((40, 100), SplitDir::Horizontal),
        ((24, 80), SplitDir::Horizontal),
    ] {
        let (mut app, wid, first, second, host) = claude_split(size, dir);
        frame(&mut app, wid);
        let sides = match dir {
            SplitDir::Vertical => ["left", "right"],
            SplitDir::Horizontal => ["top", "bottom"],
        };
        for (session, other, side) in [(second, first, sides[1]), (first, second, sides[0])] {
            let context = format!("{size:?} {dir:?}, the {side} pane");
            if session == first {
                assert!(app.raise_session_pane(first));
                assert_eq!(app.focused_session_id(wid), Some(first));
                frame(&mut app, wid);
            }
            let chrome = usize::from(app.chrome_rows(wid));
            let pane_of = |app: &App, session: u64| {
                let (row, col, height, width) = app.visible_session_rect(wid, session).unwrap();
                let (row, col) = (chrome + usize::from(row), usize::from(col));
                (
                    row..row + usize::from(height),
                    col..col + usize::from(width),
                )
            };
            let (rows, pane) = pane_of(&app, session);
            let (other_rows, other_cols) = pane_of(&app, other);
            let the_other = |app: &App| -> Vec<String> {
                other_rows
                    .clone()
                    .map(|row| cells(app, wid, row, other_cols.clone()))
                    .collect()
            };
            let before = the_other(&app);
            app.open_front_autopilot_menu()
                .unwrap_or_else(|why| panic!("{context}: {why}"));
            frame(&mut app, wid);
            let rect = rect_of(&app).unwrap_or_else(|| panic!("{context}: painted"));
            assert!(
                rect.col >= pane.start && rect.col + rect.w <= pane.end,
                "{context}: the card {rect:?} inside its pane's columns {pane:?}"
            );
            assert!(
                rect.row >= rows.start && rect.row + rect.h <= rows.end,
                "{context}: the card {rect:?} inside its pane's rows {rows:?}"
            );
            assert_eq!(
                the_other(&app),
                before,
                "{context}: the other pane is as it was"
            );
            let menu = app.autopilot_menu.as_ref().unwrap();
            let shown: Vec<String> = (rect.row + 1..rect.row + rect.h - 1)
                .map(|row| cells(&app, wid, row, rect.col..rect.col + rect.w))
                .collect();
            assert!(
                shown
                    .iter()
                    .any(|row| row.contains(&format!("{side} pane"))),
                "{context}: the pane word whole: {shown:?}"
            );
            assert!(
                shown
                    .iter()
                    .any(|row| row.contains("Autopilot \u{b7} tab 1")),
                "{context}: the place: {shown:?}"
            );
            assert!(
                shown
                    .iter()
                    .any(|row| row.contains("\u{201c}\u{2733} Claude Code\u{201d}")),
                "{context}: the name whole: {shown:?}"
            );
            for mode in ["Cruise", "Land now", "Off"] {
                assert!(
                    shown.iter().any(|row| row.contains(mode)),
                    "{context}: {mode} shown: {shown:?}"
                );
            }
            for (key, span) in [
                ("autopilot-menu/land-in/60", "Cruise 1 hour"),
                ("autopilot-menu/land-in/240", "Cruise 4 hours"),
            ] {
                let i = row_of(&app, key).expect("the timed Cruise");
                if let Some(text) = shown.get(i) {
                    assert!(
                        text.contains(span),
                        "{context}: {text:?} says {span} in a pane of {}",
                        pane.len()
                    );
                }
            }
            let want = menu.rows.len() + 2;
            if rows.len() >= want {
                assert_eq!(rect.h, want, "{context}: whole");
                assert!(
                    shown.iter().any(|row| row.contains("Harness settings")),
                    "{context}: {shown:?}"
                );
            } else {
                assert!(rect.h < want, "{context}: a prefix {rect:?}");
            }
            let longest = "\u{221e}\u{2913} Cruise 4 hours, then land";
            let four = row_of(&app, "autopilot-menu/land-in/240").expect("the 4 h row");
            if let Some(text) = shown.get(four)
                && pane.len() >= display_cells(longest) + 2
            {
                assert!(
                    text.contains("Cruise 4 hours, then land"),
                    "{context}: {text:?} in a pane of {}",
                    pane.len()
                );
            }
            if pane.len() < menu.rows.iter().map(row_width).max().unwrap() + CHROME_W {
                assert_eq!(rect.w, pane.len(), "{context}: as narrow as its pane");
            }
            assert!(app.close_autopilot_menu());
            frame(&mut app, wid);
        }
        host.shutdown_and_join();
    }
}

/// THE MENU'S `Harness settings…` LANDS THE PAGE ON ITS SESSION'S ROW
/// (§10.3.3 item 8; the review of v3/stats-fold): chosen with a
/// click on the menu of Claude Code's tab, it opens Settings ▸ Harness
/// landed on that session — its row's Show drawn selected, and no other
/// row's.
#[test]
fn the_menus_harness_settings_lands_the_page_on_its_sessions_row() {
    use crate::app_introspect::AuxTarget;
    use crate::harness_screen::HarnessFocus;
    use crate::native_settings::SettingsRoute;
    let (mut app, wid, session, host) = claude_app();
    app.pool
        .get(session)
        .expect("session")
        .ctx
        .timeline
        .lock()
        .unwrap()
        .set_program(4242, Some("claude".into()));
    let other = app.next_session_id;
    app.push_stub_tab(wid, crate::stub_session(other));
    {
        let ctx = app.pool.get(other).expect("the other session").ctx.clone();
        let mut tl = ctx.timeline.lock().unwrap();
        tl.note_foreground_group(7);
        tl.set_program(7, Some("claude".into()));
    }
    let landed = |app: &App| {
        app.native_settings_route_view_target(SettingsRoute::Harness)
            .map(|(_, _, _, state)| state.harness_focus)
    };
    let selected = |app: &App, s: u64| {
        let show = format!("ui key=\"harness/session/{s}/show\" ");
        app.read_aux_controls(AuxTarget::Prefs)
            .iter()
            .any(|l| l.starts_with(&show) && l.contains("selected:true"))
    };
    assert!(app.open_harness_settings(wid, HarnessFocus::Top));
    assert_eq!(landed(&app), Some(HarnessFocus::Top));
    assert!(!selected(&app, session) && !selected(&app, other));
    app.switch_tab_in(wid, 0);
    assert_eq!(app.focused_session_id(wid), Some(session));
    draw_composer(&app, session);
    frame(&mut app, wid);
    let press_row = |app: &mut App, key: &str| {
        let menu = app.autopilot_menu.as_ref().unwrap();
        let rect = menu.rect.unwrap();
        let i = menu
            .rows
            .iter()
            .position(|r| matches!(r, MenuRow::Item { act, .. } if act.key() == key))
            .unwrap();
        let (x, y) = app.frame_cell_centre(wid, (rect.row + 1 + i, rect.col + 3));
        app.autopilot_menu_press(wid, true, x, y)
    };
    app.open_front_autopilot_menu().unwrap();
    frame(&mut app, wid);
    assert!(press_row(&mut app, "autopilot-menu/settings"));
    assert!(app.autopilot_menu.is_none(), "chosen: the menu closed");
    assert_eq!(landed(&app), Some(HarnessFocus::Session(session)));
    assert!(selected(&app, session), "its row's Show, selected");
    assert!(!selected(&app, other), "and no other row's");
    host.shutdown_and_join();
}

/// THE MENU'S CARD IS THE PET'S OVERLAY, NOT ITS GROUND (review of
/// owner-eye-6, lab `rv-day-menu`): the splice notes the card apart from the
/// chrome (`PetChrome::note_overlay`), so the pet's rest seat is solved
/// without it and opening the menu sends the sleeping cat nowhere; closing
/// it clears the overlay.
#[test]
fn the_open_menus_card_is_the_pets_overlay_not_its_ground() {
    let (mut app, wid, _session, host) = claude_app();
    let _host = MenuTestHost(host);
    frame(&mut app, wid);
    app.open_front_autopilot_menu().unwrap();
    frame(&mut app, wid);
    let rect = app
        .autopilot_menu
        .as_ref()
        .and_then(|m| m.rect)
        .expect("painted");
    let dims = |r: &aterm_effects::pet_world::PetRect| (r.rows, r.cols);
    let card = (rect.h as f32, rect.w as f32);
    let chrome = &app.windows[&wid].pet_chrome;
    assert_eq!(
        chrome.held_overlays().iter().map(dims).collect::<Vec<_>>(),
        [card],
        "the card is the one overlay"
    );
    assert!(
        chrome.held().iter().all(|r| dims(r) != card),
        "and no chrome: {:?}",
        chrome.held()
    );
    assert!(app.close_autopilot_menu());
    frame(&mut app, wid);
    assert!(
        app.windows[&wid].pet_chrome.held_overlays().is_empty(),
        "closed, no overlay"
    );
}

/// THE CARD IS ON TOP OF THE CARET AND THE PET (surfaces-F3): an open menu
/// is painted after the frame published its overlays, so the composer's
/// caret under it and the resident pet sitting where it opened would paint
/// over its rows.
#[test]
fn the_open_menu_hides_the_caret_and_cuts_the_pet_under_it_on_both_routes() {
    use aterm_core::render::FreeSprite;
    for mixed in [false, true] {
        let (mut app, wid, mut session, host) = claude_app();
        let _host = MenuTestHost(host);
        if mixed {
            session = beside_settings(&mut app, wid);
        } else {
            frame(&mut app, wid);
        }
        app.open_front_autopilot_menu().unwrap();
        assert_eq!(app.autopilot_menu.as_ref().unwrap().session, session);
        frame(&mut app, wid);
        let rect = app
            .autopilot_menu
            .as_ref()
            .and_then(|m| m.rect)
            .expect("painted");
        let (cw, ch) = app.win_cell_size(wid);
        let px = |cells: usize, size: usize| i32::try_from(cells * size).unwrap();
        let right = rect.col + rect.w;
        assert!(
            right + 3 < app.windows[&wid].input_scratch.cols,
            "mixed={mixed}: room beside the card"
        );
        let pet = FreeSprite {
            x: px(right - 2, cw),
            y: px(rect.row + 2, ch),
            w: u16::try_from(4 * cw).unwrap(),
            h: u16::try_from(ch).unwrap(),
            aw: u16::try_from(4 * cw).unwrap(),
            ah: u16::try_from(ch).unwrap(),
            ..FreeSprite::default()
        };
        let publish = |app: &mut App, caret: (usize, usize), seq: &mut u64| {
            let s = &mut app.windows.get_mut(&wid).unwrap().input_scratch;
            (s.cursor_row, s.cursor_col, s.cursor_visible) = (caret.0, caret.1, true);
            s.free_sprites = vec![pet];
            *seq = s.snapshot_seq;
        };
        let mut seq = 0;
        frame_with(&mut app, wid, |app| {
            publish(app, (rect.row + 2, rect.col + 3), &mut seq);
        });
        let s = &app.windows[&wid].input_scratch;
        assert!(
            !s.cursor_visible,
            "mixed={mixed}: the caret under the card is hidden"
        );
        assert_ne!(
            s.snapshot_seq, seq,
            "mixed={mixed}: the card is a host write"
        );
        assert_eq!(
            s.free_sprites
                .iter()
                .map(|q| (q.x, q.w, q.ax))
                .collect::<Vec<_>>(),
            [(
                px(right, cw),
                u16::try_from(2 * cw).unwrap(),
                u16::try_from(2 * cw).unwrap()
            )],
            "mixed={mixed}: the pet beside the card stays, the part on it goes"
        );
        frame_with(&mut app, wid, |app| {
            publish(app, (rect.row + 2, right + 1), &mut seq);
        });
        assert!(
            app.windows[&wid].input_scratch.cursor_visible,
            "mixed={mixed}: CONTROL, the caret beside the card stands"
        );
    }
}

/// The frame rows the renderer's damage diff repaints going from `prev` to
/// `now`: every row on a full repaint.
fn repainted(
    prev: &aterm_render::RenderInput,
    now: &aterm_render::RenderInput,
    cell_h: usize,
) -> Vec<usize> {
    let mut dirty = Vec::new();
    match aterm_render::compute_dirty_rows(
        prev,
        now,
        false,
        None,
        false,
        None,
        aterm_render::RowBands::new(0, cell_h),
        &mut dirty,
    ) {
        aterm_render::DirtyDecision::FullRepaint => (0..now.rows).collect(),
        aterm_render::DirtyDecision::Rows(_) => (0..dirty.len()).filter(|&r| dirty[r]).collect(),
    }
}

/// THE CARD IS PAINTED WHOLE (review of surfaces-F3): the renderer repaints
/// only the rows whose inputs moved, and reads a row the engine vouches for
/// by its revision stamp alone — so a card written over rows the program did
/// not redraw, with nothing saying so, was painted only where the program
/// happened to redraw (in strips, the lab's `main-menu-open-c.png`), and its
/// rows lingered after the close the same way.
#[test]
fn the_open_menu_is_repainted_whole_where_the_program_drew_nothing() {
    for mixed in [false, true] {
        let (mut app, wid, _, host) = claude_app();
        let _host = MenuTestHost(host);
        if mixed {
            beside_settings(&mut app, wid);
        }
        frame(&mut app, wid);
        frame(&mut app, wid);
        let ch = app.win_cell_size(wid).1;
        let scratch = |app: &App| app.windows[&wid].input_scratch.clone();
        let closed = scratch(&app);
        app.open_front_autopilot_menu().unwrap();
        frame(&mut app, wid);
        let open = scratch(&app);
        let rect = rect_of(&app).expect("painted");
        let card: Vec<usize> = (rect.row..rect.row + rect.h).collect();
        let shown = repainted(&closed, &open, ch);
        assert!(
            card.iter().all(|r| shown.contains(r)),
            "mixed={mixed}: the open repaints every card row {card:?}, not only {shown:?}"
        );
        frame(&mut app, wid);
        let held = scratch(&app);
        let still = repainted(&open, &held, ch);
        assert!(
            !card.iter().all(|r| still.contains(r)),
            "mixed={mixed}: CONTROL, a frame the card did not move in does not repaint it \
             whole: {still:?}"
        );
        assert!(app.close_autopilot_menu());
        frame(&mut app, wid);
        let gone = repainted(&held, &scratch(&app), ch);
        assert!(
            card.iter().all(|r| gone.contains(r)),
            "mixed={mixed}: the close repaints every card row {card:?}, not only {gone:?}"
        );
    }
}

/// How a press reaches the open menu in a lifecycle test, always at the
/// pixel the pointer last moved to.
#[derive(Clone, Copy, Debug)]
enum Press {
    /// The event loop's `MouseInput` alone, as Linux and Windows deliver it.
    Bare,
    /// As AppKit delivers a click: `mouseDown:` (and `mouseUp:`) queue a
    /// `CursorMoved` at the press point ahead of the button
    /// (`aterm-window`'s macOS `view.rs`, `mouse_down`) — a motion event
    /// in which the pointer did not move.
    AppKit,
    /// `ctl pointer click`, which runs what the platform delivers.
    Ctl,
}

/// One left click on window `wid` at `(x, y)` (where the pointer is), the
/// way `how` delivers it.
fn click(app: &mut App, wid: WindowId, (x, y): (f64, f64), how: Press) {
    use aterm_window::event::{ElementState, MouseButton};
    assert_eq!(app.windows[&wid].last_cursor_px, (x, y), "{how:?}: pointer");
    match how {
        Press::Bare | Press::AppKit => {
            for state in [ElementState::Pressed, ElementState::Released] {
                if matches!(how, Press::AppKit) {
                    app.on_cursor_moved(wid, x, y);
                }
                app.mouse_input_then_fold(wid, state, MouseButton::Left);
            }
        }
        Press::Ctl => {
            app.pointer_cmd(crate::app_mouse::PointerAction::Click)
                .expect("the pointer has a position");
        }
    }
}

/// The open menu's rect, once a frame painted it.
fn rect_of(app: &App) -> Option<MenuRect> {
    app.autopilot_menu.as_ref().and_then(|m| m.rect)
}

/// Window `wid`'s pixel at the centre of the open menu's row `i` (its third
/// column), and that row's frame row.
fn row_px(app: &App, wid: WindowId, i: usize) -> ((f64, f64), usize) {
    let rect = rect_of(app).expect("painted");
    let row = rect.row + 1 + i;
    (app.frame_cell_centre(wid, (row, rect.col + 3)), row)
}

/// The open menu's choice key on frame row `row`, where one stands there.
fn key_on(app: &App, row: usize) -> Option<String> {
    let rect = rect_of(app)?;
    let menu = app.autopilot_menu.as_ref()?;
    keys(&menu.rows)
        .get(row.checked_sub(rect.row + 1)?)?
        .clone()
}

/// `session`'s timer, armed elsewhere for an hour from the fake wall clock
/// (a wire `meta set`, its wake applied): the cancel row ARRIVES in an open
/// menu, which grows above the chip, so the rows above it move up one.
fn arm_elsewhere(app: &mut App, session: u64) {
    wire(app, session, "set autopilot cruise until=+1h");
    app.on_meta_changed(session);
}

/// THE ROWS NEVER MOVE UNDER A STILL POINTER TO ANOTHER CHOICE
/// (lifecycle-1), for every way a press arrives ([`Press`]).
#[test]
fn a_row_never_moves_under_a_still_pointer_to_another_choice() {
    use crate::autopilot_timers::TimerChoice;
    const LAND: &str = "autopilot-menu/mode/land";
    const OFF: &str = "autopilot-menu/mode/off";
    let menu_open = |app: &App| app.autopilot_menu.is_some();
    for how in [Press::Bare, Press::Ctl, Press::AppKit] {
        let (mut app, wid, session, host) = claude_app();
        let _host = MenuTestHost(host);
        let (mono, wall) = (Instant::now(), 1_792_177_200_000_i64);
        crate::autopilot_timers::set_fake_clocks(Some((mono, wall)));
        frame(&mut app, wid);
        let fire = |app: &mut App, session: u64| {
            crate::autopilot_timers::set_fake_clocks(Some((
                mono + std::time::Duration::from_secs(3_600),
                wall + 3_600_000,
            )));
            app.expire_autopilot_timers();
            assert_eq!(pair(app, session), (Some("land".into()), None), "landed");
        };
        for before_the_frame in [true, false] {
            crate::autopilot_timers::set_fake_clocks(Some((mono, wall)));
            assert!(app.choose_autopilot_timer(
                session,
                TimerChoice::In(60),
                crate::autopilot::Surface::Menu
            ));
            frame(&mut app, wid);
            app.open_front_autopilot_menu().unwrap();
            frame(&mut app, wid);
            let rect = rect_of(&app).expect("painted");
            let (target, want) = if before_the_frame {
                (OFF, "off")
            } else {
                (LAND, "land")
            };
            let i = row_of(&app, target).expect("the target row");
            let (at, frame_row) = row_px(&app, wid, i);
            app.on_cursor_moved(wid, at.0, at.1);
            assert_eq!(app.autopilot_menu.as_ref().unwrap().highlight, Some(i));
            fire(&mut app, session);
            assert_eq!(rect_of(&app), Some(rect), "{how:?}: the card kept its rect");
            if !before_the_frame {
                frame(&mut app, wid);
                let shown = app.autopilot_menu_controls().join("\n");
                assert!(!shown.contains("land-cancel"), "{shown}");
                assert!(shown.contains("autopilot-menu gap index="), "{shown}");
                assert_eq!(rect_of(&app), Some(rect), "{how:?}: and kept it painted");
            }
            assert_eq!(
                key_on(&app, frame_row).as_deref(),
                Some(target),
                "{how:?}: {target} kept its row"
            );
            click(&mut app, wid, at, how);
            assert!(!menu_open(&app), "{how:?}: chosen");
            assert_eq!(
                pair(&app, session),
                (Some(want.into()), None),
                "{how:?}, before the frame={before_the_frame}: the press chose {target}, as \
                 the pointer saw"
            );
            app.on_cursor_left(wid);
        }

        let (mut app, wid, session, host) = claude_app();
        let _host = MenuTestHost(host);
        crate::autopilot_timers::set_fake_clocks(Some((mono, wall)));
        frame(&mut app, wid);
        app.open_front_autopilot_menu().unwrap();
        frame(&mut app, wid);
        let rect = rect_of(&app).expect("painted");
        let (at, frame_row) = row_px(&app, wid, row_of(&app, LAND).unwrap());
        app.on_cursor_moved(wid, at.0, at.1);
        arm_elsewhere(&mut app, session);
        frame(&mut app, wid);
        let grown = rect_of(&app).expect("painted");
        assert_eq!(grown.h, rect.h + 2, "the cancel row arrived: the card grew");
        assert_eq!(
            key_on(&app, frame_row).as_deref(),
            Some(OFF),
            "Off now stands where the pointer rests"
        );
        let armed = pair(&app, session);
        assert_eq!(armed.0.as_deref(), Some("cruise"));
        click(&mut app, wid, at, how);
        assert!(
            menu_open(&app),
            "{how:?}: the press the rows moved under chose nothing"
        );
        assert_eq!(pair(&app, session), armed, "{how:?}: nothing written");
        assert_eq!(
            app.autopilot_menu.as_ref().unwrap().highlight,
            row_of(&app, OFF),
            "{how:?}: it lit the choice under the pointer"
        );
        click(&mut app, wid, at, how);
        assert!(!menu_open(&app), "{how:?}: the next press chose it");
        assert_eq!(pair(&app, session), (Some("off".into()), None));

        for over_a_menu in [false, true] {
            app.open_front_autopilot_menu().unwrap();
            frame(&mut app, wid);
            let (at, frame_row) = row_px(&app, wid, row_of(&app, LAND).unwrap());
            if over_a_menu {
                app.on_cursor_moved(wid, at.0, at.1);
            }
            assert!(app.close_autopilot_menu());
            frame(&mut app, wid);
            if !over_a_menu {
                app.on_cursor_moved(wid, at.0, at.1);
            }
            app.open_front_autopilot_menu().unwrap();
            frame(&mut app, wid);
            assert_eq!(key_on(&app, frame_row).as_deref(), Some(LAND));
            arm_elsewhere(&mut app, session);
            frame(&mut app, wid);
            assert_eq!(key_on(&app, frame_row).as_deref(), Some(OFF));
            let armed = pair(&app, session);
            click(&mut app, wid, at, how);
            assert!(
                menu_open(&app),
                "{how:?}, over a menu={over_a_menu}: a pointer still since before the open \
                 chose nothing"
            );
            assert_eq!(pair(&app, session), armed, "{how:?}: nothing written");
            click(&mut app, wid, at, how);
            assert!(!menu_open(&app), "{how:?}: the next press chose it");
            assert_eq!(pair(&app, session), (Some("off".into()), None));
            app.on_cursor_left(wid);
        }

        let (mut wide_app, wwid, wsession, whost) = claude_app();
        let _whost = MenuTestHost(whost);
        frame(&mut wide_app, wwid);
        wide_app.open_front_autopilot_menu().unwrap();
        frame(&mut wide_app, wwid);
        let rect = rect_of(&wide_app).expect("painted");
        let beside_cell = (
            rect.row + 1 + row_of(&wide_app, LAND).unwrap(),
            rect.col + rect.w + 1,
        );
        let beside = wide_app.frame_cell_centre(wwid, beside_cell);
        wide_app.on_cursor_moved(wwid, beside.0, beside.1);
        wide_app
            .pool
            .get(wsession)
            .unwrap()
            .ctx
            .meta
            .lock()
            .unwrap()
            .user_title = Some("a far longer name for this tab, past the card's edge".into());
        wide_app.on_meta_changed(wsession);
        frame(&mut wide_app, wwid);
        let wide = rect_of(&wide_app).expect("painted");
        assert!(
            wide.contains(beside_cell.0, beside_cell.1),
            "the card widened under the pointer: {rect:?} -> {wide:?}"
        );
        assert_eq!(key_on(&wide_app, beside_cell.0).as_deref(), Some(LAND));
        let before = pair(&wide_app, wsession);
        click(&mut wide_app, wwid, beside, how);
        assert!(
            wide_app.autopilot_menu.is_some(),
            "{how:?}: a press beside the card it grew under chose nothing"
        );
        assert_eq!(
            pair(&wide_app, wsession),
            before,
            "{how:?}: nothing written"
        );
        click(&mut wide_app, wwid, beside, how);
        assert!(
            wide_app.autopilot_menu.is_none(),
            "{how:?}: the next press chose it"
        );
        assert_eq!(pair(&wide_app, wsession), (Some("land".into()), None));
        drop(wide_app);

        app.open_front_autopilot_menu().unwrap();
        frame(&mut app, wid);
        let (at, _) = row_px(&app, wid, row_of(&app, LAND).unwrap());
        app.on_cursor_moved(wid, at.0, at.1);
        arm_elsewhere(&mut app, session);
        frame(&mut app, wid);
        let (moved, _) = row_px(&app, wid, row_of(&app, LAND).unwrap());
        assert_ne!(moved, at);
        app.on_cursor_moved(wid, moved.0, moved.1);
        click(&mut app, wid, moved, how);
        assert!(!menu_open(&app), "{how:?}: a moved pointer presses at once");
        assert_eq!(pair(&app, session), (Some("land".into()), None));
        crate::autopilot_timers::set_fake_clocks(None);
    }
}

/// A CARD PAINTED UNDER A RESTING POINTER TAKES NO PRESS IT DID NOT SEE (the
/// review of v3/polish-page's fixes), for every way a press
/// arrives ([`Press`]).
#[test]
fn a_card_painted_under_a_resting_pointer_takes_no_press_it_did_not_see() {
    use crate::pane::SplitDir;
    use aterm_window::event::{ElementState, MouseButton};
    for how in [Press::Bare, Press::Ctl, Press::AppKit] {
        let (mut app, wid, _first, session, host) = claude_split((24, 80), SplitDir::Horizontal);
        let _host = MenuTestHost(host);
        frame(&mut app, wid);
        let chip = chip_cell(&app, wid, session);
        let at = app.frame_cell_centre(wid, chip);
        let open_by_hand = |app: &mut App| {
            app.on_cursor_moved(wid, at.0, at.1);
            app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Right);
            app.on_mouse_input(wid, ElementState::Released, MouseButton::Right);
            assert!(
                app.autopilot_menu.is_some(),
                "{how:?}: the right press opened it"
            );
            frame(app, wid);
        };
        let before = pair(&app, session);
        open_by_hand(&mut app);
        let rect = rect_of(&app).expect("painted");
        assert!(
            rect.contains(chip.0, chip.1),
            "{how:?}: the card {rect:?} covers its chip {chip:?}"
        );
        let under = key_on(&app, chip.0).expect("a choice under the pointer");
        click(&mut app, wid, at, how);
        assert!(
            app.autopilot_menu.is_some(),
            "{how:?}: a press on {under}, which came up under a still pointer, chose nothing"
        );
        assert_eq!(pair(&app, session), before, "{how:?}: nothing written");
        assert_eq!(
            app.autopilot_menu.as_ref().unwrap().highlight,
            row_of(&app, &under),
            "{how:?}: it lit {under}"
        );
        click(&mut app, wid, at, how);
        assert!(
            app.autopilot_menu.is_none(),
            "{how:?}: the next press chose it"
        );
        assert_ne!(pair(&app, session), before, "{how:?}: {under} written");
        app.set_session_autopilot(session, None, crate::autopilot::Surface::Menu);
        let before = pair(&app, session);
        frame(&mut app, wid);
        app.open_front_autopilot_menu().unwrap();
        frame(&mut app, wid);
        assert!(rect_of(&app).is_some_and(|r| r.contains(chip.0, chip.1)));
        click(&mut app, wid, at, how);
        assert!(
            app.autopilot_menu.is_some(),
            "{how:?}: keyboard open, nothing chosen"
        );
        assert_eq!(
            pair(&app, session),
            before,
            "{how:?}: keyboard open, nothing written"
        );
        assert!(app.close_autopilot_menu());
        frame(&mut app, wid);
        open_by_hand(&mut app);
        let moved = (at.0 + 1.0, at.1);
        app.on_cursor_moved(wid, moved.0, moved.1);
        assert_eq!(key_on(&app, chip.0), Some(under.clone()));
        click(&mut app, wid, moved, how);
        assert!(
            app.autopilot_menu.is_none(),
            "{how:?}: a moved pointer chooses at once"
        );
        assert_ne!(pair(&app, session), before, "{how:?}: {under} written");
        app.on_cursor_left(wid);
    }
}

/// THE MENU CUTS THE LIGHT (`docs/DESIGN-card-engine-2026-10-19.md` §7
/// `fx_occluders_compose`): the open Autopilot menu, then the paste question
/// over the top rows, each push an engine occluder, so light in every fx slot
/// over the whole frame leaves both byte-identical to the frame without it.
#[test]
fn fx_occluders_compose_over_the_autopilot_menu_and_the_paste_banner() {
    use crate::app_render::fx_occluder_tests::{
        Shape, assert_chrome_cuts_the_light, chrome_written, trace_splice,
    };
    let (mut app, wid, session, _host) = claude_app();
    frame(&mut app, wid);
    app.open_front_autopilot_menu()
        .expect("the chip is on the glass");
    let mut before = None;
    frame_with(&mut app, wid, |app| {
        before = Some(app.windows[&wid].input_scratch.clone());
    });
    assert!(
        app.autopilot_menu.as_ref().and_then(|m| m.rect).is_some(),
        "the menu painted"
    );
    let menu = chrome_written(
        &mut app,
        wid,
        "Autopilot menu",
        Shape::Card,
        &before.expect("the frame reached the menu's splice"),
    );
    app.paste_banner = Some(crate::paste_banner::PendingPaste::new(
        wid,
        Some(session),
        "one\ntwo\nthree".to_owned(),
        crate::input::Source::Human,
        crate::input::PasteFraming::AtDrain,
    ));
    let banner = trace_splice(&mut app, wid, "paste banner", Shape::Band, |app| {
        app.splice_paste_banner(wid);
    });
    assert_chrome_cuts_the_light(&mut app, wid, &[menu, banner]);
}

/// THREE SURFACES IN ONE FRAME (`docs/DESIGN-card-engine-2026-10-19.md` §7
/// `fx_occluders_compose`): the open Autopilot menu, the paste question over
/// the top rows and the caption of a hovered link on the bottom row, each
/// spliced over the same frame, each push an engine occluder, and light in
/// every fx slot leaves all three byte-identical to the frame without it.
#[test]
fn fx_occluders_compose_over_the_autopilot_menu_the_question_and_the_caption() {
    use crate::app_render::fx_occluder_tests::{
        Shape, assert_chrome_cuts_the_light, chrome_written, trace_splice,
    };
    let (mut app, wid, session, _host) = claude_app();
    feed(
        &app,
        session,
        b"\x1b[7;1HVisit \x1b]8;;https://example.com/a\x1b\\example.com\x1b]8;;\x1b\\ now\x1b[22;3H",
    );
    frame(&mut app, wid);
    let (x, y) = app.frame_cell_centre(wid, (6, 8));
    app.on_cursor_moved(wid, x, y);
    app.open_front_autopilot_menu()
        .expect("the chip is on the glass");
    let mut before = None;
    frame_with(&mut app, wid, |app| {
        before = Some(app.windows[&wid].input_scratch.clone());
    });
    assert!(
        app.autopilot_menu.as_ref().and_then(|m| m.rect).is_some(),
        "the menu painted"
    );
    let menu = chrome_written(
        &mut app,
        wid,
        "Autopilot menu",
        Shape::Card,
        &before.expect("the frame reached the menu's splice"),
    );
    app.paste_banner = Some(crate::paste_banner::PendingPaste::new(
        wid,
        Some(session),
        "one\ntwo\nthree".to_owned(),
        crate::input::Source::Human,
        crate::input::PasteFraming::AtDrain,
    ));
    let banner = trace_splice(&mut app, wid, "paste banner", Shape::Band, |app| {
        app.splice_paste_banner(wid);
    });
    let caption = trace_splice(&mut app, wid, "link caption", Shape::Band, |app| {
        app.splice_link_target(wid);
    });
    assert_chrome_cuts_the_light(&mut app, wid, &[menu, banner, caption]);
    app.on_cursor_left(wid);
}

/// A font zoom to `size` px: the zoom's own transaction, its renderer
/// rebuild elided (a headless window has none, nor an OS window to re-fit),
/// so the window's cells grow and its grid keeps its rows and columns — and
/// Claude's screen, and the chip at the open menu's anchor.
fn zoom(app: &mut App, size: f32) {
    assert!(app.set_font_size_with(size, |_| true), "zoomed to {size}");
}

/// The word choosing row `i` of `rows` writes, where it is an enabled mode
/// choice (`Cruise`, `Land now`, `Off`).
fn mode_word(rows: &[MenuRow], i: usize) -> Option<&'static str> {
    match rows.get(i)? {
        MenuRow::Item {
            act: MenuAct::Mode(mode),
            enabled: true,
            ..
        } => Some(mode.word()),
        _ => None,
    }
}

/// The menu open from the chip as a pointer opens it (no row lit) and
/// painted, and the first zoom step from the window's font that puts, under
/// the pixel of one mode choice of the painted card — as the grid it was
/// laid on shows it — another mode choice's cell of that card: the zoom's
/// size, the choice the pixel shows, the choice the zoomed grid puts under
/// it, and the pixel.
fn open_where_a_zoom_moves_a_choice(
    app: &mut App,
    wid: WindowId,
    session: u64,
) -> (f32, usize, usize, (f64, f64)) {
    frame(app, wid);
    let chip = chip_cell(app, wid, session);
    app.open_autopilot_menu(wid, session, chip, false);
    frame(app, wid);
    let rect = rect_of(app).expect("painted");
    let rows = app.autopilot_menu.as_ref().expect("open").rows.clone();
    let cells: Vec<(usize, (f64, f64))> = (0..rows.len())
        .filter(|&i| mode_word(&rows, i).is_some())
        .map(|i| (i, row_px(app, wid, i).0))
        .collect();
    let base = app.font_size;
    let found = (1..=16u8)
        .find_map(|step| {
            let size = base + f32::from(step);
            zoom(app, size);
            let found = cells.iter().find_map(|&(i, (x, y))| {
                let (row, col) = app.frame_cell_at(wid, x, y)?;
                let j =
                    item_at(rect, &rows, row, col).filter(|&j| mode_word(&rows, j).is_some())?;
                (j != i).then_some((size, i, j, (x, y)))
            });
            zoom(app, base);
            found
        })
        .expect("a zoom puts another mode choice under one's pixel");
    assert_eq!(
        app.autopilot_menu.as_ref().and_then(|m| m.laid),
        app.glass_grid(wid),
        "the zoom undone: the grid the card was laid on"
    );
    found
}

/// The open menu's lit row.
fn lit(app: &App) -> Option<usize> {
    app.autopilot_menu.as_ref().and_then(|m| m.highlight)
}

/// A RE-GRID LEAVES THE AUTOPILOT MENU'S LAST RECT ANSWERING NOTHING: the
/// menu open from its chip and painted, the pointer resting on a mode
/// choice.
#[test]
fn a_regrid_leaves_the_autopilot_menus_last_rect_answering_nothing() {
    for forged in [true, false] {
        let (mut app, wid, session, host) = claude_app();
        let _host = MenuTestHost(host);
        let (size, was, now, (x, y)) = open_where_a_zoom_moves_a_choice(&mut app, wid, session);
        let rows = app.autopilot_menu.as_ref().expect("open").rows.clone();
        app.on_cursor_moved(wid, x, y);
        assert_eq!(
            lit(&app),
            Some(was),
            "on the grid it was laid on, its choice"
        );
        let before = pair(&app, session);
        zoom(&mut app, size);
        let laid = app.autopilot_menu.as_ref().and_then(|m| m.laid);
        assert_ne!(app.glass_grid(wid), laid, "the zoom re-grids the window");
        if forged {
            app.autopilot_menu.as_mut().expect("open").laid = app.glass_grid(wid);
        }
        app.on_cursor_moved(wid, x, y);
        if forged {
            assert_eq!(
                lit(&app),
                Some(now),
                "CONTROL: unstamped, another choice lit"
            );
        } else {
            assert_eq!(
                lit(&app),
                Some(was),
                "nothing lit from a rect laid on another grid"
            );
        }
        click(&mut app, wid, (x, y), Press::Bare);
        assert!(
            app.autopilot_menu.is_none(),
            "forged={forged}: the press closes it"
        );
        if forged {
            assert_eq!(
                pair(&app, session).0.as_deref(),
                mode_word(&rows, now),
                "CONTROL: the choice the pointer never rested on is chosen"
            );
            assert_ne!(pair(&app, session), before, "CONTROL");
        } else {
            assert_eq!(pair(&app, session), before, "nothing is chosen");
        }
    }
}

/// THE RE-GRID'S FRAME LAYS THE CARD AGAIN, AND ITS ROWS MOVED UNDER A STILL
/// POINTER (lifecycle-1): the menu open and painted, the pointer resting on
/// a mode choice; a font zoom puts another choice under that pixel with no
/// motion.
#[test]
fn a_card_laid_again_on_a_new_grid_takes_no_press_it_did_not_see() {
    for moved in [false, true] {
        let (mut app, wid, session, host) = claude_app();
        let _host = MenuTestHost(host);
        let (size, was, now, (x, y)) = open_where_a_zoom_moves_a_choice(&mut app, wid, session);
        let rows = app.autopilot_menu.as_ref().expect("open").rows.clone();
        let rect = rect_of(&app).expect("painted");
        app.on_cursor_moved(wid, x, y);
        assert_eq!(lit(&app), Some(was));
        let before = pair(&app, session);
        zoom(&mut app, size);
        frame(&mut app, wid);
        assert_eq!(rect_of(&app), Some(rect), "laid again at the same cells");
        assert_eq!(
            app.autopilot_menu.as_ref().and_then(|m| m.laid),
            app.glass_grid(wid),
            "on the zoomed grid"
        );
        let (row, col) = app.frame_cell_at(wid, x, y).expect("on the frame");
        assert_eq!(
            item_at(rect, &rows, row, col),
            Some(now),
            "another choice under the pixel"
        );
        if moved {
            let (cx, cy) = app.frame_cell_centre(wid, (row, col));
            app.on_cursor_moved(wid, cx, cy + 1.0);
            app.on_cursor_moved(wid, x, y);
        }
        click(&mut app, wid, (x, y), Press::Bare);
        if moved {
            assert!(app.autopilot_menu.is_none(), "CONTROL: chosen at once");
            assert_eq!(
                pair(&app, session).0.as_deref(),
                mode_word(&rows, now),
                "CONTROL"
            );
            continue;
        }
        assert!(
            app.autopilot_menu.is_some(),
            "the press the rows moved under is refused"
        );
        assert_eq!(pair(&app, session), before, "nothing is chosen");
        assert_eq!(
            lit(&app),
            Some(now),
            "the choice now under the pointer is lit"
        );
        click(&mut app, wid, (x, y), Press::Bare);
        assert!(app.autopilot_menu.is_none(), "the next press takes it");
        assert_eq!(pair(&app, session).0.as_deref(), mode_word(&rows, now));
    }
}

/// A DOUBLE-CLICK'S SECOND PRESS IS JUDGED WHERE THE CARD WAS LAID: a press
/// on a mode choice chooses it and closes the card; a font zoom then
/// re-grids the window before the double-click's second press at the same
/// pixel, which the zoomed grid puts off the card's rect.
#[test]
fn a_regrid_between_a_double_clicks_presses_moves_no_pixel_of_it() {
    for on_card in [true, false] {
        let (mut app, wid, session, host) = claude_app();
        let _host = MenuTestHost(host);
        frame(&mut app, wid);
        let chip = chip_cell(&app, wid, session);
        app.open_autopilot_menu(wid, session, chip, false);
        frame(&mut app, wid);
        let rect = rect_of(&app).expect("painted");
        let rows = app.autopilot_menu.as_ref().expect("open").rows.clone();
        let i = (0..rows.len())
            .find(|&i| mode_word(&rows, i).is_some())
            .expect("a mode choice");
        let ((x, y), _) = row_px(&app, wid, i);
        let off = app.frame_cell_centre(wid, (0, 0));
        assert!(!rect.contains(0, 0), "the corner is off the card");
        app.on_cursor_moved(wid, x, y);
        click(&mut app, wid, (x, y), Press::Bare);
        assert!(app.autopilot_menu.is_none(), "chosen and closed");
        assert!(
            app.autopilot_menu_spent.is_some(),
            "the card a choice closed"
        );
        let doubled = app.font_size * 2.0;
        zoom(&mut app, doubled);
        assert!(
            app.frame_cell_at(wid, x, y)
                .is_none_or(|(row, col)| !rect.contains(row, col)),
            "the zoomed grid puts the pixel off the card's rect"
        );
        if !on_card {
            app.on_cursor_moved(wid, off.0, off.1);
        }
        assert_eq!(
            app.spends_menu_follow_up(wid),
            on_card,
            "on_card={on_card}: spent only where it fell on the card as laid"
        );
    }
}
