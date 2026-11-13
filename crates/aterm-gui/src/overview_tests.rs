// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn sid(s: &str) -> SessionId {
    SessionId::new(s)
}

fn chip(s: &str, local: u64) -> OverviewChip {
    OverviewChip {
        sid: sid(s),
        local_id: local,
        label: format!("t-{s}"),
        mark: ChipMark::Quiet,
        need: None,
        doing: "idle".to_string(),
        task: None,
        task_words: None,
        driver: None,
        held: None,
        typed: None,
        worker_of: false,
        held_by_person: false,
        has_result: false,
        stoppable: false,
    }
}

fn worker(s: &str, local: u64, manager: &str) -> OverviewChip {
    OverviewChip {
        mark: ChipMark::Working,
        task: Some("working"),
        task_words: Some("task working 2m".to_string()),
        driver: Some(Driver::WorkerOf(manager.to_string())),
        worker_of: true,
        stoppable: true,
        ..chip(s, local)
    }
}

fn geom() -> SettingsGeom {
    SettingsGeom {
        cw: 8.0,
        ch: 16.0,
        font_px: 13.0,
        cols: 160,
        panel_rows: 48,
        px_per_pt: 1.0,
    }
}

fn state() -> OverviewState {
    let mut needy = chip("a", 1);
    needy.mark = ChipMark::Stuck;
    needy.need = Some("frozen");
    let mut done = worker("c", 3, "t-a");
    done.mark = ChipMark::Done;
    done.task = Some("done");
    done.has_result = true;
    done.stoppable = false;
    OverviewState::new(
        crate::WindowId(0),
        vec![OverviewGroup {
            label: "Window 1".to_string(),
            chips: vec![needy, worker("b", 2, "t-a"), done],
        }],
        vec![Manages {
            manager: sid("a"),
            manager_local: Some(1),
            manager_label: "t-a".to_string(),
            workers: vec![(sid("b"), "t-b".to_string()), (sid("c"), "t-c".to_string())],
        }],
    )
}

#[test]
fn the_card_counts_needs_and_workers_and_lists_who_manages_whom() {
    let s = state();
    let lines = s.controls_lines();
    assert!(
        lines[0].contains("sessions=3")
            && lines[0].contains("needs=1")
            && lines[0].contains("workers=2")
            && lines[0].contains("manages=1"),
        "{lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.contains("sid=a") && l.contains("mark=stuck") && l.contains("need=frozen"))
    );
    assert!(lines.iter().any(|l| l.contains("sid=b")
        && l.contains("task_state=working")
        && l.contains("driver=worker")
        && l.contains("stoppable=1")));
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("overview manages manager=a local=1 workers=b,c"))
    );
    let tray = overview_tray(&s, &geom(), Theme::default());
    let texts = tray
        .prims
        .iter()
        .filter(|p| matches!(p, DrawPrim::Text { .. }))
        .count();
    assert!(texts > 1 + 1 + 3 + 1, "{texts}");
}

#[test]
fn a_chip_says_who_typed_after_its_driver() {
    let mut s = state();
    let typed = Some(("lab-sweep".to_string(), "4m ago".to_string()));
    s.groups[0].chips[1].typed = typed;
    let text = s.chip_text(1, &s.groups[0].chips[1]);
    assert!(
        text.ends_with("worker of t-a \u{b7} typed by lab-sweep 4m ago"),
        "{text}"
    );
    assert!(!s.chip_text(0, &s.groups[0].chips[0]).contains("typed"));
    let lines = s.controls_lines();
    assert!(
        lines
            .iter()
            .any(|l| l.contains("sid=b ") && l.contains(" typed_by=lab-sweep "))
    );
    assert!(
        lines
            .iter()
            .any(|l| l.contains("sid=a ") && l.contains(" typed_by=- "))
    );
}

#[test]
fn select_session_lands_on_the_manages_line_or_the_chip() {
    let mut s = state();
    s.move_selection(1);
    assert!(s.select_session(1));
    assert_eq!(s.selected_item(), Some(OverviewItem::Manages { line: 0 }));
    assert_eq!(s.activate(), OverviewAct::Raise(1), "its manager");
    assert!(s.select_session(2), "control: no manages line");
    assert_eq!(s.selected_chip().map(|c| c.local_id), Some(2));
    assert!(!s.select_session(9));
    assert_eq!(s.selected_chip().map(|c| c.local_id), Some(2), "unmoved");
}

#[test]
fn the_keys_resolve_to_their_acts_and_stop_is_two_steps() {
    let mut s = state();
    assert_eq!(s.activate(), OverviewAct::Raise(1));
    assert_eq!(s.take_over_pressed(), OverviewAct::TakeOverOrHandBack(1));
    assert_eq!(s.read_result_pressed(), OverviewAct::None, "no result on a");
    assert_eq!(s.stop_pressed(false), OverviewAct::None, "a is no worker");
    s.move_selection(1);
    assert_eq!(s.stop_pressed(false), OverviewAct::Armed(2));
    assert!(s.controls_lines()[0].contains("confirm=1 confirm_by=person"));
    assert_eq!(s.stop_pressed(false), OverviewAct::StopWorker(2));
    assert_eq!(s.stop_pressed(false), OverviewAct::Armed(2));
    assert_eq!(s.activate(), OverviewAct::Raise(2));
    assert!(s.controls_lines()[0].contains("confirm=-"));
    assert_eq!(s.stop_pressed(false), OverviewAct::Armed(2));
    s.move_selection(1);
    assert!(s.controls_lines()[0].contains("confirm=-"));
    assert_eq!(s.read_result_pressed(), OverviewAct::ReadResult(3));
    assert_eq!(s.stop_pressed(false), OverviewAct::None);
    s.move_selection(1);
    assert_eq!(s.activate(), OverviewAct::Raise(1));
    assert_eq!(s.take_over_pressed(), OverviewAct::None);
    s.move_selection(-2);
    assert_eq!(s.stop_pressed(false), OverviewAct::Armed(2));
    assert!(!s.escape());
    assert!(s.escape());
}

#[test]
fn a_confirm_is_resolved_only_by_the_hand_that_armed_it() {
    let mut s = state();
    s.move_selection(1);
    assert_eq!(s.stop_pressed(true), OverviewAct::Armed(2));
    assert!(s.controls_lines()[0].contains("confirm_by=controller"));
    let by_controller = s.fingerprint();
    assert_eq!(s.stop_pressed(false), OverviewAct::Armed(2), "re-armed");
    assert!(s.controls_lines()[0].contains("confirm_by=person"));
    assert_ne!(
        s.fingerprint(),
        by_controller,
        "a re-arm by the other hand repaints its words"
    );
    assert_eq!(s.stop_pressed(true), OverviewAct::Armed(2), "re-armed");
    assert_eq!(s.stop_pressed(true), OverviewAct::StopWorker(2));
    s.move_selection(-1);
    assert!(s.arm_stop_on(2, false));
    assert!(
        s.controls_lines()
            .iter()
            .any(|l| l.contains("sid=b") && l.contains("selected=true confirm=true"))
    );
    assert_eq!(s.stop_pressed(false), OverviewAct::StopWorker(2));
    assert!(!s.arm_stop_on(3, false), "c's task is done");
    assert!(!s.arm_stop_on(42, false), "no such chip");
    assert!(s.controls_lines()[0].contains("confirm=-"));
}

#[test]
fn a_held_worker_still_counts_and_t_hands_it_back() {
    let mut s = state();
    s.move_selection(1);
    assert!(!s.t_hands_back(), "b is not held");
    let mut groups = state().groups;
    groups[0].chips[1].driver = Some(Driver::You);
    groups[0].chips[1].held_by_person = true;
    groups[0].chips[1].mark = ChipMark::Yours;
    assert!(s.retarget(groups, state().manages));
    assert!(s.t_hands_back(), "b is the person's");
    assert!(
        s.controls_lines()[0].contains("workers=2"),
        "{:?}",
        s.controls_lines()[0]
    );
}

#[test]
fn retarget_keeps_selection_identity_and_drops_a_dead_confirm() {
    let mut s = state();
    s.move_selection(1);
    assert_eq!(s.stop_pressed(false), OverviewAct::Armed(2));
    let mut groups = state().groups;
    groups[0].chips.insert(0, chip("z", 9));
    assert!(s.retarget(groups.clone(), state().manages));
    assert!(
        s.controls_lines().iter().any(|l| l.contains("sid=b")
            && l.contains("selected=true")
            && l.contains("confirm=true"))
    );
    assert!(
        !s.retarget(groups.clone(), state().manages),
        "same words: no change"
    );
    groups[0].chips[2].stoppable = false;
    assert!(s.retarget(groups, state().manages));
    assert!(s.controls_lines()[0].contains("confirm=-"));
}

#[test]
fn a_held_chip_says_why_on_a_line_of_its_own_cut_with_an_ellipsis() {
    let held = "not landed yet: needs you; 3 changed files, 1 untracked file, 1 stash, 4 \
                commits not on the remote main; open in its report: follow-up, not \
                pushed, local, a part; no \u{201c}Nothing left:\u{201d} defense yet; a reply \
                too tall to read whole \u{b7} 3 nudges";
    let mut landing = chip("h", 4);
    landing.label = "docs pass".to_string();
    landing.mark = ChipMark::Working;
    landing.task = Some("working");
    landing.task_words = Some("task working 2m".to_string());
    landing.driver = Some(Driver::Aterm("land".to_string()));
    landing.typed = Some(("aterm".to_string(), "2m ago".to_string()));
    landing.held = Some(held.to_string());
    let mut s = OverviewState::new(
        crate::WindowId(0),
        vec![OverviewGroup {
            label: "Window 1".to_string(),
            chips: vec![chip("a", 1), landing, chip("z", 5)],
        }],
        Vec::new(),
    );
    let g = geom();
    let layout = overview_layout(&s, &g);
    assert_eq!(layout.card.2, MAX_CARD_WIDTH_PT, "the card at its widest");
    assert_eq!(s.lines().len(), 5);
    assert_eq!(s.wanted_rows(), CHROME_ROWS + 5);
    let texts = |s: &OverviewState| -> Vec<(f32, String)> {
        overview_tray(s, &g, Theme::default())
            .prims
            .into_iter()
            .filter_map(|p| match p {
                DrawPrim::Text { x, s, .. } => Some((x, s)),
                _ => None,
            })
            .collect()
    };
    let painted = texts(&s);
    let row = painted
        .iter()
        .find(|(_, t)| t.contains("docs pass"))
        .expect("the chip's line");
    assert!(
        row.1
            .ends_with("driven by aterm (land) \u{b7} typed by aterm 2m ago"),
        "the chip's own words whole: {}",
        row.1
    );
    let under = painted
        .iter()
        .find(|(_, t)| t.starts_with("not landed yet: "))
        .expect("the held line");
    assert!(under.0 > row.0, "past the chip's mark");
    assert!(
        under.1.ends_with('\u{2026}') && held.starts_with(under.1.trim_end_matches('\u{2026}')),
        "cut with `…`: {}",
        under.1
    );
    let px = TypeStep::Body.px(g.font_px).get();
    let narrow = SettingsGeom {
        cw: crate::settings::text_w("0", px) * 0.9,
        ..g
    };
    for g in [g, narrow] {
        let card = overview_layout(&s, &g).card;
        let ring_right = card.0 + card.2 - g.cw;
        let painted = overview_tray(&s, &g, Theme::default()).prims;
        let body: Vec<(f32, String)> = painted
            .into_iter()
            .filter_map(|p| match p {
                DrawPrim::Text { x, s, .. } if !s.contains("esc close") => Some((x, s)),
                _ => None,
            })
            .collect();
        assert!(
            body.iter()
                .any(|(_, t)| t.starts_with("not landed yet: ") && t.ends_with('\u{2026}')),
            "the held line cut: {body:?}"
        );
        for (x, t) in &body {
            let right = x + crate::settings::text_w(t, px);
            assert!(
                right <= ring_right + 0.01,
                "{t:?} ends at {right} past the ring's {ring_right} (cw {})",
                g.cw
            );
        }
    }
    let (x, y, w, h) = overview_row_rect_in(&layout, &g, 3).expect("its slot");
    assert_eq!(overview_item_hit(&s, &g, x + w * 0.5, y + h * 0.5), Some(1));
    assert!(
        s.controls_lines()
            .iter()
            .any(|l| l.contains("sid=h") && l.ends_with(" held=1")),
        "{:?}",
        s.controls_lines()
    );
    s.select(1);
    let ring = overview_tray(&s, &g, Theme::default())
        .prims
        .into_iter()
        .find_map(|p| match p {
            DrawPrim::Stroke { x, h, .. } if x == layout.card.0 + g.cw => Some(h),
            _ => None,
        })
        .expect("the ring");
    assert!(ring > g.ch * 1.5, "two rows tall: {ring}");
}

#[test]
fn hit_test_paint_and_fingerprint_share_one_state() {
    let mut s = state();
    let g = geom();
    let layout = overview_layout(&s, &g);
    let (x, y, w, h) = overview_row_rect_in(&layout, &g, 1).unwrap();
    assert_eq!(overview_item_hit(&s, &g, x + w * 0.5, y + h * 0.5), Some(0));
    let (hx, hy, hw, hh) = overview_row_rect_in(&layout, &g, 0).unwrap();
    assert_eq!(
        overview_item_hit(&s, &g, hx + hw * 0.5, hy + hh * 0.5),
        None
    );
    let tray = overview_tray(&s, &g, Theme::default());
    assert_eq!(tray.card, layout.card);
    let before = s.fingerprint();
    assert_ne!(before, 0);
    let mut groups = state().groups;
    groups[0].chips[1].doing = "busy 4m".to_string();
    assert!(s.retarget(groups, state().manages));
    assert_ne!(s.fingerprint(), before);
}

/// The same window at backing scale `k`: its cells, type and width cap scaled together.
fn geom_at(k: f32) -> SettingsGeom {
    SettingsGeom {
        cw: 8.0 * k,
        ch: 16.0 * k,
        font_px: 13.0 * k,
        cols: 144,
        panel_rows: 40,
        px_per_pt: k,
    }
}

/// Every Text prim of a tray as `(x, baseline, string)`.
fn painted_texts(tray: &TrayInput) -> Vec<(f32, f32, String)> {
    tray.prims
        .iter()
        .filter_map(|p| match p {
            DrawPrim::Text { x, baseline, s, .. } => Some((*x, *baseline, s.clone())),
            _ => None,
        })
        .collect()
}

/// A window too narrow for the cap at backing scale `k`, whose cell (7 points) makes two cells
/// less than the margin's 16-point floor, so the floor decides the card's width.
fn narrow_at(k: f32) -> SettingsGeom {
    SettingsGeom {
        cw: 7.0 * k,
        cols: 60,
        ..geom_at(k)
    }
}

/// At 2 device pixels per point the Overview is the 1x card at twice the pixels: its corner
/// radius cap, shadow offset, border, row insets and hover ring are points. The cell
/// (12 x 25) is tall enough that the 14-point radius cap binds (§14.16).
#[test]
fn the_overview_wears_its_corners_shadow_and_ring_in_points() {
    let at = |k: f32| SettingsGeom {
        cw: 12.0 * k,
        ch: 25.0 * k,
        font_px: 18.0 * k,
        cols: 144,
        panel_rows: 40,
        px_per_pt: k,
    };
    let s = closing_scene();
    let one = overview_tray(&s, &at(1.0), Theme::default()).prims;
    let two = overview_tray(&s, &at(2.0), Theme::default()).prims;
    let DrawPrim::Stroke { radius, .. } = one[2] else {
        panic!("the card's border: {:?}", one[2]);
    };
    assert_eq!(radius, 14.0, "the 1x radius is at its cap");
    assert!(
        one.iter()
            .any(|p| matches!(p, DrawPrim::Stroke { width, .. } if *width == 1.5)),
        "the scene rings its selected row"
    );
    crate::palette::assert_drawn_at_scale(&one, &two, 2.0, &[], "overview");
}

#[test]
fn the_card_is_one_width_in_points_at_1x_and_2x_and_says_the_same() {
    let s = closing_scene();
    for at in [geom_at, narrow_at] {
        let (g1, g2) = (at(1.0), at(2.0));
        let (c1, c2) = (overview_layout(&s, &g1).card, overview_layout(&s, &g2).card);
        for (one, two) in [(c1.0, c2.0), (c1.1, c2.1), (c1.2, c2.2), (c1.3, c2.3)] {
            assert!(
                (two / g2.px_per_pt - one).abs() < 0.01,
                "the 2x card {c2:?} is the 1x card {c1:?} in points"
            );
        }
        let (t1, t2) = (
            overview_tray(&s, &g1, Theme::default()),
            overview_tray(&s, &g2, Theme::default()),
        );
        let words = |t: &TrayInput| -> Vec<String> {
            painted_texts(t).into_iter().map(|(_, _, s)| s).collect()
        };
        assert_eq!(
            words(&t2),
            words(&t1),
            "the 2x card cuts its rows where the 1x card does"
        );
        // within a point: a centred run moves by half of what its measure's per-glyph rounding adds
        let x_at =
            |t: &TrayInput| -> Vec<f32> { painted_texts(t).into_iter().map(|(x, ..)| x).collect() };
        for (one, two) in x_at(&t1).into_iter().zip(x_at(&t2)) {
            assert!(
                (two / g2.px_per_pt - one).abs() < 1.0,
                "each run starts where the 1x card starts it: {one} vs {two}"
            );
        }
    }
    let card_w = |g: SettingsGeom| overview_layout(&s, &g).card.2;
    assert_eq!(
        card_w(geom_at(1.0)),
        MAX_CARD_WIDTH_PT,
        "the wide card at its widest"
    );
    let narrow = narrow_at(1.0);
    assert_eq!(
        card_w(narrow),
        narrow.cols as f32 * narrow.cw - 2.0 * 16.0,
        "the narrow card: the tray less a 16-point margin on each side"
    );
}

#[test]
fn the_key_hints_fit_whole_on_a_2x_card() {
    let s = closing_scene();
    let g = geom_at(2.0);
    let layout = overview_layout(&s, &g);
    let (card_x, card_y, card_w, _) = layout.card;
    let last_row = card_y + layout.card_rows.saturating_sub(1) as f32 * g.ch;
    let hints: Vec<(f32, String)> = painted_texts(&overview_tray(&s, &g, Theme::default()))
        .into_iter()
        .filter(|(_, baseline, _)| *baseline > last_row)
        .map(|(x, _, s)| (x, s))
        .collect();
    assert_eq!(
        hints.len(),
        1,
        "one run of key hints on the card's last row: {hints:?}"
    );
    let (x, hint) = &hints[0];
    let right = x + crate::settings::text_w(hint, TypeStep::Caption.px(g.font_px).get());
    assert!(
        *x >= card_x + g.cw - 0.01 && right <= card_x + card_w - g.cw + 0.01,
        "{hint:?} spans {x}..{right}, past the card's {}..{}",
        card_x + g.cw,
        card_x + card_w - g.cw
    );
}
