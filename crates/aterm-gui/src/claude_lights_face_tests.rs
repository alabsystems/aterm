// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Autopilot chip face table, title, ink, and repaint term tests.

use super::*;

use aterm_agent::harness::acts::{Act, TaskFace};

fn blank() -> RenderCell {
    RenderCell {
        ch: ' ',
        fg: [200, 200, 200],
        bg: [0, 0, 0],
        ..RenderCell::default()
    }
}

fn text(cells: &[RenderCell]) -> String {
    cells.iter().map(|c| c.ch).collect()
}

fn clock(h: u16, m: u16) -> Clock {
    Clock::of_minute(h * 60 + m)
}

const TODAY: i64 = 20_007;

fn stamp(h: u16, m: u16) -> Stamp {
    Stamp {
        clock: clock(h, m),
        day: TODAY,
    }
}

fn timer(h: u16, m: u16, day: Option<Weekday>) -> ChipGlass {
    ChipGlass {
        land_at: Some(LandAtGlass {
            clock: clock(h, m),
            other_day: day,
        }),
        ..ChipGlass::default()
    }
}

fn told(face: TaskFace) -> ChipGlass {
    ChipGlass {
        task: Some(face),
        task_at: Some(stamp(17, 42)),
        ..ChipGlass::default()
    }
}

fn told_under(face: TaskFace, h: u16, m: u16, day: Option<Weekday>) -> ChipGlass {
    ChipGlass {
        land_at: timer(h, m, day).land_at,
        ..told(face)
    }
}

struct Row {
    view: AutopilotView,
    glass: ChipGlass,
    face: ChipFace,
    drawn: &'static str,
    glyph: &'static str,
    tones: (ChipTone, ChipTone),
    whole: String,
    short: &'static str,
    spoken: String,
}

fn rows() -> Vec<Row> {
    use crate::autopilot::mode_words;
    use ChipTone::{Letter, Quiet};
    use aterm_agent::supervise::Autopilot;
    let timed = crate::autopilot::TIMED_CRUISE_CHIP;
    let landed = aterm_agent::supervise::mode_words::landed_brief(None);
    let (cruise, land, off) = (
        mode_words(Autopilot::Cruise).chip,
        mode_words(Autopilot::Land).chip,
        mode_words(Autopilot::Off).chip,
    );
    let row = |view, glass, face, drawn, glyph, tones, whole: &str, short, spoken: &str| Row {
        view,
        glass,
        face,
        drawn,
        glyph,
        tones,
        whole: whole.to_owned(),
        short,
        spoken: spoken.to_owned(),
    };
    vec![
        row(
            AutopilotView::Cruise,
            ChipGlass::default(),
            ChipFace::Cruise,
            "\u{221e}",
            "\u{221e}",
            (Letter, Letter),
            &format!(
                "autopilot: cruise \u{2014} {cruise} \u{b7} click: land \u{b7} right-click: more"
            ),
            "autopilot: cruise",
            "Autopilot: cruise.",
        ),
        row(
            AutopilotView::Cruise,
            timer(18, 0, None),
            ChipFace::CruiseUntil,
            "\u{221e} \u{2913}18:00",
            "\u{221e}",
            (Letter, Letter),
            &format!(
                "autopilot: cruise until 18:00, then land \u{2014} {timed} \u{b7} click: land now \
                 \u{b7} right-click: more"
            ),
            "autopilot: cruise until 18:00, then land",
            "Autopilot: cruise. Lands at 18:00.",
        ),
        row(
            AutopilotView::Cruise,
            timer(16, 0, Some(Weekday::Sat)),
            ChipFace::CruiseUntil,
            "\u{221e} \u{2913}16:00",
            "\u{221e}",
            (Letter, Letter),
            &format!(
                "autopilot: cruise until Sat 16:00, then land \u{2014} {timed} \u{b7} click: land \
                 now \u{b7} right-click: more"
            ),
            "autopilot: cruise until Sat 16:00, then land",
            "Autopilot: cruise. Lands Saturday at 16:00.",
        ),
        row(
            AutopilotView::Cruise,
            told(TaskFace::Landed),
            ChipFace::Cruise,
            "\u{221e}",
            "\u{221e}",
            (Letter, Letter),
            &format!(
                "autopilot: cruise \u{2014} {cruise} \u{b7} click: land \u{b7} right-click: more"
            ),
            "autopilot: cruise",
            "Autopilot: cruise.",
        ),
        row(
            AutopilotView::Cruise,
            told_under(TaskFace::Landed, 16, 0, Some(Weekday::Sat)),
            ChipFace::CruiseUntil,
            "\u{221e} \u{2913}16:00",
            "\u{221e}",
            (Letter, Letter),
            &format!(
                "autopilot: cruise until Sat 16:00, then land \u{2014} {timed} \u{b7} click: land \
                 now \u{b7} right-click: more"
            ),
            "autopilot: cruise until Sat 16:00, then land",
            "Autopilot: cruise. Lands Saturday at 16:00.",
        ),
        row(
            AutopilotView::Cruise,
            told(TaskFace::Theirs),
            ChipFace::Cruise,
            "\u{221e}",
            "\u{221e}",
            (Letter, Letter),
            &format!(
                "autopilot: cruise \u{2014} {cruise} \u{b7} click: land \u{b7} right-click: more"
            ),
            "autopilot: cruise",
            "Autopilot: cruise.",
        ),
        row(
            AutopilotView::Cruise,
            told_under(TaskFace::Theirs, 18, 0, None),
            ChipFace::CruiseUntil,
            "\u{221e} \u{2913}18:00",
            "\u{221e}",
            (Letter, Letter),
            &format!(
                "autopilot: cruise until 18:00, then land \u{2014} {timed} \u{b7} click: land now \
                 \u{b7} right-click: more"
            ),
            "autopilot: cruise until 18:00, then land",
            "Autopilot: cruise. Lands at 18:00.",
        ),
        row(
            AutopilotView::Land,
            ChipGlass::default(),
            ChipFace::Land,
            "\u{2913}",
            "\u{2913}",
            (Letter, Letter),
            &format!("autopilot: land \u{2014} {land} \u{b7} click: off \u{b7} right-click: more"),
            "autopilot: land",
            "Autopilot: land.",
        ),
        row(
            AutopilotView::Land,
            told(TaskFace::Open),
            ChipFace::Land,
            "\u{2913}",
            "\u{2913}",
            (Letter, Letter),
            &format!("autopilot: land \u{2014} {land} \u{b7} click: off \u{b7} right-click: more"),
            "autopilot: land",
            "Autopilot: land.",
        ),
        row(
            AutopilotView::Land,
            told(TaskFace::Landed),
            ChipFace::Landed,
            "\u{2713} landed",
            "\u{2713}",
            (Letter, Letter),
            &format!(
                "autopilot: land, landed 17:42 \u{2014} {landed} \u{b7} click: off \u{b7} \
                 right-click: more"
            ),
            "autopilot: land, landed 17:42",
            "Autopilot: land. Landed at 17:42.",
        ),
        row(
            AutopilotView::Land,
            told(TaskFace::Theirs),
            ChipFace::Theirs,
            "\u{2913} quiet",
            "\u{2913}",
            (Quiet, Quiet),
            "autopilot: land, quiet \u{2014} a person's own exchange; the harness types nothing \
             until someone else's turn ends \u{b7} click: off \u{b7} right-click: more",
            "autopilot: land, quiet",
            "Autopilot: land. Quiet: a person is driving the session.",
        ),
        row(
            AutopilotView::Land,
            told(TaskFace::Failed),
            ChipFace::Failed,
            "\u{2715} failed",
            "\u{2715}",
            (Quiet, Quiet),
            "autopilot: land, failed \u{2014} the worker could not finish; the harness types \
             nothing until someone else's turn \u{b7} click: off \u{b7} right-click: more",
            "autopilot: land, failed",
            "Autopilot: land. Failed: the worker could not finish.",
        ),
        row(
            AutopilotView::Cruise,
            told(TaskFace::Failed),
            ChipFace::Cruise,
            "\u{221e}",
            "\u{221e}",
            (Letter, Letter),
            &format!(
                "autopilot: cruise \u{2014} {cruise} \u{b7} click: land \u{b7} right-click: more"
            ),
            "autopilot: cruise",
            "Autopilot: cruise.",
        ),
        row(
            AutopilotView::Off,
            ChipGlass::default(),
            ChipFace::Off,
            "\u{25cb}",
            "\u{25cb}",
            (Quiet, Quiet),
            &format!("autopilot: off \u{2014} {off} \u{b7} click: cruise \u{b7} right-click: more"),
            "autopilot: off",
            "Autopilot: off.",
        ),
        row(
            AutopilotView::MasterOff,
            ChipGlass::default(),
            ChipFace::MasterOff,
            "\u{2298} off",
            "\u{2298}",
            (Quiet, Quiet),
            "harness off in Settings \u{b7} click: turn the harness on \u{b7} right-click: more",
            "harness off in Settings",
            "Autopilot: off. The harness is off in Settings.",
        ),
        row(
            AutopilotView::AgentOff(aterm_phase::Program::Claude),
            ChipGlass::default(),
            ChipFace::AgentOff(aterm_phase::Program::Claude),
            "\u{2298} off",
            "\u{2298}",
            (Quiet, Quiet),
            "Supervise Claude Code sessions is off \u{b7} click: turn the harness on \u{b7} \
             right-click: more",
            "Supervise Claude Code sessions is off",
            "Autopilot: off. Supervise Claude Code sessions is off in Settings.",
        ),
        row(
            AutopilotView::Unhosted,
            ChipGlass::default(),
            ChipFace::Unhosted,
            "\u{2298} off",
            "\u{2298}",
            (Quiet, Quiet),
            "no harness: this aterm has no control socket \u{b7} click: open Settings \u{25b8} \
             Harness \u{b7} right-click: more",
            "no harness: this aterm has no control socket",
            "Autopilot: off. This aterm has no control socket.",
        ),
    ]
}

/// A tone's ink on [`blank`] for the Autopilot chip, the block's first, under Cruise.
fn ink(tone: ChipTone) -> [u8; 3] {
    let at = ChipAt::cruise(blank());
    match tone {
        ChipTone::Letter => at.letter_at(0),
        ChipTone::Quiet => at.quiet(),
    }
}

fn lights(view: AutopilotView, glass: ChipGlass) -> WindowLights {
    let mut w = WindowLights::default();
    w.note_autopilot_at(7, view.mode(), view, glass, Instant::now);
    w
}

/// A LANDING'S WORDS SAY WHAT ATERM CHECKED (`docs/DESIGN-harness-autopilot-2026-10-15.md`
/// §14.9, slice 9c): on every leg, the chip's screen-reader description and the `Autopilot landed`
/// record's first line say the receipt's [`aterm_agent::supervise::mode_words::checked_words`]
/// whole, the hovered `✓ landed` title its brief (`checked_brief`: on main the head, the ref and
/// the folder) without the agent's words, the record's second line what the agent said, a quote
/// as long as a receipt keeps cut before its closing mark; "remote main" on none of them (git
/// names `origin/main`). With no receipt (codex-alab's own landing, one a loop before the
/// receipt noted) each says aterm has no record of checking it, and names no head. Where the
/// receipt says the land check was answered (§14.17 item 5, slice 13a), the description and the
/// record's second line say so before what the agent said. The chip at rest stays `✓ landed`.
/// NEGATIVE CONTROLS (mutants, run by hand): 9a's fixed sentence on the title, the record leaving
/// out its receipt, the record's quote left whole, and the agent's words read without the
/// receipt's `check` each fail it.
#[test]
fn the_landed_words_say_what_aterm_checked() {
    use aterm_agent::harness::acts::{LandReceipt, ReceiptLeg};
    use aterm_agent::supervise::mode_words::{
        ANSWERED_CHECK, NO_RECORD, checked_brief, checked_words, landed_brief, landed_words,
    };
    let said = "Nothing left: pushed to origin/main.";
    let receipt = |leg: ReceiptLeg| {
        let named = !matches!(leg, ReceiptLeg::NoRepo | ReceiptLeg::Gone);
        let on = leg == ReceiptLeg::OnMain;
        LandReceipt {
            head: if named { "3f9a1c2e4b7d" } else { "" }.to_owned(),
            main_ref: if on { "origin/main" } else { "" }.to_owned(),
            repo: if leg == ReceiptLeg::NoRepo {
                ""
            } else {
                "/Users//me/src/proj"
            }
            .to_owned(),
            branch: if on { "main" } else { "" }.to_owned(),
            leg,
            check: false,
            said: said.to_owned(),
            at_ms: 0,
            statement: aterm_agent::harness::land_receipt::Statement::Unread,
        }
    };
    let surfaces = |r: Option<&LandReceipt>| {
        let words = TitleWords {
            receipt: r,
            ..TitleWords::default()
        };
        let glass = told(TaskFace::Landed);
        let (whole, short) = autopilot_title(AutopilotView::Land, &glass, "", Some(TODAY), words);
        let face = ChipFace::of(AutopilotView::Land, &glass);
        let spoken =
            autopilot_description(face, &glass, words, Some(TODAY), None).unwrap_or_default();
        let record =
            crate::harness_task::task_record("s-1", "in tab 3", "in tab 3", TaskFace::Landed, r)
                .expect("a landing's record");
        (whole, short, spoken, record.detail)
    };
    for leg in ReceiptLeg::ALL {
        let r = receipt(leg);
        let checked = checked_words(Some(&r));
        let (whole, short, spoken, detail) = surfaces(Some(&r));
        assert!(
            whole.contains(&landed_brief(Some(&r))) && whole.contains(&checked_brief(Some(&r))),
            "{leg:?} title: {whole}"
        );
        assert!(
            spoken.contains(&landed_words(Some(&r))) && spoken.contains(&checked),
            "{leg:?} description: {spoken}"
        );
        assert_eq!(detail[0], format!("Tab 3: {checked}"), "{leg:?}");
        assert_eq!(
            detail.get(1).map(String::as_str),
            Some(format!("It said \u{201c}{said}\u{201d}").as_str()),
            "{leg:?}"
        );
        assert!(!short.contains(&checked), "{leg:?} at rest: {short}");
        assert!(
            !whole.contains(said),
            "{leg:?} title quotes the agent: {whole}"
        );
        for text in [&whole, &spoken, &detail[0]] {
            assert!(!text.contains("remote main"), "{leg:?}: {text}");
        }
    }
    // The land check answered (§14.17 item 5, slice 13a): the description and the record's second
    // line say so before what the agent said; the title's brief, which quotes no agent, is the
    // unchecked one's.
    let unchecked = receipt(ReceiptLeg::OnMain);
    let checked = LandReceipt {
        check: true,
        ..unchecked.clone()
    };
    let (whole, _, spoken, detail) = surfaces(Some(&checked));
    assert!(spoken.contains(ANSWERED_CHECK), "{spoken}");
    let second = detail
        .get(1)
        .map(|line| line.to_lowercase())
        .unwrap_or_default();
    assert!(
        second.starts_with(ANSWERED_CHECK)
            && second.ends_with(&format!("\u{201c}{}\u{201d}", said.to_lowercase())),
        "{detail:?}"
    );
    assert_eq!(whole, surfaces(Some(&unchecked)).0);
    let (_, _, spoken, detail) = surfaces(Some(&unchecked));
    assert!(
        !spoken.contains(ANSWERED_CHECK)
            && !detail.join(" ").to_lowercase().contains(ANSWERED_CHECK)
    );
    let (whole, _, spoken, detail) = surfaces(None);
    for text in [&whole, &spoken, &detail[0]] {
        assert!(text.contains(NO_RECORD), "{text}");
        assert!(!text.contains("3f9a1c2e4b7d"), "{text}");
    }
    assert_eq!(detail.len(), 1, "{detail:?}");
    // A quote as long as a receipt keeps: the record's line is cut to its cap before the quote's
    // closing mark, never past it.
    let long = LandReceipt {
        said: format!(
            "Nothing left: {}",
            "x".repeat(aterm_agent::harness::land_receipt::SAID_BYTES)
        ),
        ..receipt(ReceiptLeg::OnMain)
    };
    let record = crate::harness_task::task_record(
        "s-1",
        "in tab 3",
        "in tab 3",
        TaskFace::Landed,
        Some(&long),
    )
    .expect("a landing's record");
    let line = &record.detail[1];
    assert!(
        line.chars().count() <= aterm_messages::DETAIL_LINE_CAP
            && line.starts_with("It said \u{201c}Nothing left: x")
            && line.ends_with("x\u{2026}\u{201d}")
            && line.matches('\u{2026}').count() == 1,
        "{line}"
    );
}

/// THE HOVERED LANDED TITLE KEEPS WHAT ATERM CHECKED BESIDE THE AGENT'S FOOTER (§14.9, slice 9c;
/// its review round): with the agent's words as long as a receipt keeps them (`SAID_BYTES`), the
/// `✓ landed` chip's title, fitted into the rule beside Claude Code's footer at 300 and 400
/// columns, still draws `aterm checked <head> on origin/main in <folder>`, and it shows from no
/// wider a window than 9a's fixed sentence did beside the same footer: the title says what aterm
/// checked in brief and leaves the agent's words to the state line, the tips, the record and the
/// description. NEGATIVE CONTROL (run by hand): a brief that quotes the agent (the build before
/// the review round) draws `autopilot: land, landed 17:42 · click: off · right-click: more` at
/// both widths and fails it.
#[test]
fn the_hovered_landed_title_keeps_what_aterm_checked_beside_the_footer() {
    use aterm_agent::harness::acts::{LandReceipt, ReceiptLeg};
    use aterm_agent::harness::footer::{self, FooterFacts, LightsWidth, TitleFit};
    use aterm_agent::supervise::mode_words::{checked_brief, landed_brief};
    let facts = FooterFacts {
        model: Some("Opus 5.5".into()),
        effort: Some("xhigh".into()),
        path: Some("~/aterm".into()),
        branch: Some("main".into()),
        ..FooterFacts::default()
    };
    let receipt = LandReceipt {
        head: "3f9a1c2e4b7d".into(),
        main_ref: "origin/main".into(),
        repo: "/Users//me/src/proj-api".into(),
        branch: "main".into(),
        leg: ReceiptLeg::OnMain,
        check: false,
        said: format!(
            "Nothing left: {}",
            "x".repeat(aterm_agent::harness::land_receipt::SAID_BYTES - 14)
        ),
        at_ms: 0,
        statement: aterm_agent::harness::land_receipt::Statement::Unread,
    };
    let glass = told(TaskFace::Landed);
    let words = TitleWords {
        receipt: Some(&receipt),
        ..TitleWords::default()
    };
    let (whole, short) = autopilot_title(
        AutopilotView::Land,
        &glass,
        "\u{2913} landing 17:42",
        Some(TODAY),
        words,
    );
    // 9a's title: the same parts around its fixed sentence.
    let nine_a = whole.replace(
        &landed_brief(Some(&receipt)),
        "it said nothing is left, and no repository check found work left to land; the harness \
         types nothing more until someone else's turn",
    );
    assert_ne!(nine_a, whole);
    let now = Instant::now();
    let fitted = |whole: &str| {
        let w = lights(AutopilotView::Land, glass);
        let mut block = Block::default();
        assert!(w.fit_into(
            7,
            blank(),
            now,
            ChipAt::cruise(blank()),
            None,
            Some(Chip::Autopilot),
            (400, true),
            (Some((whole, short.as_str())), None, None),
            &mut block,
        ));
        let widths = LightsWidth {
            chips: block.holds,
            title: block.title.len(),
            lean: block.lean.each_ref().map(Vec::len),
            short: block.short_title.len(),
            reason: block.reason,
        };
        (block, widths)
    };
    let drawn_at = |(block, widths): &(Block, LightsWidth), cols: usize| {
        let fit = footer::fit_rule(&facts, footer::rule_room(&(0..cols)), Some(*widths));
        match fit.title {
            TitleFit::Full => text(&block.title),
            TitleFit::Lean(n) => text(&block.lean[usize::from(n)]),
            TitleFit::Short => text(&block.short_title),
            TitleFit::None => String::new(),
        }
    };
    let brief = checked_brief(Some(&receipt));
    let ours = fitted(&whole);
    for cols in [300, 400] {
        let drawn = drawn_at(&ours, cols);
        assert!(drawn.contains(&brief), "{cols} columns drew {drawn:?}");
        assert!(drawn.contains("aterm checked 3f9a1c2e4b7d"), "{drawn:?}");
    }
    let main = fitted(&nine_a);
    let explained_from = |fitted: &(Block, LightsWidth), says: &str| {
        (40..=600).find(|&cols| drawn_at(fitted, cols).contains(says))
    };
    let from = explained_from(&ours, &brief).expect("the brief is drawn at some width");
    let main_from = explained_from(&main, "no repository check found")
        .expect("9a's sentence is drawn at some width");
    eprintln!("what aterm checked shows from {from} columns; 9a's sentence from {main_from}");
    assert!(from <= main_from, "{from} > 9a's {main_from}");
}

#[test]
fn the_face_table_gives_every_state_its_face_ink_titles_and_words() {
    let now = Instant::now();
    let mut seen = std::collections::BTreeSet::new();
    for row in rows() {
        let what = format!("{:?} {:?}", row.view, row.glass);
        assert_eq!(ChipFace::of(row.view, &row.glass), row.face, "{what}");
        assert_eq!(row.face.rung(), row.glyph, "{what}: the glyph-only rung");
        seen.insert(format!("{:?}", row.face));
        assert_eq!(row.face.tones(), row.tones, "{what}");
        assert_eq!(
            autopilot_title(row.view, &row.glass, "", Some(TODAY), TitleWords::default()),
            (row.whole.clone(), row.short.to_owned()),
            "{what}"
        );
        assert_eq!(
            row.face.spoken(&row.glass, Some(TODAY)),
            row.spoken,
            "{what}"
        );
        if row.view == AutopilotView::Unhosted {
            let w = lights(row.view, row.glass);
            assert!(
                w.block(7, blank(), now, ChipAt::cruise(blank()), Keep::Drawn)
                    .lights
                    .is_empty()
            );
            continue;
        }
        let w = lights(row.view, row.glass);
        let block = w.block(7, blank(), now, ChipAt::cruise(blank()), Keep::Drawn);
        let n = row.drawn.chars().count();
        assert_eq!(text(&block.lights), row.drawn, "{what}");
        assert_eq!(block.cols[0], (0..n, Chip::Autopilot), "{what}");
        let glyph = row.face.glyph().chars().count();
        let (glyph_ink, word_ink) = (ink(row.tones.0), ink(row.tones.1));
        assert!(
            block.lights[..glyph].iter().all(|c| c.fg == glyph_ink),
            "{what}: the glyph's ink"
        );
        assert!(
            block.lights[glyph..n].iter().all(|c| c.fg == word_ink),
            "{what}: the word's ink"
        );
        {
            let s = &w.sessions[&7];
            let on = drawn_at(s, now, None, None, Keep::Drawn);
            assert_eq!(
                chips_width_at(
                    s,
                    Chip::row().filter(|chip| on(*chip)),
                    Keep::Drawn,
                    Inks::of(blank(), ChipAt::cruise(blank())),
                    None,
                ),
                block.lights.len(),
                "{what}"
            );
        }
        let mut w = w;
        w.hover = Some((7, Target::from(Chip::Autopilot)));
        let block = w.block(7, blank(), now, ChipAt::cruise(blank()), Keep::Drawn);
        assert_eq!(text(&block.title), format!("{}  ", row.whole), "{what}");
        assert_eq!(
            text(&block.short_title),
            format!("{}  ", row.short),
            "{what}"
        );
    }
    assert_eq!(
        seen.len(),
        ChipFace::ALL.len(),
        "every face is a state's: {seen:?}"
    );
    for view in [
        AutopilotView::Land,
        AutopilotView::Off,
        AutopilotView::MasterOff,
    ] {
        assert_ne!(
            ChipFace::of(view, &timer(18, 0, None)),
            ChipFace::CruiseUntil,
            "{view:?}"
        );
    }
    for view in [
        AutopilotView::Off,
        AutopilotView::MasterOff,
        AutopilotView::AgentOff(aterm_phase::Program::Claude),
    ] {
        for face in TaskFace::ALL {
            assert_eq!(
                ChipFace::of(view, &told(face)),
                ChipFace::of(view, &ChipGlass::default()),
                "{view:?} {face:?}: v1's face"
            );
        }
    }
}

/// A WORKER'S FAILED CONCLUSION UNDER LAND WEARS ITS OWN FACE: nothing is being wrapped up, so the
/// chip never says the open face's `wrapping up`, and it carries nothing.
#[test]
fn a_failed_worker_wears_its_own_face_and_never_says_wrapping_up() {
    let glass = told(TaskFace::Failed);
    let face = ChipFace::of(AutopilotView::Land, &glass);
    assert_eq!(face, ChipFace::Failed);
    assert!(!face.carries() && face.standing());
    let (whole, short) = autopilot_title(
        AutopilotView::Land,
        &glass,
        "",
        Some(TODAY),
        TitleWords::default(),
    );
    assert!(!whole.contains("wrapping up"), "{whole}");
    assert!(whole.contains("could not finish"), "{whole}");
    assert_eq!(short, "autopilot: land, failed");
    let (open, _) = autopilot_title(
        AutopilotView::Land,
        &told(TaskFace::Open),
        "",
        Some(TODAY),
        TitleWords::default(),
    );
    assert!(open.contains("wrapping up"), "the control: {open}");
}

#[test]
fn under_cruise_no_task_face_shows_and_land_wears_its_own() {
    let faces = std::iter::once(None).chain(TaskFace::ALL.into_iter().map(Some));
    for task in faces {
        for armed in [false, true] {
            let glass = ChipGlass {
                task,
                task_at: Some(stamp(17, 42)),
                land_at: armed.then(|| timer(18, 0, None).land_at).flatten(),
                ..ChipGlass::default()
            };
            let cruise = if armed {
                ChipFace::CruiseUntil
            } else {
                ChipFace::Cruise
            };
            assert_eq!(
                ChipFace::of(AutopilotView::Cruise, &glass),
                cruise,
                "{task:?} armed={armed}"
            );
            let land = match task {
                Some(TaskFace::Landed) => ChipFace::Landed,
                Some(TaskFace::Theirs) => ChipFace::Theirs,
                Some(TaskFace::Failed) => ChipFace::Failed,
                Some(TaskFace::Open) | None => ChipFace::Land,
            };
            assert_eq!(
                ChipFace::of(AutopilotView::Land, &glass),
                land,
                "{task:?} armed={armed}"
            );
        }
    }
    assert_eq!(
        ChipFace::of(AutopilotView::Land, &told(TaskFace::Landed)),
        ChipFace::Landed,
        "NEGATIVE CONTROL"
    );
}

#[test]
fn every_face_clock_names_its_day_when_it_is_not_today() {
    let tomorrow = Some(TODAY + 1);
    for (view, task, word) in [(AutopilotView::Land, TaskFace::Landed, "landed")] {
        let glass = told(task);
        let face = ChipFace::of(view, &glass);
        let what = format!("{view:?} {task:?}");
        let (whole, short) = autopilot_title(view, &glass, "", tomorrow, TitleWords::default());
        assert!(
            short.ends_with(&format!("{word} Fri 17:42")),
            "{what}: {short}"
        );
        assert!(whole.starts_with(&short), "{what}: {whole}");
        let said = face.spoken(&glass, tomorrow);
        assert!(said.ends_with(" on Friday at 17:42."), "{what}: {said}");
        for today in [Some(TODAY), None] {
            let (whole, short) = autopilot_title(view, &glass, "", today, TitleWords::default());
            assert!(
                short.ends_with(&format!("{word} 17:42")),
                "{what} {today:?}: {short}"
            );
            assert!(!whole.contains("Fri"), "{what} {today:?}: {whole}");
            let said = face.spoken(&glass, today);
            assert!(
                said.ends_with(" at 17:42.") && !said.contains("Friday"),
                "{said}"
            );
        }
    }
    let quiet = told(TaskFace::Theirs);
    let (whole, short) = autopilot_title(
        AutopilotView::Land,
        &quiet,
        "",
        tomorrow,
        TitleWords::default(),
    );
    assert!(!whole.contains("Fri"), "{whole}");
    assert_eq!(short, "autopilot: land, quiet");
}

#[test]
fn each_mode_phrase_holds_no_cut_mark() {
    for row in rows() {
        let phrase = row.face.phrase(&row.glass, Some(TODAY));
        assert!(
            !phrase.contains(" \u{2014} ") && !phrase.contains(" \u{b7} "),
            "{phrase}"
        );
        assert_eq!(phrase, row.short);
        let trail = "\u{2713} approved 14:05, \u{21bb} kept going 14:02";
        for trail in ["", trail] {
            let (whole, short) = autopilot_title(
                row.view,
                &row.glass,
                trail,
                Some(TODAY),
                TitleWords::default(),
            );
            assert_eq!(short, row.short, "{whole}");
            assert_eq!(short_title(&whole), row.short, "{whole}");
            assert!(whole.ends_with(" \u{b7} right-click: more"), "{whole}");
            assert!(
                !short.contains("right-click"),
                "the short form gives the clicks up: {short}"
            );
            if !trail.is_empty() {
                let at = whole.find(" \u{b7} last: ").expect("the trail");
                let explained = row
                    .face
                    .explanation(&row.glass, TitleWords::default(), Some(TODAY))
                    .map_or(0, |e| {
                        whole.find(e.as_ref()).expect("the explanation") + e.len()
                    });
                assert!(explained <= at, "the explanation leads the trail: {whole}");
                assert!(whole.contains(&format!(" \u{b7} last: {trail} \u{b7} click: ")));
            }
            let [no_trail, words, clicks, click] =
                lean_titles(&whole).map(|(head, tail)| format!("{head}{tail}"));
            let (bare, _) =
                autopilot_title(row.view, &row.glass, "", Some(TODAY), TitleWords::default());
            if trail.is_empty() {
                assert_eq!(no_trail, "", "no trail, no form without one: {whole}");
            } else {
                assert_eq!(no_trail, bare, "the trail given up: {whole}");
                assert!(no_trail.len() < whole.len());
            }
            assert_eq!(
                Some(words.as_str()),
                bare.strip_suffix(" \u{b7} right-click: more"),
                "the menu's line given up too: {whole}"
            );
            assert!(words.starts_with(row.short) && words.len() > row.short.len());
            assert!(
                words.contains(" \u{b7} click: "),
                "the click stays: {words}"
            );
            assert_eq!(short_title(&words), row.short, "{words}");
            let explained = row
                .face
                .explanation(&row.glass, TitleWords::default(), Some(TODAY));
            let click_words = format!(" \u{b7} click: {}", row.face.click(&row.glass));
            match explained {
                None => assert_eq!(
                    (clicks.as_str(), click.as_str()),
                    ("", ""),
                    "no explanation, nothing to give up: {whole}"
                ),
                Some(e) => {
                    assert_eq!(click, format!("{}{click_words}", row.short), "{whole}");
                    let cells = |t: &str| t.chars().count();
                    if cells(&e) + 3 > cells(" \u{b7} right-click: more") {
                        assert_eq!(
                            clicks,
                            format!("{click} \u{b7} right-click: more"),
                            "{whole}"
                        );
                        assert!(cells(&clicks) < cells(&words), "{whole}");
                        assert!(cells(&click) < cells(&clicks), "{whole}");
                    } else {
                        assert_eq!(clicks, "", "no narrower than v1's words: {whole}");
                        assert!(cells(&click) < cells(&words), "{whole}");
                    }
                    assert!(cells(&click) > cells(row.short), "{whole}");
                    assert!(!click.contains(" \u{2014} "), "{click}");
                    assert_eq!(short_title(&click), row.short, "{click}");
                }
            }
        }
    }
    assert_eq!(
        lean_titles("worker of lab-sweep \u{b7} task working 4m \u{2014} click to go there"),
        [("", ""); LEAN_FORMS]
    );
    assert_eq!(lean_titles("autopilot: cruise"), [("", ""); LEAN_FORMS]);
}

#[test]
fn every_face_keeps_its_clicks_after_its_explanation() {
    use aterm_agent::harness::footer::{self, FooterFacts, LightsWidth, TitleFit};
    let now = Instant::now();
    let facts = FooterFacts {
        model: Some("Opus 5.5".into()),
        effort: Some("xhigh".into()),
        path: Some("~/aterm".into()),
        branch: Some("main".into()),
        ..FooterFacts::default()
    };
    let whole_facts = footer::fit_rule(&facts, 400, None).segments;
    let hovered = |row: &Row, trail: &str| {
        let (whole, short) = autopilot_title(
            row.view,
            &row.glass,
            trail,
            Some(TODAY),
            TitleWords::default(),
        );
        let w = lights(row.view, row.glass);
        let mut block = Block::default();
        assert!(w.fit_into(
            7,
            blank(),
            now,
            ChipAt::cruise(blank()),
            None,
            Some(Chip::Autopilot),
            (400, true),
            (Some((whole.as_str(), short.as_str())), None, None),
            &mut block,
        ));
        let widths = LightsWidth {
            chips: block.holds,
            title: block.title.len(),
            lean: block.lean.each_ref().map(Vec::len),
            short: block.short_title.len(),
            reason: block.reason,
        };
        (block, widths)
    };
    let laid = |block: &Block, title: TitleFit| match title {
        TitleFit::Full => text(&block.title),
        TitleFit::Lean(n) => text(&block.lean[usize::from(n)]),
        TitleFit::Short => text(&block.short_title),
        TitleFit::None => String::new(),
    };
    let walk = |widths: LightsWidth| {
        let mut met: Vec<(TitleFit, usize)> = Vec::new();
        for room in (0..=400).rev() {
            let fit = footer::fit_rule(&facts, room, Some(widths));
            if !fit.chips {
                break;
            }
            match met.last_mut() {
                Some((title, narrowest)) if *title == fit.title => *narrowest = room,
                _ => met.push((fit.title, room)),
            }
            if fit.title != TitleFit::None {
                assert_eq!(fit.segments, whole_facts, "{room}: the facts stay whole");
            }
        }
        met
    };
    for row in rows() {
        if row.view == AutopilotView::Unhosted {
            continue;
        }
        let what = format!("{:?} {:?}", row.view, row.glass);
        for trail in ["", "\u{2713} approved 14:05"] {
            let (block, widths) = hovered(&row, trail);
            let explained = row
                .face
                .explanation(&row.glass, TitleWords::default(), Some(TODAY))
                .is_some();
            let both = widths.lean[2] > 0;
            let mut want = vec![TitleFit::Full];
            if !trail.is_empty() {
                want.push(TitleFit::Lean(0));
            }
            want.push(TitleFit::Lean(1));
            if both {
                want.push(TitleFit::Lean(2));
            }
            if explained {
                want.push(TitleFit::Lean(3));
            }
            want.extend([TitleFit::Short, TitleFit::None]);
            let met = walk(widths);
            assert_eq!(
                met.iter().map(|(t, _)| *t).collect::<Vec<_>>(),
                want,
                "{what} {trail:?}: the forms in order"
            );
            for &(title, narrowest) in &met {
                if title == TitleFit::None {
                    continue;
                }
                let fit = footer::fit_rule(&facts, narrowest, Some(widths));
                assert_eq!(
                    fit.width(Some(widths)),
                    narrowest,
                    "{what} {trail:?}: {title:?} stands down to its own width"
                );
                let drawn = laid(&block, title);
                assert_eq!(
                    drawn.contains(" \u{b7} click: "),
                    title != TitleFit::Short,
                    "{what} {trail:?}: {title:?} keeps the click: {drawn:?}"
                );
                if title == TitleFit::Lean(2) {
                    assert!(drawn.ends_with("right-click: more  "), "{drawn:?}");
                    assert!(!drawn.contains(" \u{2014} "), "{drawn:?}");
                }
            }
            let main = LightsWidth {
                lean: [widths.lean[0], widths.lean[1], 0, 0],
                ..widths
            };
            let before: Vec<TitleFit> = walk(main).into_iter().map(|(t, _)| t).collect();
            assert!(
                !before.contains(&TitleFit::Lean(2)) && !before.contains(&TitleFit::Lean(3)),
                "CONTROL {what}: {before:?}"
            );
        }
    }
    let landed = rows()
        .into_iter()
        .find(|r| r.face == ChipFace::Landed && r.glass.land_at.is_none())
        .expect("the landed face");
    let (block, widths) = hovered(&landed, "");
    let rule = |cols: usize, widths: LightsWidth| {
        let fit = footer::fit_rule(&facts, footer::rule_room(&(0..cols)), Some(widths));
        laid(&block, fit.title)
    };
    assert_eq!(
        rule(160, widths),
        "autopilot: land, landed 17:42 \u{b7} click: off \u{b7} right-click: more  "
    );
    assert_eq!(rule(120, widths), rule(160, widths), "both clicks at 120");
    assert_eq!(
        rule(110, widths),
        "autopilot: land, landed 17:42 \u{b7} click: off  ",
        "the click at 110 columns"
    );
    assert_eq!(
        rule(90, widths),
        "autopilot: land, landed 17:42  ",
        "the phrase alone at 90"
    );
    let from = |title: TitleFit| {
        (40..=400)
            .filter(|cols| {
                footer::fit_rule(&facts, footer::rule_room(&(0..*cols)), Some(widths)).title
                    == title
            })
            .min()
    };
    eprintln!(
        "the landed face: whole from {:?} columns, v1's words from {:?}, both clicks from {:?}, \
         the click from {:?}, the phrase from {:?}",
        from(TitleFit::Full),
        from(TitleFit::Lean(1)),
        from(TitleFit::Lean(2)),
        from(TitleFit::Lean(3)),
        from(TitleFit::Short)
    );
    let main = LightsWidth {
        lean: [widths.lean[0], widths.lean[1], 0, 0],
        ..widths
    };
    assert_eq!(
        rule(160, main),
        "autopilot: land, landed 17:42  ",
        "CONTROL: main's ladder gives the clicks up"
    );
}

#[test]
fn each_spoken_label_holds_no_glyph() {
    let words = |text: &str| {
        text.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '.' | ':' | ','))
    };
    for row in rows() {
        assert!(words(&row.spoken), "{}", row.spoken);
        assert!(row.spoken.starts_with("Autopilot: "), "{}", row.spoken);
    }
    for act in Act::ALL {
        for day in [None, Some(Weekday::Thu)] {
            let said = spoken_last(None, act, clock(14, 5), day);
            assert!(words(&said), "{said}");
            assert!(said.ends_with(" at 14:05."), "{said}");
        }
    }
    assert_eq!(
        spoken_last(None, Act::Approved, clock(14, 5), None),
        "Last: approved at 14:05."
    );
    assert_eq!(
        spoken_last(None, Act::Approved, clock(23, 58), Some(Weekday::Thu)),
        "Last: approved on Thursday at 23:58."
    );
    let unplaced = ChipGlass {
        task: Some(TaskFace::Landed),
        ..ChipGlass::default()
    };
    assert_eq!(
        ChipFace::Landed.spoken(&unplaced, Some(TODAY)),
        "Autopilot: land. Landed."
    );
}

#[test]
fn each_glyph_only_rung_is_the_glyph_alone() {
    let now = Instant::now();
    for row in rows() {
        if row.view == AutopilotView::Unhosted {
            continue;
        }
        let what = format!("{:?} {:?}", row.view, row.glass);
        let w = lights(row.view, row.glass);
        let block = w.block(7, blank(), now, ChipAt::cruise(blank()), Keep::GlyphOnly);
        assert_eq!(text(&block.lights), row.glyph, "{what}");
        assert!(
            block.lights.iter().all(|c| c.fg == ink(row.tones.0)),
            "{what}"
        );
        assert!(
            block.cols.iter().all(|(_, chip)| *chip == Chip::Autopilot),
            "{what}: Autopilot alone"
        );
        let s = &w.sessions[&7];
        let on = drawn_at(s, now, None, None, Keep::GlyphOnly);
        assert_eq!(
            chips_width_at(
                s,
                Chip::row().filter(|chip| on(*chip)),
                Keep::GlyphOnly,
                Inks::of(blank(), ChipAt::cruise(blank())),
                None,
            ),
            block.lights.len(),
            "{what}"
        );
    }
    let mut w = lights(AutopilotView::Cruise, timer(18, 0, None));
    w.sessions.get_mut(&7).unwrap().autopilot_pending =
        Some((Autopilot::Land, now + Duration::from_secs(1)));
    assert_eq!(
        text(
            &w.block(7, blank(), now, ChipAt::cruise(blank()), Keep::GlyphOnly)
                .lights
        ),
        PENDING_GLYPH
    );
    assert_eq!(
        Keep::LADDER.last(),
        Some(&Keep::GlyphOnly),
        "the ladder's last rung"
    );
    let apart: Vec<ChipFace> = ChipFace::ALL
        .into_iter()
        .filter(|face| face.rung() != face.glyph())
        .collect();
    assert!(
        apart.is_empty(),
        "every face's rung is its glyph: {apart:?}"
    );
}

#[test]
fn a_narrow_rule_gives_a_standing_face_down_to_its_glyph() {
    let now = Instant::now();
    let standing: Vec<ChipFace> = ChipFace::ALL.into_iter().filter(|f| f.standing()).collect();
    assert_eq!(
        standing,
        [
            ChipFace::CruiseUntil,
            ChipFace::Landed,
            ChipFace::Theirs,
            ChipFace::Failed
        ],
        "CONTROL: v2's standing faces"
    );
    for view in AutopilotView::ALL {
        assert!(
            !ChipFace::of(view, &ChipGlass::default()).standing(),
            "CONTROL: v1's {view:?} stands on v1's ladder"
        );
    }
    for row in rows() {
        if row.view == AutopilotView::Unhosted {
            continue;
        }
        let what = format!("{:?} {:?}", row.view, row.glass);
        let w = lights(row.view, row.glass);
        let face = row.drawn.chars().count();
        let mut wider = usize::MAX;
        for room in (0..=face + 4).rev() {
            let block = w.fitted(7, blank(), now, ChipAt::cruise(blank()), None, None, room);
            let drawn = block.as_ref().map(|b| text(&b.lights));
            let want = if room >= face {
                Some(row.drawn.to_owned())
            } else if room >= 1 && row.face.standing() {
                Some(row.glyph.to_owned())
            } else {
                None
            };
            assert_eq!(drawn, want, "{what} in {room} cells");
            let cells = drawn.map_or(0, |d| d.chars().count());
            assert!(cells <= wider, "{what} in {room} cells: a chip came back");
            wider = cells;
            if let Some(block) = block
                && room < face
            {
                assert!(
                    block.lights.iter().all(|c| c.fg == ink(row.tones.0)),
                    "{what} in {room} cells: the glyph's ink"
                );
                assert_eq!(block.holds, room, "{what} in {room} cells: holds the room");
            }
        }
    }
}

#[test]
fn the_default_glass_is_v1s_chip() {
    let now = Instant::now();
    let rest = ChipGlass::default();
    for view in AutopilotView::ALL {
        let face = ChipFace::of(view, &rest);
        let drawn = match face.word() {
            Some(word) => format!("{} {word}", face.glyph()),
            None => face.glyph().to_owned(),
        };
        assert_eq!(drawn, view.label(), "{view:?}");
        assert_eq!(face.tones(), (view.tone(), view.tone()), "{view:?}");
        assert_eq!(face.clock(&rest), None, "{view:?}");
        let (whole, short) = autopilot_title(view, &rest, "", None, TitleWords::default());
        assert_eq!(
            whole,
            format!("{} \u{b7} right-click: more", view.title()),
            "{view:?}"
        );
        assert_eq!(short, short_title(&view.title()), "{view:?}");
        let [_, (head, tail), ..] = lean_titles(&whole);
        assert_eq!(
            format!("{head}{tail}"),
            view.title(),
            "{view:?}: v1's words"
        );
        let mut w = WindowLights::default();
        w.note_autopilot(7, view.mode(), view, now);
        let v1 = |word: Option<Autopilot>| word.map_or(0, |w| 1 + w as u64);
        assert_eq!(
            w.autopilot_term(7, view.mode(), view, rest),
            v1(view.mode()) | view.code() << 8,
            "{view:?}"
        );
    }
    assert_eq!(glass_bits(rest, true), 0);
}

#[test]
fn the_autopilot_title_is_sized_once() {
    let rest = ChipGlass::default();
    for view in AutopilotView::ALL {
        let ((whole, short), allocations) = crate::work_counts::allocations_in(|| {
            autopilot_title(view, &rest, "", None, TitleWords::default())
        });
        assert_eq!(allocations, 2, "{view:?}: {whole}");
        assert!(
            whole.starts_with(&view.title()),
            "CONTROL {view:?}: {whole}"
        );
        assert!(whole.ends_with("right-click: more"), "CONTROL {view:?}");
        assert_eq!(whole.capacity(), whole.len(), "sized exactly: {whole}");
        assert_eq!(short, short_title(&view.title()), "CONTROL {view:?}");
    }
    let trail = "\u{2713} approved 14:05";
    let ((whole, _), allocations) = crate::work_counts::allocations_in(|| {
        autopilot_title(
            AutopilotView::Cruise,
            &rest,
            trail,
            None,
            TitleWords::default(),
        )
    });
    assert_eq!(allocations, 2, "{whole}");
    assert_eq!(whole.capacity(), whole.len(), "sized exactly: {whole}");
}

#[derive(Debug, PartialEq, Eq)]
struct Unpacked {
    lit: Option<Act>,
    task: Option<TaskFace>,
    minute: Option<u16>,
    other_day: bool,
    trail: u8,
    weekday: Option<Weekday>,
}

fn unpack(term: u64) -> Unpacked {
    let field = |at: u32, width: u32| (term >> at) & ((1 << width) - 1);
    let lit = match field(24, 8) {
        0 => None,
        n => Some(Act::ALL[usize::try_from(n - 1).unwrap()]),
    };
    let task = match field(32, 3) {
        0 => None,
        1 => Some(TaskFace::Landed),
        2 => Some(TaskFace::Failed),
        4 => Some(TaskFace::Theirs),
        n => panic!("no face {n}"),
    };
    let minute = match field(36, 11) {
        0 => None,
        n => Some(u16::try_from(n - 1).unwrap()),
    };
    assert_eq!(
        term & 0xff_ffff,
        0,
        "v1's bits are the word's, view's and click's"
    );
    assert_eq!(term >> 59, 0, "bits 59–63 are spare");
    assert_eq!(
        field(35, 1),
        0,
        "bit 35 is spare (the retired Codex goal hold)"
    );
    Unpacked {
        lit,
        task,
        minute,
        other_day: field(47, 1) == 1,
        trail: u8::try_from(field(48, 8)).unwrap(),
        weekday: match field(56, 3) {
            0 => None,
            n => Some(
                [
                    Weekday::Mon,
                    Weekday::Tue,
                    Weekday::Wed,
                    Weekday::Thu,
                    Weekday::Fri,
                    Weekday::Sat,
                    Weekday::Sun,
                ][usize::try_from(n - 1).unwrap()],
            ),
        },
    }
}

#[test]
fn the_autopilot_terms_bit_table_round_trips() {
    let acts = std::iter::once(None).chain(Act::ALL.into_iter().map(Some));
    for lit in acts {
        let faces = std::iter::once(None).chain(TaskFace::ALL.into_iter().map(Some));
        for task in faces {
            for at in [
                None,
                Some((0, None)),
                Some((1439, Some(Weekday::Sun))),
                Some((1080, None)),
            ] {
                for (trail, reading) in [(0, true), (7, true), (255, true), (9, false)] {
                    let glass = ChipGlass {
                        lit,
                        task,
                        task_at: Some(stamp(17, 42)),
                        land_at: at.map(|(minute, other_day)| LandAtGlass {
                            clock: Clock::of_minute(minute),
                            other_day,
                        }),
                        last: Some((Act::Approved, stamp(14, 5))),
                        last_by: None,
                        trail,
                        pause: None,
                    };
                    let back = unpack(glass_bits(glass, reading));
                    assert_eq!(
                        back,
                        Unpacked {
                            lit,
                            task: task.filter(|f| *f != TaskFace::Open),
                            minute: at.map(|(m, _)| m),
                            other_day: at.is_some_and(|(_, d)| d.is_some()),
                            trail: if reading { trail } else { 0 },
                            weekday: at.and_then(|(_, d)| d),
                        },
                        "{glass:?} reading={reading}"
                    );
                }
            }
        }
    }
    let mut w = WindowLights::default();
    w.note_autopilot(
        7,
        Some(Autopilot::Cruise),
        AutopilotView::Cruise,
        Instant::now(),
    );
    let glass = ChipGlass {
        lit: Some(Act::Nudged),
        ..timer(23, 59, Some(Weekday::Mon))
    };
    let term = w.autopilot_term(7, Some(Autopilot::Cruise), AutopilotView::Cruise, glass);
    assert_eq!(
        term & 0xff_ffff,
        w.autopilot_term(
            7,
            Some(Autopilot::Cruise),
            AutopilotView::Cruise,
            ChipGlass::default()
        )
    );
    assert_eq!(term >> 24, glass_bits(glass, false) >> 24);
}

#[test]
fn the_trail_moves_the_term_only_while_the_chip_is_read() {
    let now = Instant::now();
    let mut w = WindowLights::default();
    w.note_autopilot(7, None, AutopilotView::Cruise, now);
    let term = |w: &WindowLights, trail: u8| {
        let glass = ChipGlass {
            trail,
            ..ChipGlass::default()
        };
        w.autopilot_term(7, None, AutopilotView::Cruise, glass)
    };
    assert_eq!(term(&w, 3), term(&w, 4), "no one reads the title");
    w.hover = Some((7, Target::from(Chip::Light(Light::Mode))));
    assert_eq!(term(&w, 3), term(&w, 4), "another chip's title");
    w.hover = Some((8, Target::from(Chip::Autopilot)));
    assert_eq!(term(&w, 3), term(&w, 4), "another session's chip");
    w.hover = Some((7, Target::from(Chip::Autopilot)));
    assert_ne!(term(&w, 3), term(&w, 4), "the pointer reads it");
    w.hover = None;
    w.selected = Some((7, Target::from(Chip::Autopilot)));
    assert_ne!(term(&w, 3), term(&w, 4), "the keyboard reads it");
}

#[test]
fn a_face_clock_that_moves_under_its_face_repaints_the_title_being_read() {
    let mut app = App::headless_for_test();
    let mut w = WindowLights::default();
    let term =
        |w: &WindowLights, glass: ChipGlass| w.autopilot_term(7, None, AutopilotView::Land, glass);
    assert!(crate::harness_glass::on_task(
        &mut app,
        7,
        Some(TaskFace::Landed),
        1_792_249_200_000
    ));
    let before = app.chip_glass(7);
    let at_rest = term(&w, before);
    w.hover = Some((7, Target::from(Chip::Autopilot)));
    let read = term(&w, before);
    assert!(crate::harness_glass::on_task(
        &mut app,
        7,
        Some(TaskFace::Landed),
        1_792_250_700_000
    ));
    let after = app.chip_glass(7);
    assert_eq!(after.task, before.task, "the same face");
    assert_ne!(after.task_at, before.task_at, "its clock moved");
    assert_ne!(term(&w, after), read, "the title being read is built again");
    w.hover = None;
    assert_eq!(
        term(&w, after),
        at_rest,
        "no one reads it: nothing repaints"
    );
    w.hover = Some((7, Target::from(Chip::Autopilot)));
    let clock_alone = ChipGlass {
        trail: before.trail,
        ..after
    };
    assert_eq!(term(&w, clock_alone), read);
}

#[test]
fn the_acts_label_and_the_glass_clock_are_the_tables() {
    assert_eq!(
        act_label(AutopilotView::Cruise, Act::Approved),
        ("\u{2713}", "approved", ChipTone::Letter)
    );
    assert_eq!(
        act_label(AutopilotView::Land, Act::Continued),
        ("\u{21bb}", "kept going", ChipTone::Letter)
    );
    for minute in [0, 59, 60, 1080, 1439] {
        assert_eq!(Clock::of_minute(minute).minute(), minute);
    }
    assert_eq!(Clock::of_minute(18 * 60).as_str(), "18:00");
    assert_eq!(Clock::of_minute(7 * 60 + 5).as_str(), "07:05");
    assert_eq!(Clock::of_minute(1440), Clock::of_minute(0), "wraps");
    assert_eq!(Weekday::of_days(0), Weekday::Thu);
    assert_eq!(Weekday::of_days(2), Weekday::Sat);
    assert_eq!(Weekday::of_days(-1), Weekday::Wed);
    assert_eq!(
        (Weekday::Sat.short(), Weekday::Sat.long()),
        ("Sat", "Saturday")
    );
}

/// The amber Land and auto wore before the glass's tokens (Claude Code's auto-mode pill).
const PILL_AMBER: [u8; 3] = [0xE8, 0xB0, 0x3C];

/// A built-in theme's row cell.
fn theme_row(name: &str) -> RenderCell {
    let rgb = |c: u32| [(c >> 16) as u8, (c >> 8) as u8, c as u8];
    let scheme = aterm_types::scheme::builtin(name).expect("a listed scheme exists");
    let theme = aterm_render::Theme::from(&scheme);
    RenderCell {
        ch: ' ',
        fg: rgb(theme.fg),
        bg: rgb(theme.bg),
        ..RenderCell::default()
    }
}

/// LAND'S LETTERS READ ON EVERY BUILT-IN THEME: under each view's palette the Land chip, a
/// landed one and a timed cruise's `⤓18:00` wear the letter of the chip's column, at 4.5:1 or
/// more, and an act's label the letter of its own column. NEGATIVE CONTROL: the fixed amber
/// Land wore before fails a light ground.
#[test]
fn lands_letters_read_on_every_builtin_theme() {
    use crate::chrome_band::contrast;
    let now = Instant::now();
    let mut under = Vec::new();
    for name in aterm_types::scheme::builtin_names() {
        let row = theme_row(name);
        let ground = row.bg;
        if contrast(PILL_AMBER, ground) < 4.5 {
            under.push(name);
        }
        for (view, glass, at, cells) in [
            (
                AutopilotView::Land,
                ChipGlass::default(),
                ChipAt::land(row),
                0..1,
            ),
            (
                AutopilotView::Land,
                told(TaskFace::Landed),
                ChipAt::land(row),
                0..8,
            ),
            (
                AutopilotView::Cruise,
                timer(18, 0, None),
                ChipAt::cruise(row),
                0..8,
            ),
        ] {
            let w = lights(view, glass);
            let block = w.block(7, row, now, at, Keep::Drawn);
            let letter = at.letter_at(0);
            assert!(
                contrast(letter, ground) >= 4.5,
                "{name} {view:?}: {letter:?} on {ground:?}"
            );
            let cells = &block.lights[cells];
            assert!(
                cells.iter().all(|c| c.fg == letter),
                "{name} {view:?}: {:?}",
                text(cells)
            );
        }
        let mut w = lights(
            AutopilotView::Land,
            ChipGlass {
                lit: Some(Act::Approved),
                ..ChipGlass::default()
            },
        );
        w.hover = None;
        let at = ChipAt::land(row);
        let block = w.block(7, row, now, at, Keep::Drawn);
        let letter = at.letter_at(block.lights.len() + 2);
        let label: Vec<&RenderCell> = block.title.iter().filter(|c| c.ch != ' ').collect();
        assert!(!label.is_empty(), "{name}: the act's label");
        assert!(
            label.iter().all(|c| c.fg == letter),
            "{name}: the act's label in its column's letter"
        );
    }
    assert!(
        under.contains(&"Aterm Day"),
        "NEGATIVE CONTROL: the fixed amber fails a light ground: {under:?}"
    );
}

/// AUTO READS AS A VALUE ON EVERY BUILT-IN THEME: the expected mode's light (`⏵⏵ auto`) wears
/// the glass's value ink, at 7:1 or more, never its pill's amber. NEGATIVE CONTROL: the pill's
/// amber fails a light ground.
#[test]
fn auto_reads_as_a_value_on_every_builtin_theme() {
    use crate::chrome_band::contrast;
    let now = Instant::now();
    assert_eq!(Mode::Auto.ink(), ModeInk::Value, "auto: the value ink");
    let mut under = Vec::new();
    for name in aterm_types::scheme::builtin_names() {
        let row = theme_row(name);
        let ground = row.bg;
        if contrast(PILL_AMBER, ground) < 4.5 {
            under.push(name);
        }
        let mut w = lights(AutopilotView::Cruise, ChipGlass::default());
        w.sessions.get_mut(&7).expect("session 7").shown.mode = Some(Mode::Auto);
        w.hover = Some((7, Target::from(Chip::Light(Light::Mode))));
        let at = ChipAt::cruise(row);
        let block = w.block(7, row, now, at, Keep::Drawn);
        let pill: String = format!("{} {}", Mode::Auto.glyph(), Mode::Auto.word());
        let all = text(&block.lights);
        let first = all
            .find(&pill)
            .map(|at| all[..at].chars().count())
            .unwrap_or_else(|| panic!("{name}: the light drawn: {all:?}"));
        for cell in block.lights[first..first + pill.chars().count()]
            .iter()
            .filter(|c| c.ch != ' ')
        {
            assert_eq!(cell.fg, at.value(), "{name}: the value ink");
            assert!(
                contrast(cell.fg, ground) >= 6.99,
                "{name}: {:?} on {ground:?}",
                cell.fg
            );
        }
    }
    assert!(
        under.contains(&"Aterm Day"),
        "NEGATIVE CONTROL: the pill's amber fails a light ground: {under:?}"
    );
}

/// YOLO READS AS A VALUE, NEVER RED (the owner, 2026-11-02): on every built-in theme every cell
/// of `yolo` (a rewritten row) and of `⏵⏵ bypass` (Claude's own row) wears the glass's value ink,
/// at 7:1 or more, whatever grey the rule it sits on wears. NEGATIVE CONTROL: Claude's rule grey,
/// floored to read, is not that ink on some ground, so the rule no longer decides it.
#[test]
fn yolo_wears_the_value_ink_on_every_builtin_theme() {
    use crate::chrome_band::contrast;
    let now = Instant::now();
    let old_red = [0xE0, 0x5A, 0x5A];
    let mut apart = Vec::new();
    for name in aterm_types::scheme::builtin_names() {
        let row = theme_row(name);
        let ground = row.bg;
        let at = ChipAt::cruise(row);
        if crate::chrome_band::ensure_contrast([0x88; 3], ground, 4.5) != at.value() {
            apart.push(name);
        }
        for rewritten in [true, false] {
            let mut w = lights(AutopilotView::Cruise, ChipGlass::default());
            let s = w.sessions.get_mut(&7).expect("session 7");
            s.shown.mode = Some(Mode::Bypass);
            s.rewritten = rewritten;
            w.hover = Some((7, Target::from(Chip::Light(Light::Mode))));
            let block = w.block(7, row, now, at, Keep::Drawn);
            let face = match Mode::Bypass.glyph_on_glass(rewritten) {
                "" => Mode::Bypass.word_on_glass(rewritten).to_owned(),
                glyph => format!("{glyph} {}", Mode::Bypass.word_on_glass(rewritten)),
            };
            assert_eq!(face == "yolo", rewritten, "{name}");
            let all = text(&block.lights);
            let first = all
                .find(&face)
                .map(|at| all[..at].chars().count())
                .unwrap_or_else(|| panic!("{name}: {face:?} drawn: {all:?}"));
            for cell in block.lights[first..first + face.chars().count()]
                .iter()
                .filter(|c| c.ch != ' ')
            {
                assert_eq!(cell.fg, at.value(), "{name} rewritten={rewritten}");
                assert_ne!(cell.fg, old_red, "{name}: never Claude's red");
                assert!(
                    contrast(cell.fg, ground) >= 6.99,
                    "{name}: {:?} on {ground:?}",
                    cell.fg
                );
            }
        }
    }
    assert!(
        !apart.is_empty(),
        "NEGATIVE CONTROL: Claude's grey is the value ink on every ground"
    );
}

#[test]
fn every_face_says_a_hold_and_a_need_where_its_harness_explains() {
    use crate::harness_glass::hold_tests::{ask, glass, held, unread};
    let need = ask();
    let (read, fresh) = (glass(held(), TODAY), glass(unread(), TODAY));
    let need_part = " \u{b7} needs you: sign 0.48.0, the release credential, on this Mac (+1 more); \
                     carrying on";
    let need_said = "Needs you: sign 0.48.0 the release credential on this Mac. And 1 more.";
    let legs = "2 unpushed commits; open in its report: follow-up, not pushed";
    let since = "landing since 14:02, 3 nudges";
    // No chip glyph (✓, ⤓) in a description; a landed one's what-aterm-checked sentence keeps
    // its own `;` and `'` (§14.9, 9c).
    let glyph_free = |text: &str| {
        text.chars()
            .all(|c| c.is_alphanumeric() || matches!(c, ' ' | '.' | ':' | ',' | ';' | '\''))
    };
    for row in rows() {
        let face = row.face;
        let explained = !matches!(
            face,
            ChipFace::MasterOff | ChipFace::AgentOff(_) | ChipFace::Unhosted
        );
        let land = face == ChipFace::Land;
        let click_at = row.whole.rfind(" \u{b7} click: ").expect("a click");
        let (head, clicks) = row.whole.split_at(click_at);
        let menu_at = clicks.rfind(" \u{b7} right-click: more").expect("the menu");
        for (case, hold, said_need) in [
            ("none", None, None),
            ("a need", None, Some(&need)),
            ("a read hold", Some(&read), None),
            ("an unread hold", Some(&fresh), None),
            ("a read hold and a need", Some(&read), Some(&need)),
            ("an unread hold and a need", Some(&fresh), Some(&need)),
        ] {
            let words = TitleWords {
                stopped: None,
                hold,
                need: said_need,
                switch: false,
                receipt: None,
                said: None,
            };
            let (whole, short) = autopilot_title(row.view, &row.glass, "", Some(TODAY), words);
            let read_hold = land && hold.is_some_and(|h| h.read());
            let mut expected = match hold.filter(|_| land) {
                Some(h) if h.read() => {
                    let first = if said_need.is_some() {
                        "needs you: sign 0.48.0, the release credential, on this Mac (+1 more)"
                    } else {
                        "a need named earlier in its reply"
                    };
                    format!(
                        "{} \u{2014} not landed yet: {first}; {legs} \u{b7} {since}",
                        row.short
                    )
                }
                Some(_) => format!("{head} \u{b7} {since}"),
                None => head.to_owned(),
            };
            if said_need.is_some() && explained && !read_hold {
                expected.push_str(need_part);
            }
            let head_said = expected.clone();
            expected.push_str(clicks);
            assert_eq!(whole, expected, "{face:?}, {case}");
            assert_eq!(short, row.short, "{face:?}, {case}");
            assert_eq!(whole.capacity(), whole.len(), "sized exactly: {whole}");
            assert!(whole.matches(" \u{2014} ").count() <= 1, "{whole}");
            assert_eq!(whole.matches(" \u{b7} click: ").count(), 1, "{whole}");
            assert!(!whole.contains(" \u{b7} last: "), "{whole}");
            assert!(!whole.contains(" \u{b7} pausing until "), "{whole}");
            let [lean, v1, both, click] =
                lean_titles(&whole).map(|(head, tail)| format!("{head}{tail}"));
            assert_eq!(lean, "", "no trail: {whole}");
            assert_eq!(
                v1,
                format!("{head_said}{}", &clicks[..menu_at]),
                "v1's words keep the hold and the need: {whole}"
            );
            if whole.contains(" \u{2014} ") {
                assert_eq!(
                    click,
                    format!("{}{}", row.short, &clicks[..menu_at]),
                    "{whole}"
                );
                assert!(
                    both.is_empty() || both == format!("{}{clicks}", row.short),
                    "{whole}"
                );
            } else {
                assert_eq!((both.as_str(), click.as_str()), ("", ""), "{whole}");
            }
            for form in [&both, &click] {
                for word in ["needs you", "landing since", "not landed"] {
                    assert!(
                        !form.contains(word),
                        "given up with the explanation: {form}"
                    );
                }
            }
            let description = autopilot_description(face, &row.glass, words, Some(TODAY), None);
            let mut sentences = Vec::new();
            if face == ChipFace::Landed {
                // What aterm checked leads a landed chip's description (§14.9, 9c): these rows
                // carry no receipt, so it checked nothing.
                sentences.push(format!(
                    "{}.",
                    aterm_agent::supervise::mode_words::landed_words(None)
                ));
            }
            match hold.filter(|_| land) {
                Some(h) if h.read() => sentences.push(format!(
                    "Not landed yet. {} 2 unpushed commits. Open in its report: follow up, \
                     not pushed. Landing since 14:02, 3 nudges.",
                    if said_need.is_some() {
                        need_said
                    } else {
                        "A need named earlier in its reply."
                    }
                )),
                Some(_) => sentences.push("Landing since 14:02, 3 nudges.".to_owned()),
                None => {}
            }
            if said_need.is_some() && explained && !read_hold {
                sentences.push(format!("{need_said} Carrying on."));
            }
            let wanted = (!sentences.is_empty()).then(|| sentences.join(" "));
            assert_eq!(description, wanted, "{face:?}, {case}");
            if let Some(description) = &description {
                assert!(glyph_free(description), "{description}");
            }
            assert_eq!(face.spoken(&row.glass, Some(TODAY)), row.spoken);
        }
    }
}

/// A LANDED CHIP SAYS THE NEED WAITS ON THE PERSON, AS ITS TITLE DOES (the post-landing review,
/// 2026-11-02): the description speaks the entry's own tail ([`crate::needs::NeedTail`], the one
/// source the title's `need_head` reads), so a need beside a landing is heard as waiting, not as
/// work that goes on. Negative control: `need_spoken`'s literal `Carrying on.` (`origin/main`
/// `21b1e42025`) speaks "Carrying on." where the title says "waits on you".
#[test]
fn a_landed_chip_speaks_its_needs_own_tail() {
    use crate::needs::{HarnessAsk, NeedTail};
    let glass = told(TaskFace::Landed);
    let face = ChipFace::of(AutopilotView::Land, &glass);
    assert_eq!(face, ChipFace::Landed);
    for (tail, not) in [
        (NeedTail::WaitsOnYou, NeedTail::CarryingOn),
        (NeedTail::CarryingOn, NeedTail::WaitsOnYou),
    ] {
        let (said, not) = (tail.sentence(), not.sentence());
        let ask = HarnessAsk {
            first: "rotate the deploy key".to_string(),
            more: 0,
            others: Vec::new(),
            program: aterm_phase::Program::Claude,
            tail,
        };
        let words = TitleWords {
            need: Some(&ask),
            ..TitleWords::default()
        };
        let spoken = autopilot_description(face, &glass, words, Some(TODAY), None)
            .expect("a landed chip speaks");
        assert!(
            spoken.ends_with(&format!("Needs you: rotate the deploy key. {said}")),
            "{tail:?}: {spoken}"
        );
        assert!(!spoken.contains(&not), "{tail:?}: {spoken}");
        let (title, _) = autopilot_title(AutopilotView::Land, &glass, "", Some(TODAY), words);
        assert!(title.contains(tail.words()), "{tail:?} title: {title}");
        assert!(spoken.is_ascii(), "glyph-free: {spoken}");
    }
}

#[test]
fn a_need_in_the_title_keeps_its_cut_marks() {
    use crate::harness_glass::PauseGlass;
    let raw = crate::needs::HarnessAsk {
        first: "sign 0.48.0 \u{b7} last: the release credential \u{2014} on this Mac \u{b7} \
                click: approve"
            .to_string(),
        more: 1,
        others: Vec::new(),
        program: aterm_phase::Program::Claude,
        tail: crate::needs::NeedTail::CarryingOn,
    };
    let written = crate::needs::HarnessAsk {
        first: "sign 0.48.0, last: the release credential, on this Mac, click: approve".to_string(),
        ..raw.clone()
    };
    let trail = "\u{2713} approved 14:05";
    for row in rows() {
        let glass = ChipGlass {
            pause: Some(PauseGlass::of(crate::PausePlan {
                at_unix: 1_792_159_500,
                short: 3,
            })),
            ..row.glass
        };
        let title = |need: &crate::needs::HarnessAsk| {
            autopilot_title(
                row.view,
                &glass,
                trail,
                Some(TODAY),
                TitleWords {
                    stopped: None,
                    hold: None,
                    need: Some(need),
                    switch: false,
                    receipt: None,
                    said: None,
                },
            )
            .0
        };
        let (cut, plain) = (title(&raw), title(&written));
        assert_eq!(cut, plain, "{:?}", row.face);
        let forms = lean_titles(&cut);
        assert_eq!(forms, lean_titles(&plain), "{cut}");
        let [lean, v1, ..] = forms;
        assert!(!lean.0.is_empty(), "{cut}");
        assert!(!v1.0.is_empty(), "{cut}");
        if matches!(
            row.face,
            ChipFace::MasterOff | ChipFace::AgentOff(_) | ChipFace::Unhosted
        ) {
            assert!(!cut.contains("needs you"), "no harness carries on: {cut}");
        } else {
            assert!(
                lean.0.contains("; carrying on \u{b7} pausing until "),
                "the first leaner form keeps the need and the pause: {cut}"
            );
            assert!(v1.0.ends_with("; carrying on"), "v1 keeps the need: {cut}");
        }
    }
}

#[test]
fn a_held_landing_keeps_the_titles_four_lean_forms() {
    use crate::harness_glass::hold_tests::{ask, glass, held};
    let hold = glass(held(), TODAY);
    let need = ask();
    let pause = crate::harness_glass::PauseGlass::of(crate::PausePlan {
        at_unix: 1_792_159_500,
        short: 3,
    });
    let chip = ChipGlass {
        pause: Some(pause),
        ..ChipGlass::default()
    };
    let words = TitleWords {
        stopped: None,
        hold: Some(&hold),
        need: Some(&need),
        switch: false,
        receipt: None,
        said: None,
    };
    let (whole, short) = autopilot_title(
        AutopilotView::Land,
        &chip,
        "\u{2913} nudged 14:22",
        Some(TODAY),
        words,
    );
    assert_eq!(short, "autopilot: land");
    let explanation = ChipFace::Land
        .explanation(&chip, words, Some(TODAY))
        .expect("the hold");
    let (legs, since) = explanation
        .split_once(" \u{b7} ")
        .expect("the legs, then the clock");
    assert_eq!(since, "landing since 14:02, 3 nudges", "{whole}");
    for leg in legs.split("; ") {
        for mark in [
            " \u{2014} ",
            " \u{b7} last: ",
            " \u{b7} pausing until ",
            " \u{b7} ",
        ] {
            assert!(!leg.contains(mark), "{leg:?} in {whole}");
        }
    }
    assert!(
        whole.starts_with(&format!(
            "autopilot: land \u{2014} {explanation} \u{b7} pausing until "
        )),
        "{whole}"
    );
    assert_eq!(whole.matches(" \u{2014} ").count(), 1, "{whole}");
    let [lean, v1, clicks, click] = lean_titles(&whole).map(|(head, tail)| format!("{head}{tail}"));
    assert_eq!(
        lean,
        whole.replace(" \u{b7} last: \u{2913} nudged 14:22", ""),
        "without the trail"
    );
    assert_eq!(
        v1,
        format!("autopilot: land \u{2014} {explanation} \u{b7} click: off"),
        "v1's words keep the hold"
    );
    assert_eq!(
        clicks,
        "autopilot: land \u{b7} click: off \u{b7} right-click: more"
    );
    assert_eq!(click, "autopilot: land \u{b7} click: off");
    let mut app = App::headless_for_test();
    assert!(crate::harness_glass::on_land_held(
        &mut app,
        0,
        Some(held())
    ));
    let (label, description) = app.autopilot_spoken(0, AutopilotView::Land, &chip);
    assert_eq!(label, ChipFace::Land.spoken(&chip, Some(TODAY)));
    assert_eq!(label, "Autopilot: land.", "the painted word, unchanged");
    let description = description.expect("the hold described");
    assert!(
        description
            .starts_with("Not landed yet. A need named earlier in its reply. 2 unpushed commits."),
        "{description}"
    );
    assert!(
        description
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, ' ' | '.' | ':' | ',')),
        "glyph-free: {description}"
    );
    assert!(description.contains(" Pausing until "), "{description}");
}

#[test]
fn the_dim_faces_and_the_rows_ink_read_on_every_ground() {
    use aterm_effects::cli_marks::{ICON_CONTRAST, NAME_CONTRAST};
    let pack = |c: [u8; 3]| u32::from_be_bytes([0, c[0], c[1], c[2]]);
    let reads =
        |a: [u8; 3], b: [u8; 3]| aterm_effects::color_math::contrast_ratio(pack(a), pack(b));
    let halfway = |b: RenderCell| -> [u8; 3] {
        std::array::from_fn(|i| ((u16::from(b.fg[i]) + u16::from(b.bg[i])) / 2) as u8)
    };
    let rgb = |c: aterm_types::Rgb| [c.r, c.g, c.b];
    let blank_of = |name: &str| {
        let scheme = aterm_types::scheme::builtin(name).expect("a built-in");
        RenderCell {
            ch: ' ',
            fg: rgb(scheme.foreground),
            bg: rgb(scheme.background),
            ..RenderCell::default()
        }
    };
    let names = aterm_types::scheme::builtin_names();
    let soft = RenderCell {
        ch: ' ',
        fg: [0x7a; 3],
        bg: [0xff; 3],
        ..RenderCell::default()
    };
    let tabs: Vec<(&str, RenderCell)> = names
        .iter()
        .map(|&name| (name, blank_of(name)))
        .chain(std::iter::once(("a soft theme of one's own", soft)))
        .collect();
    let mut checked = 0usize;
    let mut lifted = 0usize;
    for &(tab, blank) in &tabs {
        let inks = Inks::of(blank, ChipAt::cruise(blank));
        let (ink, value) = (inks.ink, inks.title);
        let (ink_k, value_k) = (reads(ink, blank.bg), reads(value, blank.bg));
        // The row's ink reads as words do and never quieter than a value, so an unexpected
        // mode never reads under an expected one: the pane's own ink where it reads that loud.
        assert!(
            ink_k >= NAME_CONTRAST && ink_k >= value_k,
            "{tab}: the row's ink at {ink_k:.2}:1, a value at {value_k:.2}:1"
        );
        let own = reads(blank.fg, blank.bg);
        if own >= value_k {
            assert_eq!(
                ink, blank.fg,
                "{tab}: an ink that reads as loud as a value is kept"
            );
        }
        lifted += usize::from((NAME_CONTRAST..value_k).contains(&own));
        for (view, palette) in [
            (
                AutopilotView::Off,
                Some(aterm_effects::harness_card::CardPalette::Off),
            ),
            (AutopilotView::MasterOff, None),
            (AutopilotView::Unhosted, None),
        ] {
            let at = ChipAt::test(palette, blank);
            let block = Block::of_autopilot(view, blank, at);
            for c in block.lights.iter().filter(|c| c.ch != ' ') {
                let k = reads(c.fg, blank.bg);
                // Quiet, never faint: as words read, and quieter than the row's ink, as loud
                // only on a tab whose own ink reads under the floor, which leaves no room above
                // it.
                let room = reads(blank.fg, blank.bg) >= NAME_CONTRAST;
                assert!(
                    k >= NAME_CONTRAST && (k < ink_k || (!room && k <= ink_k)),
                    "{tab}, {view:?}: {:?} at {k:.2}:1, the row's ink at {ink_k:.2}:1",
                    c.ch
                );
                assert_eq!(c.fg, at.quiet(), "{tab}, {view:?}: the quiet ink");
                checked += 1;
            }
        }
    }
    // Off's `○` and the master's `⊘ off`, on every tab.
    assert!(checked >= tabs.len() * 5, "{checked}");
    for name in ["Solarized Light", "Default Light"] {
        let blank = blank_of(name);
        assert!(
            reads(halfway(blank), blank.bg) < ICON_CONTRAST,
            "CONTROL: halfway at {:.2}:1 on {name}",
            reads(halfway(blank), blank.bg)
        );
    }
    assert!(
        reads(soft.fg, soft.bg) < NAME_CONTRAST,
        "CONTROL: the soft theme's ink at {:.2}:1",
        reads(soft.fg, soft.bg)
    );
    assert!(
        lifted > 0,
        "CONTROL: no tab's own ink reads between the name floor and a value (Solarized does)"
    );
}
