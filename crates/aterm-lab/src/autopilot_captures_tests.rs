// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `autopilot-captures`' manifest held to its plan. The recorded run's manifest
//! (`fixtures/autopilot-captures-manifest.tsv`: `--harness all` at 1x and 2x, in Aterm Day and
//! Aterm Night) has a row for every harness × scene × look × scale, each a png, a skip naming its
//! reason, or a NOT CAPTURED row naming why, and none of its pngs shows anything wrong; a manifest
//! missing any one of them, or with a png that shows something wrong, is caught.

use std::cell::Cell;

use super::*;

/// The recorded run's manifest.
const RECORDED: &str = include_str!("fixtures/autopilot-captures-manifest.tsv");

/// The recorded run's planned captures that have no png, each with its row's why: codex-alab's
/// Land, landed and need legs. The managed codex-alab package that run had (store build
/// 2026102001, 0.164.0-dev.12479+g07a25d86a) typed Cruise's acts on a Land tab and raised no need,
/// so those legs never reached their scenes; `codex-alab-proof` fails the same legs with it, on
/// main's lab and aterm as on this tree's.
const UNCAPTURED_HERE: [(&str, &str); 3] = [
    ("codex-alab", "land"),
    ("codex-alab", "landed"),
    ("codex-alab", "need"),
];

const SCALES: [u8; 2] = [1, 2];

fn recorded() -> Vec<Row> {
    parse_manifest(RECORDED)
}

/// The recorded rows, each changed by `change`.
fn recorded_with(change: impl Fn(&mut Row)) -> Vec<Row> {
    let mut rows = recorded();
    rows.iter_mut().for_each(change);
    rows
}

/// Every harness's every scene, with its plan.
fn keys() -> Vec<(Who, Scene, Plan)> {
    Who::ALL
        .iter()
        .flat_map(|&w| w.scenes().iter().map(move |&(s, p)| (w, s, p)))
        .collect()
}

/// The manifest a run that shows every planned scene writes: a rest png for each planned capture,
/// a skip naming its reason for each planned skip.
fn planned() -> Vec<Row> {
    let mut rows = Vec::new();
    for (who, scene, plan) in keys() {
        for scale in SCALES {
            for look in Look::BOTH {
                rows.push(match plan {
                    Plan::Skip(why) => skip_row(who, scene, why, look, scale),
                    Plan::Chain(_) | Plan::Leg(_) => {
                        let png = png_name(who, scene, look, scale, Kind::Rest);
                        Row::new(who, scene, look, scale, &png, scene.what().to_string())
                    }
                });
            }
        }
    }
    rows
}

/// [`planned`] as a clean run shoots it: each rest png's caption names its chip as `chrome` drew
/// it, and where a chip is drawn its hovered title's png sits beside it.
fn shown() -> Vec<Row> {
    let mut rows = Vec::new();
    for r in planned() {
        let Some((who, scene, look, scale)) = r.keys().filter(|_| r.png != "-") else {
            rows.push(r);
            continue;
        };
        let line = match who.chipless() {
            None => r#"window=0 program=claude face=cruise drawn="∞" title="autopilot: cruise""#,
            Some(_) => "window=0 program=gemini face=- drawn=- why=no-footer",
        };
        let caption = format!("{}; {}", scene.what(), chip_words(line));
        if who.chipless().is_none() {
            let png = png_name(who, scene, look, scale, Kind::Title);
            rows.push(Row::new(who, scene, look, scale, &png, caption.clone()));
        }
        rows.push(Row { caption, ..r });
    }
    rows
}

fn without(rows: &[Row], who: Who, scene: Scene) -> Vec<Row> {
    rows.iter()
        .filter(|r| !(r.harness == who.name() && r.scene == scene.name()))
        .cloned()
        .collect()
}

fn gaps_of(rows: &[Row]) -> Vec<Gap> {
    missing(rows, &Who::ALL, &Look::BOTH, &SCALES)
}

#[test]
fn the_recorded_manifest_has_a_row_for_every_harness_scene_look_and_scale() {
    let rows = recorded();
    for g in gaps_of(&rows) {
        let named = matches!(&g.lack, Lack::NotCaptured(why) if !why.trim().is_empty());
        assert!(named, "the recorded manifest lacks {g}");
        assert!(
            UNCAPTURED_HERE.contains(&(g.who.name(), g.scene.name())),
            "the recorded manifest has no png for {g}"
        );
    }
    assert_eq!(
        manifest_text(&rows),
        RECORDED,
        "the recorded manifest is not the verb's own text"
    );
}

#[test]
fn the_recorded_manifests_pngs_show_nothing_wrong() {
    let flaws = faults(&recorded());
    assert!(
        flaws.is_empty(),
        "{}",
        flaws
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    );
    // Every harness that draws a chip has its hovered titles in the recording.
    for who in Who::ALL {
        let titled = recorded()
            .iter()
            .filter(|r| r.harness == who.name() && r.kind() == Some(Kind::Title))
            .count();
        let chipped = who
            .scenes()
            .iter()
            .any(|&(_, p)| !matches!(p, Plan::Skip(_)))
            && who.chipless().is_none();
        assert_eq!(titled > 0, chipped, "{who:?}: {titled} title png(s)");
    }
}

#[test]
fn a_recorded_manifest_missing_any_one_harness_scene_fails() {
    // The negative control of the test above, at every key of the recording: drop its rows and
    // the gap is named as no row.
    let rows = recorded();
    for (who, scene, _) in keys() {
        let dropped = without(&rows, who, scene);
        assert!(
            dropped.len() < rows.len(),
            "the recording has no {who:?} {scene:?}"
        );
        let gaps = gaps_of(&dropped);
        assert!(
            gaps.iter()
                .any(|g| (g.who, g.scene, &g.lack) == (who, scene, &Lack::NoRow)),
            "{who:?} {scene:?} dropped, yet no gap names it: {gaps:#?}"
        );
    }
}

#[test]
fn a_recording_without_its_title_shots_fails() {
    let rows: Vec<Row> = recorded()
        .into_iter()
        .filter(|r| r.kind() != Some(Kind::Title))
        .collect();
    let chipped = rows
        .iter()
        .filter(|r| r.kind() == Some(Kind::Rest) && drawn_of(&r.caption).is_some())
        .count();
    assert!(chipped > 0, "the recording draws no chip at rest");
    let flaws = faults(&rows);
    assert_eq!(flaws.len(), chipped, "{flaws:#?}");
    assert!(
        flaws
            .iter()
            .all(|f| f.fault == Fault::NoTitle("no title row".into())),
        "{flaws:#?}"
    );
}

#[test]
fn a_recording_with_a_finding_in_its_captions_fails() {
    let rows = recorded_with(|r| {
        if r.look == Look::Night.name() && r.png != "-" {
            r.caption
                .push_str(&format!("; {FINDING}a night frame of mean luminance 0.95"));
        }
    });
    let marked = rows
        .iter()
        .filter(|r| r.look == Look::Night.name() && r.png != "-")
        .count();
    assert!(marked > 0);
    let flaws = faults(&rows);
    assert_eq!(flaws.len(), marked, "{flaws:#?}");
    assert!(
        flaws
            .iter()
            .all(|f| f.fault == Fault::Finding("a night frame of mean luminance 0.95".into())),
        "{flaws:#?}"
    );
}

#[test]
fn a_recording_with_no_chip_where_its_harness_draws_one_fails() {
    let footless = chip_words("window=0 program=claude face=- drawn=- why=no-footer");
    let rows = recorded_with(|r| {
        if r.harness != Who::Agent(Harness::Claude).name() {
            return;
        }
        if let (Some(face), Some(drawn)) = (caption_word(&r.caption, "face"), drawn_of(&r.caption))
        {
            let was = chip_words(&format!("face={face} drawn={drawn:?}"));
            assert!(r.caption.contains(&was), "{}", r.caption);
            r.caption = r.caption.replace(&was, &footless);
        }
    });
    let rests = rows
        .iter()
        .filter(|r| r.harness == "claude" && r.kind() == Some(Kind::Rest))
        .count();
    assert!(rests > 0);
    let flaws = faults(&rows);
    assert_eq!(flaws.len(), rests, "{flaws:#?}");
    assert!(
        flaws
            .iter()
            .all(|f| f.fault == Fault::NoChip("no-footer".into())),
        "{flaws:#?}"
    );
}

#[test]
fn a_recording_whose_pngs_are_named_for_another_harness_fails() {
    let rows = recorded_with(|r| {
        if r.harness == "claude" && r.png != "-" {
            r.png = r.png.replacen("claude-", "agy-", 1);
        }
    });
    let renamed = rows
        .iter()
        .filter(|r| r.harness == "claude" && r.png != "-")
        .count();
    assert!(renamed > 0);
    let flaws = faults(&rows);
    assert_eq!(flaws.len(), renamed, "{flaws:#?}");
    assert!(
        flaws
            .iter()
            .all(|f| matches!(&f.fault, Fault::Misnamed(png) if png.starts_with("agy-"))),
        "{flaws:#?}"
    );
}

#[test]
fn a_clean_run_shows_nothing_wrong_and_lacks_nothing() {
    let rows = shown();
    assert_eq!(faults(&rows), []);
    assert_eq!(gaps_of(&rows), []);
}

#[test]
fn a_title_shot_that_gave_no_frame_is_named_not_dropped() {
    let mut rows = shown();
    let at = rows
        .iter()
        .position(|r| r.harness == "claude" && r.kind() == Some(Kind::Title))
        .expect("a Claude Code title png");
    let gone = rows.remove(at);
    let why = "`image` gave no frame of @s-1 with the pointer on the chip (OK at=15,4)";
    rows.push(Row {
        png: "-".into(),
        caption: format!("{NOT_CAPTURED}{why}"),
        ..gone.clone()
    });
    assert_eq!(gaps_of(&rows), [], "its rest png still stands for the cell");
    assert_eq!(
        faults(&rows),
        [Flaw {
            at: format!(
                "{} {} {} {}x",
                gone.harness, gone.scene, gone.look, gone.scale
            ),
            fault: Fault::NoTitle(why.into()),
        }]
    );
}

#[test]
fn a_run_showing_every_planned_scene_lacks_nothing() {
    assert_eq!(gaps_of(&planned()), []);
}

#[test]
fn a_manifest_missing_one_harness_scene_or_one_look_or_scale_row_fails() {
    let rows = planned();
    for (who, scene, _) in keys() {
        let gaps = gaps_of(&without(&rows, who, scene));
        assert_eq!(gaps.len(), Look::BOTH.len() * SCALES.len(), "{gaps:#?}");
        assert!(
            gaps.iter()
                .all(|g| (g.who, g.scene, &g.lack) == (who, scene, &Lack::NoRow)),
            "{gaps:#?}"
        );
    }
    for i in 0..rows.len() {
        let mut dropped = rows.clone();
        let gone = dropped.remove(i);
        let gaps = gaps_of(&dropped);
        assert_eq!(gaps.len(), 1, "{} dropped: {gaps:#?}", gone.line());
        assert_eq!(gaps[0].lack, Lack::NoRow);
        assert!(gone.of(gaps[0].who, gaps[0].scene, gaps[0].look, gaps[0].scale));
    }
}

#[test]
fn codex_alab_holds_pass_as_skips_naming_their_reason() {
    let holds: Vec<Scene> = Who::CodexAlab
        .scenes()
        .iter()
        .filter(|(_, p)| *p == Plan::Skip(A3_WAITS))
        .map(|(s, _)| *s)
        .collect();
    assert_eq!(holds, [Scene::LandOpen, Scene::LandRepo, Scene::SameLand]);
    for rows in [planned(), recorded()] {
        for &scene in &holds {
            let skips: Vec<&Row> = rows
                .iter()
                .filter(|r| r.harness == "codex-alab" && r.scene == scene.name())
                .collect();
            assert_eq!(skips.len(), Look::BOTH.len() * SCALES.len(), "{scene:?}");
            for r in skips {
                assert_eq!(
                    (r.png.as_str(), r.caption.clone()),
                    ("-", format!("{SKIPPED}{A3_WAITS}")),
                    "{}",
                    r.line()
                );
            }
        }
        let codex_holds = missing(&rows, &[Who::CodexAlab], &Look::BOTH, &SCALES);
        assert!(
            codex_holds.iter().all(|g| !holds.contains(&g.scene)),
            "{codex_holds:#?}"
        );
    }
}

#[test]
fn a_skip_naming_no_reason_fails() {
    // The word alone, and the word with a blank reason after it.
    for caption in [SKIPPED.trim_end().to_string(), format!("{SKIPPED}  ")] {
        let mut rows = planned();
        let at = rows
            .iter()
            .position(|r| r.harness == "codex-alab" && r.scene == "land-open")
            .expect("a codex-alab land-open row");
        rows[at].caption.clone_from(&caption);
        let gaps = gaps_of(&rows);
        assert_eq!(gaps.len(), 1, "{caption:?}: {gaps:#?}");
        assert_eq!(
            (gaps[0].who, gaps[0].scene, &gaps[0].lack),
            (Who::CodexAlab, Scene::LandOpen, &Lack::Unreasoned),
            "{caption:?}"
        );
    }
}

#[test]
fn a_planned_scene_present_only_as_a_skip_or_uncaptured_fails() {
    let rows = planned();
    let swapped = |caption: &str| -> Vec<Row> {
        let who = Who::Agent(Harness::Claude);
        let mut v = without(&rows, who, Scene::CruiseNeeds);
        for look in Look::BOTH {
            for scale in SCALES {
                v.push(Row::new(
                    who,
                    Scene::CruiseNeeds,
                    look,
                    scale,
                    "-",
                    caption.to_string(),
                ));
            }
        }
        v
    };
    let gaps = gaps_of(&swapped("skipped: not today"));
    assert_eq!(gaps.len(), 4, "{gaps:#?}");
    assert!(gaps.iter().all(|g| g.lack == Lack::Skipped), "{gaps:#?}");
    let gaps = gaps_of(&swapped(&format!("{NOT_CAPTURED}the lab did not come up")));
    assert_eq!(gaps.len(), 4, "{gaps:#?}");
    assert!(
        gaps.iter()
            .all(|g| g.lack == Lack::NotCaptured("the lab did not come up".into())),
        "{gaps:#?}"
    );
}

#[test]
fn a_row_survives_its_line() {
    let r = Row::new(
        Who::CodexAlab,
        Scene::Need,
        Look::Night,
        2,
        "codex-alab-need-night-2x-rest.png",
        "a\ttab and\na line break".to_string(),
    );
    let back = parse_manifest(&manifest_text(std::slice::from_ref(&r)));
    assert_eq!(back.len(), 1);
    assert_eq!(back[0].caption, "a tab and a line break");
    assert_eq!(back[0].png, r.png);
    assert_eq!(back[0].kind(), Some(Kind::Rest));
}

#[test]
fn the_verb_takes_its_words_and_refuses_others() {
    let words = |a: &[&str]| parse_args(&a.iter().map(|s| (*s).to_string()).collect::<Vec<_>>());
    let all = words(&["--harness", "all", "--scale", "2", "--look", "night"])
        .expect("parsed")
        .expect("asked");
    assert_eq!(all.whos, Who::ALL);
    assert_eq!((all.scale, all.looks.as_slice()), (2, &[Look::Night][..]));
    assert_eq!((all.cols, all.lines), (COLS, LINES));
    let one = words(&["--harness", "codex-alab"])
        .expect("parsed")
        .expect("asked");
    assert_eq!(
        (one.whos.as_slice(), one.looks.len()),
        (&[Who::CodexAlab][..], 2)
    );
    let pair = words(&["--baseline", "/x/aterm"])
        .expect("parsed")
        .expect("asked");
    assert!(pair.whos.is_empty() && pair.baseline.is_some());
    for bad in [
        &["--harness", "vim"][..],
        &["--harness", "all", "--scale", "3"],
        &["--harness", "all", "--look", "dusk"],
        &[],
    ] {
        assert_eq!(
            words(bad).map(|_| ()).map_err(|f| f.code),
            Err(2),
            "{bad:?}"
        );
    }
}

#[test]
fn a_look_is_its_instances_theme_from_launch() {
    let doc = |t: &str| aterm_toml::from_str::<aterm_toml::Value>(t).expect("one document");
    let theme = |v: &aterm_toml::Value| {
        v.get("theme")
            .and_then(aterm_toml::Value::as_str)
            .map(str::to_string)
    };
    assert_eq!(
        theme(&doc(&themed("", Look::Day.theme()))).as_deref(),
        Some(Look::Day.theme())
    );
    // `LAB_ATERM_TOML`'s own top-level theme gives way; a table's `theme` is its own.
    let mine = themed(
        "theme = \"Nord\"\nfont_px = 13\n[gemini]\ntheme = \"x\"",
        Look::Night.theme(),
    );
    let v = doc(&mine);
    assert_eq!(theme(&v).as_deref(), Some(Look::Night.theme()));
    assert_eq!(
        v.get("font_px").and_then(aterm_toml::Value::as_integer),
        Some(13)
    );
    assert_eq!(
        v.get("gemini").and_then(theme).as_deref(),
        Some("x"),
        "{mine}"
    );
}

/// A lab whose `chrome` names `labels`, selecting `selected` before a `tab` and `after` once one
/// was asked, and whose `tab` says `tab_says`.
struct Strip {
    labels: &'static str,
    selected: usize,
    after: usize,
    tab_says: &'static str,
    tabbed: Cell<bool>,
}

impl Lab for Strip {
    fn said(&self, args: &[&str]) -> String {
        match args.first().copied() {
            Some("chrome") => format!(
                "toolbar (none)\ntoolbar-tabs count=3 selected={} labels={} states=[[], [], []]\n",
                if self.tabbed.get() {
                    self.after
                } else {
                    self.selected
                },
                self.labels
            ),
            Some("tab") => {
                self.tabbed.set(true);
                self.tab_says.to_string()
            }
            _ => String::new(),
        }
    }

    fn images(&self) -> PathBuf {
        PathBuf::new()
    }
}

#[test]
fn a_scene_is_shot_only_once_its_tab_is_in_front() {
    let strip = |after: usize, tab_says: &'static str| Strip {
        labels: r#"["proj", "land-b", "off"]"#,
        selected: 0,
        after,
        tab_says,
        tabbed: Cell::new(false),
    };
    assert!(raise(&strip(1, "OK 1 3\n"), "land-b").is_ok());
    // No such tab, a `tab` that did not select it, and a refused `tab`.
    assert!(raise(&strip(1, "OK 1 3\n"), "land-c").is_err());
    assert!(raise(&strip(0, "OK 1 3\n"), "land-b").is_err());
    assert!(raise(&strip(1, "ERR no such tab\n"), "land-b").is_err());
}

#[test]
fn the_typing_take_needs_one_recording_that_outlasts_its_burst() {
    assert!(recording_held("OK video /x/keys.mov frames=600\n", true).is_ok());
    // Refused at once, so it answered before the burst was over; and one that ran out first.
    assert!(recording_held("ERR video: keys is owner-only\n", false).is_err());
    assert!(recording_held("OK video /x/keys.mov frames=600\n", false).is_err());
    assert!(recording_held("ERR video: the recording failed\n", true).is_err());
}

fn frame(w: usize, h: usize, px: [u8; 4]) -> Frame {
    Frame {
        rgba: px.repeat(w * h),
        w,
        h,
    }
}

fn ink(f: &mut Frame, xs: impl Iterator<Item = usize> + Clone, ys: std::ops::Range<usize>) {
    for y in ys {
        for x in xs.clone() {
            let i = (y * f.w + x) * 4;
            f.rgba[i..i + 3].copy_from_slice(&[20, 20, 20]);
        }
    }
}

#[test]
fn a_title_is_read_off_the_rule_band_past_the_chip() {
    let light = [250, 250, 250, 255];
    let rest = frame(200, 40, light);
    let c = ChipAt {
        row: 1,
        col: 1,
        x: 5.0,
        y: 20.0,
        cw: 2.0,
        ch: 10.0,
    };
    assert!(c.title_share(&rest, &rest) < TITLE_INK);
    // A title's glyphs across the band (20 to 70 cells on from the chip).
    let mut hovered = frame(200, 40, light);
    ink(&mut hovered, (45..145).step_by(3), 16..24);
    assert!(c.title_share(&hovered, &rest) >= TITLE_INK);
    // The same ink short of the band (the pet at the pointer, on the chip) is no title.
    let mut pet = frame(200, 40, light);
    ink(&mut pet, 0..40, 0..40);
    assert!(c.title_share(&pet, &rest) < TITLE_INK);
    // A frame of another size is no title either.
    assert!(c.title_share(&frame(100, 40, light), &rest) < TITLE_INK);
}

/// Cells 4 pixels wide and 10 high, the chip's centred on x 2: a rule's line across row 5 at rest.
const CELL: usize = 4;

fn rule(cells: usize) -> Frame {
    let mut f = frame(cells * CELL, 10, [250, 250, 250, 255]);
    for x in 0..f.w {
        let i = (5 * f.w + x) * 4;
        f.rgba[i..i + 3].copy_from_slice(&[150, 150, 150]);
    }
    f
}

/// `rest` with `text` drawn into the rule from cell `at`, and the blank after it: a glyph inked
/// (the line under it gone), a blank the line gone, and `—` left as the line was.
fn drawn(rest: &Frame, at: usize, text: &str) -> Frame {
    let mut f = Frame {
        rgba: rest.rgba.clone(),
        w: rest.w,
        h: rest.h,
    };
    for (i, ch) in text.chars().chain([' ']).enumerate() {
        let x0 = (at + i) * CELL;
        if ch == '—' {
            continue;
        }
        for x in x0..x0 + CELL {
            let i = (5 * f.w + x) * 4;
            f.rgba[i..i + 3].copy_from_slice(&[250, 250, 250]);
        }
        if ch != ' ' {
            ink(&mut f, x0 + 1..x0 + 3, 2..8);
        }
    }
    f
}

#[test]
fn a_hovered_titles_cells_are_counted_to_its_end() {
    let c = ChipAt {
        row: 0,
        col: 0,
        x: 2.0,
        y: 5.0,
        cw: 4.0,
        ch: 10.0,
    };
    let rest = rule(160);
    let lead = text_width("∞") + 2;
    let whole = "autopilot: cruise — keeps going for ever · needs you: grant it · click: land";
    let line = format!("window=0 program=claude face=cruise drawn=\"∞\" title={whole:?}");
    let h = |text: &str| c.title_cells(&drawn(&rest, lead, text), &rest, lead);
    // The whole title, its blank after it counted too.
    assert_eq!(h(whole), text_width(whole) + 1);
    let said = |cells: usize| title_verdict(Hovered { share: 0.2, cells }, &line);
    assert!(!said(h(whole)).contains(FINDING), "{}", said(h(whole)));
    // The short fit the review saw at 240 columns is cut short.
    let short = "autopilot: cruise · click: land";
    assert_eq!(h(short), text_width(short) + 1);
    assert!(said(h(short)).starts_with(FINDING), "{}", said(h(short)));
    // Within the slack is whole; a cell past it is not.
    let want = text_width(whole);
    assert!(!said(want - TITLE_SLACK).contains(FINDING));
    assert!(said(want - TITLE_SLACK - 1).starts_with(FINDING));
    // A title that did not show at all is a finding whatever it measured.
    assert!(
        title_verdict(
            Hovered {
                share: 0.01,
                cells: h(whole)
            },
            &line
        )
        .starts_with(FINDING)
    );
    // Four cells left as the rule was end a title; the facts redrawn far along the rule are none
    // of it.
    assert_eq!(h("ab————cd"), 2);
    let mut facts = drawn(&rest, lead, short);
    ink(&mut facts, 140 * CELL..150 * CELL, 2..8);
    assert_eq!(c.title_cells(&facts, &rest, lead), text_width(short) + 1);
    // Nothing drawn, and a frame of another size, measure none.
    assert_eq!(c.title_cells(&rest, &rest, lead), 0);
    assert_eq!(c.title_cells(&rule(80), &rest, lead), 0);
}

#[test]
fn a_look_holds_its_ground() {
    let day = frame(10, 10, [246, 247, 250, 255]);
    let night = frame(10, 10, [20, 22, 30, 255]);
    assert!(Look::Day.ground_holds(day.luma()));
    assert!(!Look::Night.ground_holds(day.luma()));
    assert!(Look::Night.ground_holds(night.luma()));
    assert!(!Look::Day.ground_holds(night.luma()));
}

#[test]
fn rules_are_found_bottom_first() {
    let rule = "─".repeat(30);
    let text = format!("top\n{rule}\n> \n{rule}\n  ∞ autopilot\n");
    assert_eq!(rule_rows(&text), [3, 1]);
}
