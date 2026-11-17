// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::native_appearance::{chroma, contrast_ratio, hue_distance};
use crate::native_ui::{TextSpec, UiKey, UiTree};
use crate::system_state::marks::MARK_CONTRAST;

fn bundled_themes() -> Vec<(&'static str, aterm_render::Theme)> {
    aterm_types::scheme::builtin_names()
        .into_iter()
        .map(|name| {
            (
                name,
                aterm_render::Theme::from(
                    &aterm_types::scheme::builtin(name).expect("a listed builtin exists"),
                ),
            )
        })
        .collect()
}

/// Every word a mark stands for: each track word and the unread track cell, each activity word
/// and the unread activity cell, and an event.
fn every_word() -> Vec<Word<'static>> {
    Track::ALL
        .map(|track| Word::Track(Some(track)))
        .into_iter()
        .chain([Word::Track(None)])
        .chain(Activity::ALL.map(|activity| Word::Activity(Some(activity))))
        .chain([Word::Activity(None), Word::Event("commit")])
        .collect()
}

/// The role colour `ink` names in `roles`.
fn role_of(ink: Ink, roles: &Roles) -> [u8; 3] {
    match ink {
        Ink::Danger => roles.danger,
        Ink::Primary => roles.text_primary,
        Ink::Secondary => roles.text_secondary,
        Ink::Tertiary => roles.text_tertiary,
    }
}

/// ONLY OFF TRACK TAKES A HUE (§5.6 "Grey is normal"): of every word, `off-track` alone takes
/// `danger`, and `none` alone takes no ink; in every bundled theme off track's ink keeps danger's
/// hue, and every other word's ink is no more chromatic than the text role it is drawn from and
/// is never danger. CONTROL: some bundled theme has a `success` with a hue to take, and `done`
/// does not take it (so the test would see `done` drawn in STYLE-aterm's green); and `at-risk`
/// differs from `off-track` by its form while its ink is a text ink (a second form, not a second
/// hue).
#[test]
fn only_off_track_takes_a_hue() {
    let off_track = Word::Track(Some(Track::OffTrack));
    let untracked = Word::Track(Some(Track::Untracked));
    for word in every_word() {
        let look = look(word);
        assert_eq!(
            look.ink == Some(Ink::Danger),
            word == off_track,
            "{word:?} is drawn in {look:?}"
        );
        assert_eq!(look.ink.is_none(), word == untracked, "{word:?}: {look:?}");
    }

    let done = Word::Track(Some(Track::Done));
    let mut success_has_a_hue = 0;
    for (name, theme) in bundled_themes() {
        let roles = Roles::from_theme(theme);
        for word in every_word() {
            let Some(ink) = look(word).ink else {
                continue;
            };
            let rgb = ink_rgb(ink, &roles);
            if word == off_track {
                assert!(
                    hue_distance(rgb, roles.danger) <= 20.0,
                    "{name}: off track's {rgb:?} left danger's hue {:?}",
                    roles.danger
                );
                continue;
            }
            let role = role_of(ink, &roles);
            assert!(
                chroma(rgb) <= chroma(role) + 0.02,
                "{name}: {word:?}'s {rgb:?} is more chromatic than its role {role:?}"
            );
            assert_ne!(rgb, roles.danger, "{name}: {word:?} is drawn in danger");
        }
        if chroma(roles.success) >= 0.05 {
            success_has_a_hue += 1;
            let done_ink = ink_rgb(look(done).ink.expect("done is drawn"), &roles);
            assert_ne!(done_ink, roles.success, "{name}: done takes success");
        }
    }
    assert!(
        success_has_a_hue > 0,
        "no bundled theme's success has a hue"
    );
    let at_risk = look(Word::Track(Some(Track::AtRisk)));
    assert_ne!(at_risk.form, look(off_track).form);
    assert_eq!(at_risk.ink, Some(Ink::Primary));
}

/// The ten painted words, each by the word it stands for: the four track marks, the unread
/// cell, the four activity icons and the event mark.
fn ten_painted_words() -> [Word<'static>; 10] {
    [
        Word::Track(Some(Track::OffTrack)),
        Word::Track(Some(Track::AtRisk)),
        Word::Track(Some(Track::Done)),
        Word::Track(Some(Track::OnTrack)),
        Word::Track(None),
        Word::Activity(Some(Activity::Working)),
        Word::Activity(Some(Activity::Quiet)),
        Word::Activity(Some(Activity::Idle)),
        Word::Activity(Some(Activity::Ended)),
        Word::Event("commit"),
    ]
}

/// Where a form of `word` inks a `slot`-square raster in one grey over white at `scale`: a pixel
/// under 192 is inked.
fn grey_mask(word: Word<'_>, slot: f32, high_contrast: bool, scale: f32) -> Vec<bool> {
    let mut prims = Vec::new();
    let rect = LogicalRect::new(2.0, 2.0, slot, slot);
    paint_form(
        &mut prims,
        rect,
        look(word).form,
        [128, 128, 128],
        high_contrast,
    );
    assert!(
        prims
            .iter()
            .all(|prim| !matches!(prim, DrawPrim::Text { .. })),
        "{word:?} paints a glyph"
    );
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a slot of a few points"
    )]
    let side = (slot + 4.0) as u32;
    let (pixels, _, _) =
        crate::tray_raster::rasterize_tray(&prims, side, side, scale, [255, 255, 255, 255]);
    pixels.chunks_exact(4).map(|px| px[0] < 192).collect()
}

/// The share of the inked union where two masks disagree.
fn apart(a: &[bool], b: &[bool]) -> f64 {
    let union = a.iter().zip(b).filter(|(a, b)| **a || **b).count();
    let differ = a.iter().zip(b).filter(|(a, b)| a != b).count();
    #[allow(clippy::cast_precision_loss, reason = "pixel counts of a small raster")]
    let share = differ as f64 / union.max(1) as f64;
    share
}

/// The incentre of the triangle `corners`, in the units they are given in.
fn incentre(corners: [(f32, f32); 3]) -> (f32, f32) {
    let side = |a: (f32, f32), b: (f32, f32)| (a.0 - b.0).hypot(a.1 - b.1);
    let [a, b, c] = corners;
    let (wa, wb, wc) = (side(b, c), side(a, c), side(a, b));
    let sum = wa + wb + wc;
    (
        (wa * a.0 + wb * b.0 + wc * c.0) / sum,
        (wa * a.1 + wb * b.1 + wc * c.1) / sum,
    )
}

/// EVERY MARK AND ICON HAS ITS OWN FORM IN GREY (§5.6 "No meaning by hue alone"): each of the
/// ten painted words, drawn in one grey into a Title mark's slot and a Caption mark's slot, with
/// and without High Contrast, at 1x and 2x, inks some pixels and differs from every other in at
/// least a quarter of their inked union; `none` paints no prim; and the event mark in the Caption
/// slot under High Contrast at 1x keeps its hollow (the pixel at its incentre is about clear) on
/// a whole, a quarter and a half point. CONTROL: `quiet` and `stuck`, which share the waiting
/// form by design, give identical masks, so the measure tells one form from two; they share their
/// ink too, so on a row what tells them apart is the track mark beside them (`stuck` is read only
/// with an off-track one).
#[test]
fn every_mark_and_icon_has_its_own_form_in_grey() {
    let mut untracked = Vec::new();
    paint_form(
        &mut untracked,
        LogicalRect::new(0.0, 0.0, MARK_TITLE, MARK_TITLE),
        look(Word::Track(Some(Track::Untracked))).form,
        [128, 128, 128],
        false,
    );
    assert!(untracked.is_empty(), "none paints {untracked:?}");

    let words = ten_painted_words();
    for scale in [1.0, 2.0] {
        for slot in [MARK_TITLE, MARK_CAPTION] {
            for high_contrast in [false, true] {
                let at = format!("at {slot} pt, {scale}x (high contrast {high_contrast})");
                let masks: Vec<Vec<bool>> = words
                    .iter()
                    .map(|&word| grey_mask(word, slot, high_contrast, scale))
                    .collect();
                for (word, mask) in words.iter().zip(&masks) {
                    assert!(
                        mask.iter().any(|inked| *inked),
                        "{word:?} inks nothing {at}"
                    );
                }
                for (i, a) in masks.iter().enumerate() {
                    for (j, b) in masks.iter().enumerate().skip(i + 1) {
                        let share = apart(a, b);
                        assert!(
                            share >= 0.25,
                            "{:?} and {:?} differ in {:.0}% {at}",
                            words[i],
                            words[j],
                            share * 100.0
                        );
                    }
                }
                let quiet = Word::Activity(Some(Activity::Quiet));
                let stuck = Word::Activity(Some(Activity::Stuck));
                assert!(
                    apart(
                        &grey_mask(quiet, slot, high_contrast, scale),
                        &grey_mask(stuck, slot, high_contrast, scale)
                    ) == 0.0,
                    "quiet and stuck share a form"
                );
                assert_eq!(look(quiet), look(stuck));
            }
        }
    }

    let (hollow_x, hollow_y) = incentre(EVENT);
    let half = MARK_CAPTION / 2.0;
    for origin in [2.0_f32, 2.25, 2.5] {
        let mut prims = Vec::new();
        paint_form(
            &mut prims,
            LogicalRect::new(origin, origin, MARK_CAPTION, MARK_CAPTION),
            look(Word::Event("commit")).form,
            [128, 128, 128],
            true,
        );
        let (pixels, width, _) =
            crate::tray_raster::rasterize_tray(&prims, 16, 16, 1.0, [255, 255, 255, 255]);
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a pixel of a 16 px raster"
        )]
        let (column, row) = (
            (origin + half + hollow_x * half).floor() as usize,
            (origin + half + hollow_y * half).floor() as usize,
        );
        let red = pixels[(row * width as usize + column) * 4];
        assert!(
            red >= 250,
            "the caption-size event mark must stay hollow at 1x under High Contrast: its \
             incentre pixel ({column}, {row}) at origin {origin} is {red}"
        );
    }
}

/// A FILLED FORM IS SOLID INK INSIDE: the off-track diamond and working's triangle, with and
/// without High Contrast, at 1x and 2x, on a whole, a half and an odd quarter point, paint every
/// device pixel whose centre lies inside the form exactly in its ink, so the one red mark never
/// reads as a pale tile inside a darker rim. CONTROL: the hollow diamond, drawn the same way,
/// leaves its centre the ground, so the check reads the fill and not the outline.
#[test]
fn a_filled_form_is_solid_ink_inside() {
    const INK: [u8; 3] = [96, 96, 96];
    const SIDE: u32 = 20;
    let inside = |form: Form, (x, y): (f32, f32), (cx, cy, r): (f32, f32, f32)| match form {
        Form::Diamond | Form::HollowDiamond => (x - cx).abs() + (y - cy).abs() <= 0.9 * r,
        _ => {
            let (u, v) = ((x - cx) / r, (y - cy) / r);
            u >= -0.5 && v.abs() <= 0.65 * (0.65 - u) / 1.15
        }
    };
    for form in [Form::Diamond, Form::Play, Form::HollowDiamond] {
        for high_contrast in [false, true] {
            for scale in [1.0_f32, 2.0] {
                for (ox, oy) in [(0.0, 0.0), (0.5, 0.5), (0.25, 0.75)] {
                    let rect = LogicalRect::new(2.0 + ox, 2.0 + oy, MARK_TITLE, MARK_TITLE);
                    let centre = (rect.x + 8.0, rect.y + 8.0, 8.0);
                    let mut prims = Vec::new();
                    paint_form(&mut prims, rect, form, INK, high_contrast);
                    let (pixels, width, height) = crate::tray_raster::rasterize_tray(
                        &prims,
                        SIDE,
                        SIDE,
                        scale,
                        [255, 255, 255, 255],
                    );
                    let at = format!(
                        "{form:?} at ({ox}, {oy}), {scale}x (high contrast {high_contrast})"
                    );
                    if form == Form::HollowDiamond {
                        #[allow(
                            clippy::cast_possible_truncation,
                            clippy::cast_sign_loss,
                            reason = "a pixel of a small raster"
                        )]
                        let (column, row) = (
                            (centre.0 * scale).floor() as usize,
                            (centre.1 * scale).floor() as usize,
                        );
                        let px = &pixels[(row * width as usize + column) * 4..][..3];
                        assert_eq!(px, [255, 255, 255], "{at}: its centre is not the ground");
                        continue;
                    }
                    let mut solid = 0;
                    for row in 0..height {
                        for column in 0..width {
                            #[allow(
                                clippy::cast_precision_loss,
                                reason = "a pixel of a small raster"
                            )]
                            let point = ((column as f32 + 0.5) / scale, (row as f32 + 0.5) / scale);
                            if !inside(form, point, centre) {
                                continue;
                            }
                            let i = ((row * width + column) * 4) as usize;
                            assert_eq!(
                                &pixels[i..i + 3],
                                INK,
                                "{at}: pixel ({column}, {row}) inside the form is not its ink"
                            );
                            solid += 1;
                        }
                    }
                    assert!(solid > 0, "{at}: no pixel lies inside the form");
                }
            }
        }
    }
}

/// A representative point of a prim: where a prim of a mark lies.
fn prim_at(prim: &DrawPrim) -> Option<(f32, f32)> {
    match prim {
        DrawPrim::Line { x1, y1, x2, y2, .. } => Some(((x1 + x2) / 2.0, (y1 + y2) / 2.0)),
        DrawPrim::Dot { cx, cy, .. } | DrawPrim::Arc { cx, cy, .. } => Some((*cx, *cy)),
        DrawPrim::Panel { x, y, w, h, .. } => Some((x + w / 2.0, y + h / 2.0)),
        DrawPrim::Text { x, baseline, .. } => Some((*x, *baseline)),
        _ => None,
    }
}

/// The prims `compiled`'s tray paints inside `key`'s rect.
fn prims_inside(compiled: &crate::native_ui::CompiledUi, key: &UiKey) -> Vec<DrawPrim> {
    let rect = compiled
        .paint
        .iter()
        .find(|node| &node.key == key)
        .unwrap_or_else(|| panic!("{key:?} is painted"))
        .rect;
    compiled
        .tray(aterm_render::Theme::default(), 13.0)
        .prims
        .into_iter()
        .filter(|prim| {
            prim_at(prim).is_some_and(|(x, y)| {
                x >= rect.x - 0.5
                    && x <= rect.right() + 0.5
                    && y >= rect.y - 0.5
                    && y <= rect.bottom() + 0.5
            })
        })
        .collect()
}

/// `node` above one plain Text node, compiled at 320 × 400.
fn compiled_with(node: UiNode) -> crate::native_ui::CompiledUi {
    let words = UiNode::new(
        "words",
        UiContent::Text(TextSpec::text("Words beside a mark")),
    )
    .layout(Layout::default().height(Length::Fixed(20.0)));
    UiTree::new(
        UiNode::new(
            "root",
            UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
        )
        .layout(Layout::column().gap(8.0))
        .children(vec![node, words]),
    )
    .compile(LogicalRect::new(0.0, 0.0, 320.0, 400.0))
    .expect("the column compiles")
}

/// A MARK SPEAKS ITS WORD (§5.6, §5.2): each word's mark is a Text node whose label is its word
/// as a person reads it, and it paints its form and no glyph of the word (its paint-text record
/// paints nothing); `none` speaks nothing and keeps its slot's width. CONTROL: the same node
/// under another audit id paints its label as text, so the mark's hook is what keeps the word out
/// of the glyphs; and `quiet` and `stuck`, drawn alike, speak different words.
#[test]
fn a_mark_speaks_its_word() {
    let spoken_as = [
        (Word::Track(Some(Track::OffTrack)), "off track"),
        (Word::Track(Some(Track::AtRisk)), "at risk"),
        (Word::Track(Some(Track::Done)), "done"),
        (Word::Track(Some(Track::OnTrack)), "on track"),
        (Word::Track(None), "track unread"),
        (Word::Activity(Some(Activity::Working)), "working"),
        (Word::Activity(Some(Activity::Quiet)), "quiet"),
        (Word::Activity(Some(Activity::Stuck)), "stuck"),
        (Word::Activity(Some(Activity::Idle)), "idle"),
        (Word::Activity(Some(Activity::Ended)), "ended"),
        (Word::Activity(None), "activity unread"),
        (Word::Event("commit"), "commit"),
        (Word::Event("blocked"), "blocked on a person"),
    ];
    let key = UiKey::new("mark");
    for (word, said) in spoken_as {
        let compiled = compiled_with(mark_node("mark", word, MARK_TITLE));
        let semantic = compiled
            .semantic(&key)
            .unwrap_or_else(|| panic!("{word:?} has a semantic node"));
        assert_eq!(semantic.role, SemanticRole::Text, "{word:?}");
        assert_eq!(semantic.label, said, "{word:?}");
        let prims = prims_inside(&compiled, &key);
        assert!(!prims.is_empty(), "{word:?} paints nothing");
        assert!(
            prims
                .iter()
                .all(|prim| !matches!(prim, DrawPrim::Text { .. })),
            "{word:?} paints its word as a glyph: {prims:?}"
        );
        let audit = compiled
            .text_fit_audit_line(&key)
            .unwrap_or_else(|| panic!("{word:?} has a paint-text record"));
        assert!(audit.contains("painted=\"\""), "{word:?}: {audit}");
        assert!(audit.contains("overflow=false"), "{word:?}: {audit}");
    }

    let compiled = compiled_with(mark_node(
        "mark",
        Word::Track(Some(Track::Untracked)),
        MARK_TITLE,
    ));
    assert!(
        compiled
            .semantic(&key)
            .is_none_or(|node| node.label.is_empty()),
        "none speaks"
    );
    let slot = compiled
        .paint
        .iter()
        .find(|node| node.key == key)
        .expect("none keeps its slot")
        .rect;
    assert!((slot.width - MARK_TITLE).abs() < 0.01, "{slot:?}");

    let mut unhooked = mark_node("mark", Word::Track(Some(Track::OffTrack)), MARK_TITLE);
    if let UiContent::Custom(custom) = &mut unhooked.content {
        custom.audit_id = "comprehension.unknown";
    }
    let compiled = compiled_with(unhooked);
    assert!(
        prims_inside(&compiled, &key)
            .iter()
            .any(|prim| matches!(prim, DrawPrim::Text { s, .. } if s == "off track")),
        "a custom node with no hook paints its label"
    );
    let quiet = Word::Activity(Some(Activity::Quiet));
    let stuck = Word::Activity(Some(Activity::Stuck));
    assert_eq!(look(quiet), look(stuck));
    assert_ne!(spoken(quiet), spoken(stuck));
}

/// A survey row of an agent, live, with `agent=` and `needs=` as given.
fn agent_row<'a>(agent: &'a str, needs: Option<&'a str>) -> RowWords<'a> {
    RowWords {
        state: Some("alive"),
        program_kind: Some("agent"),
        agent: Some(agent),
        needs,
        ..RowWords::default()
    }
}

/// THE ROW IS READ AS THE GLANCE READS IT: a busy agent is busy; an idle one with an approval
/// standing waits, and reads idle with that need set aside; at a prompt its harness answers
/// (`needs=-`) it is busy, and at one whose row sends no needs it waits and reads busy set aside;
/// a wall is walled; a busy agent with a stalled task is stuck; an exited row has ended; a shell
/// has no glance word. CONTROL: the glance's own read of the same live agent rows, through the
/// fleet tables' sightings, gives each the same glance word.
#[test]
fn seen_of_row_reads_a_survey_row_as_the_glance_does() {
    let stalled = RowWords {
        task: Some("stalled"),
        ..agent_row("busy", Some("-"))
    };
    let exited = RowWords {
        state: Some("exited"),
        ..agent_row("busy", Some("-"))
    };
    let shell = RowWords {
        program_kind: Some("shell"),
        agent: Some("-"),
        ..agent_row("-", Some("-"))
    };
    let rows = [
        (
            agent_row("busy", Some("-")),
            State::Busy,
            State::Busy,
            false,
        ),
        (
            agent_row("idle", Some("approval")),
            State::Waiting,
            State::Idle,
            false,
        ),
        (
            agent_row("prompt", Some("-")),
            State::Busy,
            State::Busy,
            false,
        ),
        (
            agent_row("prompt", None),
            State::Waiting,
            State::Busy,
            false,
        ),
        (
            agent_row("wall:usage-weekly", Some("wall")),
            State::Walled,
            State::Walled,
            false,
        ),
        (stalled, State::Stuck, State::Stuck, false),
        (exited, State::Busy, State::Busy, true),
        (shell, State::Unknown, State::Unknown, false),
    ];
    for (row, glance, aside, ended) in rows {
        let seen = seen_of_row(&row);
        assert_eq!(
            (seen.glance, seen.aside, seen.ended),
            (glance, aside, ended),
            "{row:?}"
        );
    }
    assert_eq!(
        seen_of_row(&stalled).task,
        Some(aterm_db::words::Task::Stalled)
    );

    #[cfg(unix)]
    {
        use aterm_db::fleet::read::Answer;
        let live: Vec<(String, RowWords<'_>)> = rows
            .iter()
            .filter(|(row, ..)| row.program_kind == Some("agent") && row.state != Some("exited"))
            .enumerate()
            .map(|(i, (row, ..))| (format!("s-row{i:05}"), *row))
            .collect();
        let object = |sid: &str, row: &RowWords<'_>| {
            let mut members = vec![format!("\"sid\":\"{sid}\"")];
            for (key, value) in [
                ("state", row.state),
                ("program", Some("claude")),
                ("program_kind", row.program_kind),
                ("agent", row.agent),
                ("needs", row.needs),
                ("task", row.task),
                ("task_conclusion", row.task_conclusion),
            ] {
                if let Some(value) = value {
                    members.push(format!("\"{key}\":\"{value}\""));
                }
            }
            format!("{{{}}}", members.join(","))
        };
        let sessions: Vec<String> = live.iter().map(|(sid, row)| object(sid, row)).collect();
        let json = format!(
            "[{{\"label\":\"m4.local\",\"fleet\":true,\"instance\":{{\"pid\":\"7\",\
             \"host\":\"m4.local\",\"sessions\":\"{}\",\"level\":\"facts\",\"cut\":\"0\",\
             \"node\":\"n4\"}},\"sessions\":[{}]}}]",
            sessions.len(),
            sessions.join(",")
        );
        let read = crate::fleet_glance::sightings(&json, 1_000).expect("a survey");
        let agents = read
            .machines
            .iter()
            .find_map(|machine| match &machine.answer {
                Answer::Answered(answered) => Some(&answered.agents),
                Answer::Silent { .. } => None,
            })
            .expect("the peer answered");
        assert_eq!(agents.len(), live.len(), "every live agent row is sighted");
        for (sid, row) in &live {
            let sighted = agents
                .iter()
                .find(|agent| &agent.sid == sid)
                .unwrap_or_else(|| panic!("{sid} is sighted"));
            assert_eq!(sighted.state, seen_of_row(row).glance, "{row:?}");
        }
    }
}

/// THIS MAC'S ACTIVITY WORD IS THE READER'S FALLBACK: over the specimen's rows and every verdict
/// (unread included), this Mac reads each session as a reader of its row does. CONTROL: a row
/// whose need blocks nothing (an approval no verdict says blocks it) reads, under `progress`, the
/// word it has with the need set aside, not the wait's `quiet`.
#[test]
fn this_macs_activity_word_is_the_readers_fallback() {
    let verdicts: Vec<Option<TrackWhy>> = std::iter::once(None)
        .chain(TrackWhy::ALL.map(Some))
        .collect();
    for (row, _) in crate::comprehension_specimen::rows() {
        for &verdict in &verdicts {
            assert_eq!(
                activity_here(&row, verdict),
                aterm_db::words::activity(&seen_of_row(&row), verdict),
                "{row:?} under {verdict:?}"
            );
        }
    }
    let approval = agent_row("idle", Some("approval"));
    assert_eq!(
        activity_here(&approval, Some(TrackWhy::Progress)),
        Some(Activity::Idle)
    );
    assert_eq!(activity_here(&approval, None), Some(Activity::Quiet));
}

/// A MARK READS 3:1 ON EVERY GROUND A VIEW SETS IT ON: in every bundled theme each resolved ink
/// clears [`MARK_CONTRAST`] on the page, a card or popover and a grouped form's card, bare and
/// under a hovered row's wash in sRGB and linear light. CONTROL: the page's own colour, put
/// through the same hold, comes out at least 3:1 from every one of those grounds, so the floor is
/// the hold's and not a lucky role colour's.
#[test]
fn a_mark_reads_three_to_one_on_every_ground_a_view_sets_it_on() {
    for (name, theme) in bundled_themes() {
        let roles = Roles::from_theme(theme);
        let on = grounds(&roles);
        for ink in [Ink::Danger, Ink::Primary, Ink::Secondary, Ink::Tertiary] {
            let rgb = ink_rgb(ink, &roles);
            for ground in on {
                assert!(
                    contrast_ratio(rgb, ground) >= MARK_CONTRAST,
                    "{name}: {ink:?} {rgb:?} on {ground:?} is {:.2}:1",
                    contrast_ratio(rgb, ground)
                );
            }
        }
        let page = held(roles.surface, &roles);
        for ground in on {
            assert!(
                contrast_ratio(page, ground) >= MARK_CONTRAST,
                "{name}: the page held is {:.2}:1 on {ground:?}",
                contrast_ratio(page, ground)
            );
        }
    }
}

/// A MARK'S VALUE NAMES ITS WORD: each track and activity word's value parses back to it, and
/// the event mark's to an event of the same look. CONTROL: a value that names no word
/// (`track:maybe`, `event:`, `activity:`, a bare word) paints no prim, as no value does.
#[test]
fn a_mark_value_round_trips_and_an_unknown_one_paints_nothing() {
    for word in every_word() {
        let parsed = Word::parse(&word.value()).unwrap_or_else(|| panic!("{word:?}"));
        match word {
            Word::Event(_) => {
                assert!(matches!(parsed, Word::Event(_)), "{word:?}");
                assert_eq!(look(parsed), look(word));
            }
            _ => assert_eq!(parsed, word),
        }
    }
    let roles = Roles::from_theme(aterm_render::Theme::default());
    let rect = LogicalRect::new(0.0, 0.0, MARK_TITLE, MARK_TITLE);
    let mut painted = Vec::new();
    paint_mark(&mut painted, rect, Some("track:off-track"), &roles);
    assert!(!painted.is_empty(), "a word's value paints");
    for value in [
        Some("track:maybe"),
        Some("event:"),
        Some("activity:"),
        Some("off-track"),
        Some("track:none"),
        None,
    ] {
        let mut prims = Vec::new();
        paint_mark(&mut prims, rect, value, &roles);
        assert!(prims.is_empty(), "{value:?} paints {prims:?}");
    }
}
