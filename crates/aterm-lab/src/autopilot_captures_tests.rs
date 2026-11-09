// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `autopilot-captures`' manifest held to its plan. The recorded run's manifest
//! (`fixtures/autopilot-captures-manifest.tsv`: `--harness all` at 1x and 2x, in Aterm Day and
//! Aterm Night) has a row for every harness × scene × look × scale, each a png, a skip naming its
//! reason, or a NOT CAPTURED row naming why, and none of its pngs shows anything wrong; a manifest
//! missing any one of them, or with a png that shows something wrong, is caught.

use std::cell::Cell;

use super::*;

/// The recorded run's manifest. Its codex-alab rows are a later run's, on codex-alab
/// 0.1.0-dev.14249 (2026-10-31, `--harness codex-alab --look both` at 1x and 2x), every planned
/// scene a png; their hold skips carry this tree's reason ([`HOLDS_UNSHOT`]). Its Claude Code,
/// agy and Muse Code rows are slice 4b's (2026-11-01, Claude Code 2.1.316, agy 1.4.12 and Muse
/// Code 1.4.14, each look at 1x and 2x), every planned scene a png: Claude Code's 1x Night, 2x
/// Day and 2x Night on 4b's build before main's scoped input, whose same-land never settled
/// (design §14.17.4.1); agy's and Muse Code's `switch-asked` rows are skips with this tree's
/// reason ([`SWITCH_ON_CLAUDE`]).
const RECORDED: &str = include_str!("fixtures/autopilot-captures-manifest.tsv");

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
            Some(_) => "window=0 program=codex face=cruise drawn=- why=not-drawn",
        };
        let mut caption = format!("{}; {}", scene.what(), chip_words(line));
        if who.chipless().is_some() {
            caption.push_str(&pad_words(
                line,
                Some("codex-alab 0.1.0-dev.14249 \u{2500}\u{2500} \u{221e} \u{2500}[Cruise]\u{2500}[Land]\u{2500}[Off]"),
                true,
            ));
        }
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
    let gaps: Vec<String> = gaps_of(&rows).iter().map(ToString::to_string).collect();
    assert!(gaps.is_empty(), "the recorded manifest lacks {gaps:?}");
    assert!(
        !gaps_of(&without(&rows, Who::CodexAlab, Scene::Landed)).is_empty(),
        "CONTROL: codex-alab's landed rows taken out"
    );
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
        .filter(|(_, p)| *p == Plan::Skip(HOLDS_UNSHOT))
        .map(|(s, _)| *s)
        .collect();
    assert_eq!(
        holds,
        [
            Scene::LandOpen,
            Scene::LandRepo,
            Scene::SameLand,
            Scene::SwitchAsked
        ]
    );
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
                    ("-", format!("{SKIPPED}{HOLDS_UNSHOT}")),
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
        "theme = \"Nord\"\nfont_px = 13\n[agy]\ntheme = \"x\"",
        Look::Night.theme(),
    );
    let v = doc(&mine);
    assert_eq!(theme(&v).as_deref(), Some(Look::Night.theme()));
    assert_eq!(
        v.get("font_px").and_then(aterm_toml::Value::as_integer),
        Some(13)
    );
    assert_eq!(v.get("agy").and_then(theme).as_deref(), Some("x"), "{mine}");
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

    fn aterm_pid(&self) -> Option<i32> {
        None
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

#[test]
fn a_takes_cost_is_aterms_cpu_and_instructions_per_rendered_frame() {
    // Apple silicon's 24 MHz ticks (125/3 ns each): 24,000,000 ticks is one second.
    let tb = Some((125, 3));
    let at = |ticks, insn| {
        Some(Spent {
            ticks,
            insn,
            main: None,
        })
    };
    // The main thread (id `tid`), its CPU time in ns and its instructions.
    let main_at = |ticks, insn, tid, ns, main_insn| {
        Some(Spent {
            ticks,
            insn,
            main: Some(MainSpent {
                tid,
                ns,
                insn: main_insn,
            }),
        })
    };
    // The main thread ran 0.4 of the take's second and retired a quarter of its instructions.
    assert_eq!(
        spent_words(
            main_at(1_000, 7, 9, 5, Some(50_000_000)),
            main_at(24_001_000, 400_000_007, 9, 400_000_005, Some(150_000_000)),
            Some(400),
            tb
        ),
        "cpu_ms=1000.0 cpu_us_per_render=2500.0 kinsn_per_render=1000.0 \
         main_us_per_render=1000.0 main_kinsn_per_render=250.0"
    );
    // A third of a frame's microseconds kept to the tenth, not rounded up.
    assert_eq!(
        spent_words(at(0, 0), at(24_000, 30_000), Some(3), tb),
        "cpu_ms=1.0 cpu_us_per_render=333.3 kinsn_per_render=10.0 main_us_per_render=- \
         main_kinsn_per_render=-"
    );
    // Nothing guessed: no reading, no timebase, no frame, no retired-instruction count, a
    // counter that went backwards (the pid named another process), or a main thread that is
    // not the one read first reads `-`.
    let none = "cpu_ms=- cpu_us_per_render=- kinsn_per_render=- main_us_per_render=- \
                main_kinsn_per_render=-";
    assert_eq!(spent_words(None, at(24_000, 1), Some(3), tb), none);
    assert_eq!(spent_words(at(0, 0), at(24_000, 0), Some(0), None), none);
    assert_eq!(spent_words(at(24_000, 5), at(1_000, 9), Some(3), tb), none);
    assert_eq!(
        spent_words(at(0, 0), at(24_000, 0), Some(3), tb),
        "cpu_ms=1.0 cpu_us_per_render=333.3 kinsn_per_render=- main_us_per_render=- \
         main_kinsn_per_render=-"
    );
    assert_eq!(
        spent_words(
            main_at(0, 0, 9, 0, Some(0)),
            main_at(24_000, 30_000, 10, 900, Some(3_000)),
            Some(3),
            tb
        ),
        "cpu_ms=1.0 cpu_us_per_render=333.3 kinsn_per_render=10.0 main_us_per_render=- \
         main_kinsn_per_render=-"
    );
    // The main thread's time read where its counters were not (an older macOS).
    assert_eq!(
        spent_words(
            main_at(0, 0, 9, 0, None),
            main_at(24_000, 30_000, 9, 900, Some(3_000)),
            Some(3),
            tb
        ),
        "cpu_ms=1.0 cpu_us_per_render=333.3 kinsn_per_render=10.0 main_us_per_render=0.3 \
         main_kinsn_per_render=-"
    );
    // One counter of the same main thread backwards, or its instructions flat over frames it
    // drew, reads `-` for that word alone; the process's ticks forward with its instructions
    // backwards reads `-` for every word.
    assert_eq!(
        spent_words(
            main_at(0, 0, 9, 900, Some(0)),
            main_at(24_000, 30_000, 9, 100, Some(3_000)),
            Some(3),
            tb
        ),
        "cpu_ms=1.0 cpu_us_per_render=333.3 kinsn_per_render=10.0 main_us_per_render=- \
         main_kinsn_per_render=1.0"
    );
    for (from, to) in [(3_000, 3_000), (5_000, 3_000)] {
        assert_eq!(
            spent_words(
                main_at(0, 0, 9, 0, Some(from)),
                main_at(24_000, 30_000, 9, 900, Some(to)),
                Some(3),
                tb
            ),
            "cpu_ms=1.0 cpu_us_per_render=333.3 kinsn_per_render=10.0 main_us_per_render=0.3 \
             main_kinsn_per_render=-",
            "the main thread's instructions {from} then {to}"
        );
    }
    assert_eq!(spent_words(at(0, 30_000), at(24_000, 5), Some(3), tb), none);
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
    // A title drawn past another chip the rule keeps after this one (Claude Code's mode,
    // `⏵⏵ auto`), which the hover left as it was, is counted from its own first cell; one that
    // starts further along the rule than any chip is none of it.
    let pill = text_width("⏵⏵ auto") + 2;
    assert_eq!(
        c.title_cells(&drawn(&rest, lead + pill, whole), &rest, lead),
        text_width(whole) + 1
    );
    let far = lead + TITLE_LEAD_SKIP + TITLE_GAP + 2;
    assert_eq!(c.title_cells(&drawn(&rest, far, short), &rest, lead), 0);
    // Nothing drawn, and a frame of another size, measure none.
    assert_eq!(c.title_cells(&rest, &rest, lead), 0);
    assert_eq!(c.title_cells(&rule(80), &rest, lead), 0);
    // Beside Claude Code's footer the block's other chip (`─ auto`) stands between the landed
    // chip and its title, unchanged by the hover: the title is counted from where it begins, and
    // a lean fit beside it is still cut short (slice 9c's review round: the measure read 0 of
    // every title there). NEGATIVE CONTROL (run by hand): counting from the chip's own end again
    // reads 0 here.
    let landed_lead = text_width("\u{2713} landed") + 2;
    let beside = drawn(&rest, landed_lead, "\u{2500} auto");
    let at = landed_lead + text_width("\u{2500} auto") + 1;
    let titled = |text: &str| c.title_cells(&drawn(&beside, at, text), &beside, landed_lead);
    assert_eq!(titled(whole), text_width(whole) + 1);
    assert!(
        !said(titled(whole)).contains(FINDING),
        "{}",
        said(titled(whole))
    );
    assert!(
        said(titled(short)).starts_with(FINDING),
        "{}",
        said(titled(short))
    );
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

/// `controls prefs` of Settings ▸ Harness as same-land's held run read it: cruise-n's row and its
/// state line on the list's screen, land-b's row laid out past its state line, a stopped row
/// turned below the list, and one turned off it.
const HARNESS_PAGE: &str = r#"state open=true landing=false pane=native-tab category=Harness selected=0 scroll=0
ui key="settings/list/scroll" role=Group label="" value=- action=- state=- rect=1020.0,114.0,1804.0,426.0
ui key="settings/list" role=List label="Harness" value=- action=- state=- rect=1020.0,114.0,1804.0,426.0
ui key="harness/session/1" role=Group label="cruise-n: Tab 2 · Claude Code · supervised · autopilot cruise" value=- action=- state=- rect=1038.0,332.0,684.0,44.0
ui key="harness/session/1/state" role=Status label="needs you: grant Screen Recording; carrying on" value=- action=- state=- rect=1038.0,398.0,684.0,18.0
ui key="harness/session/2" role=Group label="land-b: Tab 3 · Claude Code · supervised · autopilot land" value=- action=- state=- rect=1038.0,444.0,684.0,44.0
ui key="harness/session/3" role=Group label="same-land: Tab 7 · Claude Code · supervised · autopilot land" value=- action=- state=- rect=1038.0,500.0,684.0,44.0
ui key="harness/session/3/state" role=Status label="stopped typing: the same reply twice, no work" value=- action=- state=- rect=1038.0,566.0,684.0,18.0
ui key="harness/session/4" role=Group label="land-c: Tab 4 · Claude Code · supervised · autopilot land" value=- action=- state=- rect=1038.0,132.0,684.0,44.0 offscreen=true
ui key="harness/session/4/state" role=Status label="stopped typing: the same reply twice, no work" value=- action=- state=- rect=1038.0,198.0,684.0,18.0"#;

#[test]
fn a_held_rows_state_line_is_shown_only_on_the_lists_screen() {
    assert_eq!(
        held_row_shown(HARNESS_PAGE, "cruise-n").as_deref(),
        Some("needs you: grant Screen Recording; carrying on")
    );
    for (label, why) in [
        ("land-b", "its state line is not laid out"),
        ("same-land", "its state line is below the list"),
        ("land-c", "its row is off the screen"),
        ("land-d", "no row"),
    ] {
        assert_eq!(held_row_shown(HARNESS_PAGE, label), None, "{label}: {why}");
    }
    let turned = HARNESS_PAGE.replace(
        "rect=1038.0,566.0,684.0,18.0",
        "rect=1038.0,466.0,684.0,18.0",
    );
    assert_eq!(
        held_row_shown(&turned, "same-land").as_deref(),
        Some("stopped typing: the same reply twice, no work"),
        "turned onto the list's screen"
    );
}

/// A CODEX-ALAB CAPTION QUOTES ITS PAD, AND A CHIP OVER THE PROGRAM'S OWN LINE IS A FINDING: the
/// pad codex-alab drew is quoted beside a chip that stood down (no finding). A chip drawn on
/// either a blank pad or the program's own line is a finding, which [`faults`] reports.
#[test]
fn a_codex_alab_caption_finds_a_chip_over_its_own_pad() {
    let own = "codex-alab 0.1.0-dev.13916 \u{b7} based on OpenAI Codex 0.165.0 \u{2500}[Cruise]";
    let down = "window=0 program=codex face=land drawn=- why=not-drawn";
    let drawn = "window=0 program=codex face=land drawn=\"\u{2913} land\"";
    let said = pad_words(down, Some(own), false);
    assert!(
        said.contains(&format!("{own:?}")) && !said.contains(FINDING),
        "{said}"
    );
    assert!(said.contains("names no mode"), "13916's line: {said}");
    assert!(
        pad_words(drawn, Some(""), true).contains(FINDING),
        "a chip on a blank pad"
    );
    assert!(
        !pad_words(down, None, true).contains(FINDING),
        "no prompt row"
    );
    let covered = pad_words(drawn, Some(own), true);
    assert!(covered.contains(FINDING), "CONTROL: {covered}");
    let row = Row::new(
        Who::CodexAlab,
        Scene::Land,
        Look::Night,
        1,
        &png_name(Who::CodexAlab, Scene::Land, Look::Night, 1, Kind::Rest),
        format!("{}; {}{covered}", Scene::Land.what(), chip_words(drawn)),
    );
    assert!(
        faults(&[row])
            .iter()
            .any(|f| matches!(&f.fault, Fault::Finding(_))),
        "the finding is a fault"
    );
}

/// A BUILD THAT NAMES ITS MODE ON ITS LINE MUST NAME THE FACE: 14249's line under Land says
/// `⤓ land`, so a landed shot beside it still saying land is a [`FINDING`]; 13916's line, which
/// lights a button only, is said to name no mode and is no fault for a build before
/// [`codex_alab_proof::SAYS_ITS_MODE`]. CONTROL: the same line under a build that should say it.
#[test]
fn a_codex_alab_caption_wants_its_own_line_to_name_the_face() {
    let land = "codex-alab 0.1.0-dev.14249 \u{b7} based on OpenAI Codex 0.166.0 \u{2500}\u{2500} \
                \u{2913} land \u{2500}[Cruise]\u{2500}[Land]\u{2500}[Off]";
    let landed = land.replace("\u{2913} land", "\u{2713} landed");
    let down = |face: &str| format!("window=0 program=codex face={face} drawn=- why=not-drawn");
    assert!(!pad_words(&down("land"), Some(land), true).contains(FINDING));
    assert!(!pad_words(&down("landed"), Some(&landed), true).contains(FINDING));
    let stale = pad_words(&down("landed"), Some(land), true);
    assert!(
        stale.contains(FINDING) && stale.contains("names no landed"),
        "{stale}"
    );
    let old = "codex-alab 0.1.0-dev.13916 \u{2500}[Cruise]\u{2500}[Land]\u{2500}[Off]";
    assert!(!pad_words(&down("landed"), Some(old), false).contains(FINDING));
    assert!(
        pad_words(&down("landed"), Some(old), true).contains(FINDING),
        "CONTROL: 13916's line under a build that should name the face"
    );
}

/// Native ownership needs no host chip, including on a blank pad. Historical captures
/// continue to require their recorded evidence of the old host standing down.
#[test]
fn a_codex_alab_shot_with_no_host_footer_accepts_a_blank_pad() {
    let shot = |scene: Scene, why: &str, pad: Option<&str>| {
        let line = format!("window=0 program=codex face=land drawn=- why={why}");
        Row::new(
            Who::CodexAlab,
            scene,
            Look::Day,
            1,
            &png_name(Who::CodexAlab, scene, Look::Day, 1, Kind::Rest),
            format!(
                "{}; {}{}",
                scene.what(),
                chip_words(&line),
                pad_words(&line, pad, true)
            ),
        )
    };
    let no_chip = |r: Row| {
        faults(&[r])
            .iter()
            .any(|f| matches!(f.fault, Fault::NoChip(_)))
    };
    let own = "\u{2913} land \u{2500}[Cruise]\u{2500}[Land]\u{2500}[Off]";
    assert!(!no_chip(shot(Scene::Land, "no-footer", Some(""))));
    assert!(!no_chip(shot(Scene::Land, "no-footer", None)));
    assert!(!no_chip(shot(Scene::Land, "not-drawn", Some(own))));
    assert!(!no_chip(shot(Scene::Off, "switch-only", Some(""))));
    assert!(
        no_chip(shot(Scene::Land, "not-drawn", Some(""))),
        "CONTROL: a blank pad"
    );
    assert!(
        no_chip(shot(Scene::Land, "not-drawn", None)),
        "CONTROL: no prompt row"
    );
    assert!(
        no_chip(shot(Scene::Land, "switch-only", Some(""))),
        "CONTROL: a carried scene with the switch alone"
    );
}

/// A CAPTURE RUN LEAVES ANOTHER'S LAB UP (slice 9c's review round): a lab of the run's own name
/// whose aterm still runs is another capture run's (another aterm under `LAB_ATERM`), and the
/// run refuses rather than take it down; one whose aterm is gone was left by a run that died,
/// and is taken down. NEGATIVE CONTROL (run by hand): `still_up` answering `None` whatever the
/// pid, as the run did before, fails it.
#[test]
fn a_capture_run_leaves_another_runs_lab_up() {
    let run = crate::run::Run {
        tag: "apcap-claude-day-1x".into(),
        instance_pid: 4242,
        aterm: "/x/bin-9c-base/aterm".into(),
        ..crate::run::Run::default()
    };
    let why = still_up(&run, |pid| pid == 4242).expect("its aterm runs");
    assert!(
        why.contains("apcap-claude-day-1x") && why.contains("4242"),
        "{why}"
    );
    assert_eq!(still_up(&run, |_| false), None, "a lab whose run died");
}

/// CLAUDE CODE'S SCENES WEAR THE OWNER'S WHOLE GLASS
/// (`docs/DESIGN-harness-glass-rainbow-2026-11-05.md` §7 step 5, judged 2026-11-08): its lab
/// admits the stub's base URL as the default route, and its tabs run on the default home's own
/// claude.ai login with aterm's usage tap on the line, a roster naming that login `main`, the
/// scenes' folders trusted in that home's profile and the stub's week ([`WEEK`]) on every reply,
/// so the glass claims Claude Code's statusLine row as line E. The forever lab's own launch (an
/// API key, `CLAUDE_CONFIG_DIR`, no tap) drew no line E in any scene. agy's lab admits no route.
/// A lab that does not admit it takes no own login, and nothing is laid. NEGATIVE CONTROL (run by
/// hand): `scene_launcher` giving Claude Code the forever lab's launch, as the verb did before,
/// fails it; so does `up_opts` leaving the route seam as the environment set it.
#[test]
fn claude_codes_scenes_run_on_the_owners_login_with_the_tap_so_line_e_shows() {
    use aterm_agent::harness::{accounts, tap};
    let a = parse_args(&["--harness".to_string(), "claude".to_string()])
        .expect("its words")
        .expect("no help");
    let env = crate::lab::UpOpts::default();
    assert!(up_opts(env.clone(), Harness::Claude, Look::Day, &a).route_seam);
    assert!(!up_opts(env, Harness::Agy, Look::Night, &a).route_seam);

    let tmp = aterm_tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let lab = |seam: &str| {
        let run = Run {
            tag: "apcap-claude-day-1x".into(),
            run: root.join(format!("runs/{seam}")),
            w: root.join(format!("runs/{seam}/scratch")),
            port: "5".into(),
            proxy: "http://127.0.0.1:6".into(),
            claude_bin: "/c/claude".into(),
            aterm: "/a/target/debug/aterm".into(),
            route_seam: seam.into(),
            ..Run::default()
        };
        // As `up` lays the scratch home: the API key's helper, and the profile in `.claude`.
        let dot = run.w.join("home/.claude");
        std::fs::create_dir_all(&dot).unwrap();
        std::fs::write(dot.join("settings.json"), r#"{"apiKeyHelper": "/helper"}"#).unwrap();
        std::fs::write(
            dot.join(".claude.json"),
            r#"{"hasCompletedOnboarding": true}"#,
        )
        .unwrap();
        run
    };
    let dirs = [root.join("proj/cruise-n"), root.join("proj/land-b")];
    let now = 1_794_000_000;
    let roster_at = |run: &Run| run.w.join("cfg/aterm").join(accounts::ACCOUNTS_FILE);

    let mut refused = lab("0");
    assert!(scene_launcher(&mut refused, Harness::Claude, &dirs, now).is_err());
    assert!(
        !roster_at(&refused).exists() && !refused.w.join("headers.json").exists(),
        "a refused lab lays nothing"
    );

    let mut run = lab("1");
    let script = scene_launcher(&mut run, Harness::Claude, &dirs, now).expect("an own launch");
    let text = crate::util::scratch_text(&script);
    let value = tap::settings_value(Path::new(&run.aterm)).expect("the tap names the lab's aterm");
    assert!(
        text.contains(&format!("--settings {}", crate::util::shell_quote(&value))),
        "the tap rides the line: {text}"
    );
    assert!(
        !text.contains("CLAUDE_CONFIG_DIR") && text.contains("-u HTTP_PROXY"),
        "the default home's own login, the stub its only route: {text}"
    );
    assert_eq!(run.claude_login, crate::run::OWN_LOGIN);
    let profile = crate::pyjson::J::parse(&crate::util::scratch_text(&run.claude_json())).unwrap();
    assert!(
        profile.get("oauthAccount").is_some(),
        "the HOME's own profile is signed in"
    );
    for d in &dirs {
        let trusted = profile
            .get("projects")
            .and_then(|p| p.get(&d.to_string_lossy()))
            .and_then(|e| e.get("hasTrustDialogAccepted"))
            .is_some_and(crate::pyjson::J::truthy);
        assert!(
            trusted,
            "{} trusted in the profile Claude Code reads",
            d.display()
        );
    }
    let roster = accounts::Roster::load(&roster_at(&run)).unwrap();
    assert_eq!(
        roster.row_for_login(None).map(|row| row.label.as_str()),
        Some("main"),
        "{roster:?}"
    );
    let headers =
        crate::pyjson::J::parse(&crate::util::scratch_text(&run.w.join("headers.json"))).unwrap();
    let on200 = |k: &str| {
        headers
            .get("on200")
            .and_then(|b| b.get(&format!("anthropic-ratelimit-unified-{k}")))
            .cloned()
    };
    assert_eq!(
        on200("7d-utilization").as_ref().and_then(|j| j.as_str()),
        Some(WEEK.0)
    );
    assert_eq!(
        on200("7d-reset").and_then(|j| j.as_i64()),
        Some(now + WEEK.1)
    );
    assert_eq!(
        on200("5h-reset").and_then(|j| j.as_i64()),
        Some(now + FIVE.1)
    );
}

/// A headless lab whose `chrome` prints tab `s-1`'s Account node as its last frame drew it: none
/// before the test's first draw, then with the lead's figure alone for the first `drawn_at`
/// frames, and from it the replay's reading, with the week's projection where `projects`; tab
/// `s-2`'s line E says its projection on every look. `s-1`'s node is line E's (`Accounts. …`), or
/// with `on_line_d` line D's fallback unit's (`Account: …`), the same sentences under the account
/// fact's head. Every read carries tab `s-1`'s `autopilot` line and says the build reports
/// Account nodes (`a11y kinds=autopilot,account`) but where `unreported`, an aterm whose `chrome`
/// prints no kinds line and no Account node; the read at look `fails_at` (1-based) fails instead,
/// the client's `ERR …` alone. Tab `s-1`'s `status` says `autopilot=off` where `off`, else
/// `autopilot=cruise`.
struct LineE {
    draws: Cell<usize>,
    looks: Cell<usize>,
    drawn_at: usize,
    off: bool,
    projects: bool,
    on_line_d: bool,
    unreported: bool,
    fails_at: Option<usize>,
}

impl LineE {
    fn new(drawn_at: usize, off: bool, projects: bool) -> Self {
        Self {
            draws: Cell::new(0),
            looks: Cell::new(0),
            drawn_at,
            off,
            projects,
            on_line_d: false,
            unreported: false,
            fails_at: None,
        }
    }

    /// The `chrome` read at look `look` (1-based) fails.
    fn failing_at(self, look: usize) -> Self {
        Self {
            fails_at: Some(look),
            ..self
        }
    }

    /// Tab `s-1`'s node is line D's fallback unit, line E never claimed.
    fn on_line_d(self) -> Self {
        Self {
            on_line_d: true,
            ..self
        }
    }

    /// An aterm whose `chrome` reports no Account node (main's before §7 step 5, or a release).
    fn unreported(self) -> Self {
        Self {
            unreported: true,
            ..self
        }
    }

    /// The wait's `draw`: one more frame.
    fn draw(&self) -> Result<(), Fail> {
        self.draws.set(self.draws.get() + 1);
        Ok(())
    }
}

impl Lab for LineE {
    fn said(&self, args: &[&str]) -> String {
        if args == ["@s-1", "status"] {
            let mode = if self.off { "off" } else { "cruise" };
            return format!("sid=s-1 auth=stored autopilot={mode} program=claude\n");
        }
        if args.first() != Some(&"chrome") {
            return String::new();
        }
        self.looks.set(self.looks.get() + 1);
        if self.fails_at == Some(self.looks.get()) {
            return "ERR the main thread did not answer within 5 s\n".to_string();
        }
        let autopilot = "autopilot sid=s-1 window=0 program=claude face=cruise drawn=- \
                         why=not-drawn spoken=- description=- title=-\na11y window=0 autopilot \
                         sid=s-1 role=Button label=\"Autopilot: cruise.\" description=-\n";
        if self.unreported {
            return autopilot.to_string();
        }
        let lead = "main, this tab, 80 percent of its week used";
        let read = format!("{lead}, resets Tuesday 16:06.");
        let projected = format!("{read} {LINE_E_PROJECTED}: main");
        let other = format!(
            "{autopilot}a11y kinds=autopilot,account\na11y window=0 account sid=s-2 role=Button \
             label={:?} description=-\n",
            format!("{LINE_E_HEAD}{projected}")
        );
        let Some(frame) = self.draws.get().checked_sub(1) else {
            return other;
        };
        let said = match (frame >= self.drawn_at, self.projects) {
            (false, _) => lead,
            (true, false) => &read,
            (true, true) => &projected,
        };
        let label = if self.on_line_d {
            format!("Account: {said}")
        } else {
            format!("{LINE_E_HEAD}{said}")
        };
        format!("{other}a11y window=0 account sid=s-1 role=Button label={label:?} description=-\n")
    }

    fn images(&self) -> PathBuf {
        PathBuf::new()
    }

    fn aterm_pid(&self) -> Option<i32> {
        None
    }
}

/// The wait's finding where it found one, else a panic naming what it returned.
fn finding_of(lab: &LineE) -> String {
    match line_e_unsaid(lab, "s-1", Duration::ZERO, || lab.draw()) {
        Ok(Some(Unsaid::Finding(why))) => why,
        other => panic!("no finding: {other:?}"),
    }
}

/// A CLAUDE CODE SCENE IS SHOT ONCE LINE E SAYS WHAT ITS PANE ALLOWS
/// (`docs/DESIGN-harness-glass-rainbow-2026-11-05.md` §7 step 5, judged 2026-11-08): the scenes
/// that settle seconds after their first reply (cruise-needs, off) showed line E before the
/// outlook replayed (the lead alone, or `┄` ahead with the reset), so the shot waits for the tab's
/// own line E node (`Accounts. …`), as `chrome` reads it, to say the week's chain, and at its hang
/// detector says what the node said last; another tab's projection is not this tab's. The node is
/// the last frame's and a headless window draws none of its own, so every look follows a frame
/// the wait draws (the first wait drew none between its looks and saw no node for 60 s in every
/// scene). Under Autopilot Off the pane's recovery refuses forecasts (§5.1), so there the shot
/// waits for the replay's reading alone and calls a projection a finding. NEGATIVE CONTROLS (run
/// by hand): `line_e_unsaid` answering at its first look, as the shots did before, fails it; so
/// does a wait that draws only between its looks, and `line_e_due` always due the projection, as
/// the first wait was, which held every Off scene to its hang.
#[test]
fn a_claude_scene_is_shot_once_line_e_says_what_its_pane_allows() {
    let lab = LineE::new(3, false, true);
    let waited = line_e_unsaid(&lab, "s-1", LINE_E_HANG, || lab.draw());
    assert_eq!(waited.ok(), Some(None), "drawn at the fourth frame");
    assert_eq!(
        (lab.draws.get(), lab.looks.get()),
        (4, 4),
        "each look reads a frame drawn for it"
    );
    let never = finding_of(&LineE::new(usize::MAX, false, true));
    assert!(
        never.starts_with("line E drew no projection within 0 s")
            && never.contains("label=\"Accounts. main, this tab, 80 percent")
            && !never.contains(LINE_E_PROJECTED),
        "{never}"
    );
    let unprojected = finding_of(&LineE::new(0, false, false));
    assert!(
        unprojected.starts_with("line E drew no projection"),
        "{unprojected}"
    );
    let off = LineE::new(2, true, false);
    let read = line_e_unsaid(&off, "s-1", LINE_E_HANG, || off.draw());
    assert_eq!(read.ok(), Some(None), "Off's reading at the third frame");
    assert_eq!((off.draws.get(), off.looks.get()), (3, 3));
    let unread = finding_of(&LineE::new(usize::MAX, true, false));
    assert!(
        unread.starts_with("line E drew no reading of its week within 0 s"),
        "{unread}"
    );
    let projected = finding_of(&LineE::new(0, true, true));
    assert!(
        projected.starts_with("line E drew a projection under Autopilot Off"),
        "{projected}"
    );
    assert_eq!(line_e_due("sid=s-1 autopilot=land"), LINE_E_PROJECTED);
    assert_eq!(line_e_due("sid=s-1 autopilot=off"), LINE_E_READ);
    assert_eq!(
        line_e_node(
            "a11y window=0 autopilot sid=s-1 role=Button label=\"Accounts. x\" description=-",
            "s-1"
        ),
        None,
        "an Autopilot node is no Account node"
    );
}

/// LINE D'S ACCOUNT UNIT IS NOT LINE E (§7 step 5, after review 2026-11-08): where line E is never
/// claimed (Claude Code's statusLine row refused, or the tap gone), line D draws its fallback
/// account unit about 10 s on, and its Account node says the quota view's sentences under the
/// account fact's head (`Account: main, this tab, …, resets …. Next 7 days …`). The wait reads
/// line E's node alone (`Accounts. …`), so that unit, projection and all, ends in the hang's
/// finding, which names the unit that stood instead, as it does under Off. NEGATIVE CONTROL (run
/// by hand): `line_e_node` taking any Account node, as `account_node` did, answers at the first
/// look and fails it.
#[test]
fn line_ds_account_unit_is_not_line_e() {
    let d = LineE::new(0, false, true).on_line_d();
    let why = finding_of(&d);
    assert!(
        why.starts_with("line E drew no projection within 0 s")
            && why.contains("(its Account node: -; line D's account unit stood instead: ")
            && why.contains("label=\"Account: main, this tab, 80 percent"),
        "{why}"
    );
    let off = finding_of(&LineE::new(0, true, false).on_line_d());
    assert!(
        off.starts_with("line E drew no reading of its week within 0 s"),
        "{off}"
    );
}

/// AN ATERM THAT REPORTS NO ACCOUNT NODE LEAVES LINE E UNJUDGED (§7 step 5, after review
/// 2026-11-08): a release build's `chrome`, or main's from before step 5 under `LAB_ATERM`, prints
/// no `a11y kinds=` line naming `account`, so no read can tell what line E drew. The scene is shot
/// at its first look that answered for the tab, with no wait, and its caption says so plainly,
/// which `faults` does not count; a finding it does. A read that failed (`ERR …`, after recheck
/// 2026-11-09) says nothing of the build: the wait looks again, so one failed read on this
/// branch's own build neither shoots early nor calls the build old. NEGATIVE CONTROLS (run by
/// hand): the wait not asking `reports_account_nodes` runs its whole hang and calls it a finding;
/// `Unsaid::noted` writing `NotJudged` as a FINDING fails it; the wait not asking `answered_for`
/// calls the failed read not judged.
#[test]
fn an_aterm_that_reports_no_account_node_leaves_line_e_unjudged() {
    let old = LineE::new(0, false, true).unreported();
    let started = Instant::now();
    let waited = line_e_unsaid(&old, "s-1", LINE_E_HANG, || old.draw());
    let Ok(Some(Unsaid::NotJudged(why))) = waited else {
        panic!("judged: {waited:?}");
    };
    assert!(why.contains("reports no Account node"), "{why}");
    assert_eq!(
        (old.draws.get(), old.looks.get()),
        (1, 1),
        "one look, no wait ({:?})",
        started.elapsed()
    );
    let blip = LineE::new(3, false, true).failing_at(1);
    let waited = line_e_unsaid(&blip, "s-1", LINE_E_HANG, || blip.draw());
    assert_eq!(waited.ok(), Some(None), "a failed read is looked past");
    assert_eq!((blip.draws.get(), blip.looks.get()), (4, 4));
    let old_blip = LineE::new(0, false, true).unreported().failing_at(1);
    let waited = line_e_unsaid(&old_blip, "s-1", LINE_E_HANG, || old_blip.draw());
    assert!(
        matches!(waited, Ok(Some(Unsaid::NotJudged(_)))),
        "{waited:?}"
    );
    assert_eq!(
        (old_blip.draws.get(), old_blip.looks.get()),
        (2, 2),
        "judged at the first read that answered"
    );
    assert!(reports_account_nodes("a11y kinds=autopilot,account\n"));
    assert!(!reports_account_nodes(
        "a11y kinds=autopilot\na11y window=3 autopilot sid=s-1 role=Button label=\"x\" \
         description=-\n"
    ));
    assert!(!answered_for(
        "ERR the main thread did not answer within 5 s\n",
        "s-1"
    ));
    assert!(!answered_for(
        "autopilot sid=s-2 window=0 program=claude\n",
        "s-1"
    ));
    assert!(answered_for(
        "autopilot sid=s-1 window=0 program=claude\n",
        "s-1"
    ));
    let row = |unsaid: &Unsaid| {
        Row::new(
            Who::Agent(Harness::Claude),
            Scene::CruiseNeeds,
            Look::Day,
            1,
            "-",
            format!("caption{}", unsaid.noted()),
        )
    };
    let unjudged = row(&Unsaid::NotJudged(why));
    assert!(unjudged.caption.contains(LINE_E_NOT_JUDGED));
    assert!(faults(&[unjudged]).is_empty());
    let found = faults(&[row(&Unsaid::Finding("line E drew no projection".into()))]);
    assert_eq!(found.len(), 1);
    assert_eq!(
        found[0].fault,
        Fault::Finding("line E drew no projection".into())
    );
}
