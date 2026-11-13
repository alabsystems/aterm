// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// The mode light's chip.
const MODE: Chip = Chip::Light(Light::Mode);

#[test]
fn inert_session_history_does_not_change_the_lights_repaint_key() {
    let now = Instant::now();
    let mut w = WindowLights {
        hover: Some((7, Target::from(MODE))),
        ..WindowLights::default()
    };
    w.sessions.insert(7, SessionLights::default());
    let hovered = w.fingerprint(|| now);
    assert_ne!(hovered, 0);

    for session in 100..1_100 {
        w.sessions.insert(session, SessionLights::default());
    }
    assert_eq!(w.fingerprint(|| now), hovered);

    w.sessions.get_mut(&1_000).unwrap().refused =
        Some((Light::Mode, Refusal::NotShown, now + Duration::from_secs(1)));
    assert_ne!(w.fingerprint(|| now), hovered);
    assert_eq!(w.fingerprint(|| now + Duration::from_secs(2)), hovered);

    w.hover = None;
    assert_eq!(w.fingerprint(|| now + Duration::from_secs(2)), 0);
}

fn blank() -> RenderCell {
    RenderCell {
        ch: ' ',
        fg: [200, 200, 200],
        bg: [0, 0, 0],
        ..RenderCell::default()
    }
}

fn screen(rule: &str, mode_row: &str, draft: &str) -> Vec<String> {
    vec![
        rule.into(),
        format!("\u{276F} {draft}"),
        "\u{2500}".repeat(60),
        mode_row.into(),
    ]
}

const BYPASS: &str = "  \u{23F5}\u{23F5} bypass permissions on (shift+tab to cycle)";
const AUTO: &str = "  \u{23F5}\u{23F5} auto mode on (shift+tab to cycle)";
const PLAN: &str = "  \u{23F8} plan mode on (shift+tab to cycle)";
const ACCEPT: &str = "  \u{23F5}\u{23F5} accept edits on (shift+tab to cycle)";
const MANUAL: &str = "  \u{23F8} manual mode on \u{00B7} ? for shortcuts";

fn observe(w: &mut WindowLights, rows: &[String]) {
    w.observe_rows(7, rows, lights::read_lights);
}

#[test]
fn a_reader_panic_folds_nothing_in_and_types_nothing() {
    let rule = "\u{2500}".repeat(60);
    let mut w = WindowLights::default();
    observe(&mut w, &screen(&rule, MANUAL, ""));
    let shown = w.sessions[&7].shown;
    let boom = |_: &[String]| -> Option<Reading> { panic!("stand-in reader panic") };
    let plan = screen(&rule, PLAN, "");
    w.observe_rows(7, &plan, boom);
    let s = &w.sessions[&7];
    assert_eq!(s.shown, shown, "the lights keep what they showed");
    assert!(
        fenced_screen_now(7, &plan, || -> Option<Screen> {
            panic!("stand-in reader panic")
        })
        .is_none()
    );
    assert!(
        fenced_screen_now(7, &plan, || lights::read_screen(&plan, None)).is_some(),
        "the real reader sees the composer"
    );
    observe(&mut w, &plan);
    assert_eq!(
        w.sessions[&7].shown.mode,
        Some(Mode::Plan),
        "the real read folds"
    );
}

/// THE LIGHT SAYS THE BIT, ON A HOSTED SESSION, AND KEEPS WHAT IT SAW
/// (`docs/DESIGN-yolo-binary-2026-11-04.md` §2): bypass wears `yolo` in the glass's value ink,
/// every other pill a quiet `yolo` (never another word for the flag, and no mark before it),
/// each beside Autopilot at rest; a covered composer keeps the last reading; a hover titles it
/// apart from the chips: the mode in Claude's own words, then what a click restarts into,
/// `click: yolo` off and `click: settings default` on. With no bit read the light is drawn only
/// while a click stands, the same word slanted in the row's ink, so the chip is one width in
/// every state. CONTROL: an unhosted session draws no chip under any pill.
#[test]
fn the_light_says_the_bit_on_a_hosted_session_and_keeps_what_it_saw() {
    let now = Instant::now();
    let bare = "\u{2500}".repeat(60);
    let at = ChipAt::cruise(blank());
    let inks = Inks::of(blank(), at);
    let text = |cells: &[RenderCell]| -> String { cells.iter().map(|c| c.ch).collect() };
    assert_ne!(
        inks.quiet, inks.ink,
        "CONTROL: the quiet ink is not the row's"
    );
    for (mode, word, ink, says) in [
        (BYPASS, "yolo", inks.title, "bypass permissions on"),
        (AUTO, "yolo", inks.quiet, "auto mode on"),
        (PLAN, "yolo", inks.quiet, "plan mode on"),
        (ACCEPT, "yolo", inks.quiet, "accept edits on"),
        (MANUAL, "yolo", inks.quiet, "manual mode on"),
    ] {
        let mut w = WindowLights::default();
        observe(&mut w, &screen(&bare, mode, ""));
        w.note_autopilot(7, None, AutopilotView::Unhosted, now);
        assert!(
            w.block(7, blank(), now, at, Keep::Drawn).cols.is_empty(),
            "CONTROL: no host, no chip: {mode:?}"
        );
        w.note_autopilot(7, None, AutopilotView::Cruise, now);
        let block = w.block(7, blank(), now, at, Keep::Drawn);
        assert_eq!(text(&block.lights), format!("\u{221e} {word}"), "{mode:?}");
        assert_eq!(
            block.cols,
            vec![(0..1, Chip::Autopilot), (2..6, MODE)],
            "{mode:?}"
        );
        assert!(
            block.lights[2..].iter().all(|c| c.fg == ink && !c.italic),
            "{mode:?}: {word} in its ink, and upright"
        );
        assert!(
            !text(&block.lights).contains("lame"),
            "{mode:?}: the off chip names the flag, never another word"
        );
        assert!(block.title.is_empty(), "nothing asked for a title");
        w.observe(7, None);
        assert_eq!(
            w.block(7, blank(), now, at, Keep::Drawn).lights,
            block.lights,
            "a covered composer keeps the last reading"
        );
        w.hover = Some((7, Target::from(MODE)));
        let hovered = w.block(7, blank(), now, at, Keep::Drawn);
        // A chip names its click only on a host whose click restarts: a Windows tab has no
        // restart (`permission_click::RESTARTS`), so the off chip's title is the mode alone and
        // the on chip's the mode and what it means.
        let restarts = crate::permission_click::RESTARTS;
        let title = if mode == BYPASS && restarts {
            format!("{says} \u{b7} click: settings default")
        } else if mode == BYPASS {
            format!("{says} \u{b7} no prompts")
        } else if restarts {
            format!("{says} \u{b7} click: yolo")
        } else {
            says.to_owned()
        };
        assert_eq!(text(&hovered.title), format!("{title}  "), "{mode:?}");
        assert_eq!(text(&hovered.short_title), format!("{says}  "), "{mode:?}");
        assert!(!hovered.reason, "{mode:?}: a hover's title gives way first");
        assert_eq!(
            hovered.lights.len(),
            block.lights.len(),
            "the title is apart"
        );
        // The glass's one hover: the lit ink over one lit underline, no weight changed.
        let lit = at.lit();
        assert!(
            hovered.lights[2..].iter().all(|c| {
                c.fg == lit
                    && !c.bold
                    && (c.underline, c.underline_color) == (UnderlineStyle::Single, Some(lit))
            }),
            "the whole hovered chip is lit: {:?}",
            &hovered.lights[2..]
        );
        assert!(
            block.lights[2..]
                .iter()
                .all(|c| c.underline == UnderlineStyle::None),
            "CONTROL: at rest no line"
        );
    }
    let mut w = WindowLights::default();
    w.note_autopilot(7, None, AutopilotView::Cruise, now);
    assert_eq!(
        w.block(7, blank(), now, at, Keep::Drawn).cols,
        vec![(0..1, Chip::Autopilot)],
        "no bit read: Autopilot alone"
    );
    w.sessions.get_mut(&7).unwrap().click = Some(ClickShown {
        when: Some("after this turn"),
    });
    let block = w.block(7, blank(), now, at, Keep::Drawn);
    assert_eq!(text(&block.lights), "\u{221e} yolo");
    assert_eq!(block.cols[1], (2..6, MODE), "a standing click's light");
    assert!(
        block.lights[2..]
            .iter()
            .all(|c| c.italic && c.fg == inks.ink),
        "the word slanted in the row's ink while its click stands"
    );
}

/// NO CLICK WHERE NOTHING RESTARTS (`docs/DESIGN-yolo-binary-2026-11-04.md` §2): on a host
/// with no restart the off light's title says the mode alone, with no `click:` and no job a
/// restart would end, spoken the same, and the on light's says the mode and what it means.
/// CONTROL: a host that restarts offers the click each way and names the job it ends.
#[test]
fn the_light_offers_no_click_on_a_host_with_no_restart() {
    let mut w = WindowLights::default();
    observe(&mut w, &screen(&"\u{2500}".repeat(60), PLAN, ""));
    let s = w.sessions.get_mut(&7).unwrap();
    s.jobs = "1 shell".to_owned();
    let none = light_title(s, Light::Mode, None, false);
    assert_eq!(none.text.whole, "plan mode on");
    assert_eq!(none.text.short, "plan mode on");
    assert!(!none.text.reason);
    assert_eq!(none.spoken, "Plan mode on.");
    let offered = light_title(s, Light::Mode, None, true);
    assert_eq!(
        offered.text.whole, "plan mode on \u{b7} click: yolo \u{b7} ends 1 shell",
        "CONTROL: a host that restarts"
    );
    assert_eq!(offered.text.short, "plan mode on \u{b7} ends 1 shell");
    assert_eq!(
        offered.spoken,
        "Plan mode on. Click to turn yolo on. Ends 1 shell."
    );
    let mut w = WindowLights::default();
    observe(&mut w, &screen(&"\u{2500}".repeat(60), BYPASS, ""));
    let s = w.sessions.get_mut(&7).unwrap();
    s.jobs = "1 shell".to_owned();
    let on = light_title(s, Light::Mode, None, false);
    assert_eq!(on.text.whole, "bypass permissions on \u{b7} no prompts");
    assert_eq!(on.text.short, "bypass permissions on");
    assert!(!on.text.reason, "nothing a restart would end");
    assert_eq!(on.spoken, "Bypass permissions on. No prompts.");
    let offered = light_title(s, Light::Mode, None, true);
    assert_eq!(
        offered.text.whole,
        "bypass permissions on \u{b7} click: settings default \u{b7} ends 1 shell",
        "CONTROL: a host that restarts"
    );
    assert_eq!(
        offered.text.short,
        "bypass permissions on \u{b7} ends 1 shell"
    );
    assert!(offered.text.reason);
    assert_eq!(
        offered.spoken,
        "Bypass permissions on. Click to leave yolo for the settings' default mode. Ends 1 shell."
    );
}

/// A WORDED CHIP NEVER RUNS INTO THE NEXT: after `⊘ off` the rule threads between the chips
/// (` ─ `, the rule's own cell once laid in the rule), so `⊘ off ─ yolo` never reads as one
/// phrase, nor does a click's mode name (`⤓ land ─ yolo`); the block's width counts it (the
/// name's cells apart, [`Block::word`]) and no chip's hit takes it. CONTROL: a bare glyph
/// (`∞`, `⤓`, `○`) stays one blank from the next chip.
#[test]
fn a_worded_chip_never_runs_into_the_next() {
    let now = Instant::now();
    let bare = "\u{2500}".repeat(60);
    let text = |cells: &[RenderCell]| -> String { cells.iter().map(|c| c.ch).collect() };
    for (view, named, lead) in [
        (AutopilotView::MasterOff, false, "\u{2298} off \u{2500} "),
        (AutopilotView::Off, false, "\u{25cb} "),
        (AutopilotView::Land, false, "\u{2913} "),
        (AutopilotView::Land, true, "\u{2913} land \u{2500} "),
        (AutopilotView::Cruise, false, "\u{221e} "),
    ] {
        let mut w = WindowLights::default();
        observe(&mut w, &screen(&bare, PLAN, ""));
        w.note_autopilot(7, view.mode(), view, now);
        if named {
            w.sessions.get_mut(&7).expect("noted").word = Some(ChipWord {
                mode: Autopilot::Land,
                from: now,
                fades: true,
                asked: 0,
            });
        }
        let block = w.block(7, blank(), now, ChipAt::cruise(blank()), Keep::Drawn);
        assert_eq!(text(&block.lights), format!("{lead}yolo"), "{view:?}");
        let gaps: Vec<usize> = block.rule_gaps().collect();
        let worded = lead.contains('\u{2500}');
        assert_eq!(
            gaps,
            if worded {
                vec![lead.chars().count() - 2]
            } else {
                Vec::new()
            },
            "{view:?}"
        );
        assert!(
            gaps.iter()
                .all(|g| !block.cols.iter().any(|(cols, _)| cols.contains(g))),
            "{view:?}: no chip's hit takes the rule"
        );
        let s = &w.sessions[&7];
        let inks = Inks::of(blank(), ChipAt::cruise(blank()));
        let drawn = drawn_at(s, Keep::Drawn);
        assert_eq!(
            chips_width_at(
                s,
                Chip::row().filter(|c| drawn(*c)),
                Keep::Drawn,
                inks,
                None
            ) + block.word,
            block.lights.len(),
            "{view:?}: the fit counts it, the name apart"
        );
    }
}

/// THE MODE'S CHIP KEEPS ONE BLANK BEFORE THE TEAM CHIP: the rule sets off the Autopilot
/// chip's word alone, where `⊘ off yolo` would misread; after the mode's word the Team chip's
/// own glyph sets it off (`∞ yolo ⇠ lab-sweep`), as before the rule was drawn, and the fit
/// counts what is drawn. CONTROL: a harness held off keeps the rule before the mode.
#[test]
fn the_mode_chip_keeps_one_blank_before_the_team_chip() {
    let now = Instant::now();
    let bare = "\u{2500}".repeat(60);
    let text = |cells: &[RenderCell]| -> String { cells.iter().map(|c| c.ch).collect() };
    let mut w = WindowLights::default();
    observe(&mut w, &screen(&bare, PLAN, ""));
    w.sessions.get_mut(&7).unwrap().team = Some(TeamFace::Worker {
        manager: Some(3),
        label: std::sync::Arc::from("lab-sweep"),
        task: None,
    });
    for (view, want, gaps) in [
        (AutopilotView::Cruise, "\u{221e} yolo \u{21e0} lab-sweep", 0),
        (
            AutopilotView::MasterOff,
            "\u{2298} off \u{2500} yolo \u{21e0} lab-sweep",
            1,
        ),
    ] {
        w.note_autopilot(7, view.mode(), view, now);
        let block = w.block(7, blank(), now, ChipAt::cruise(blank()), Keep::Drawn);
        assert_eq!(text(&block.lights), want, "{view:?}");
        assert_eq!(block.rule_gaps().count(), gaps, "{view:?}: the rule cells");
        let s = &w.sessions[&7];
        let drawn = drawn_at(s, Keep::Drawn);
        assert_eq!(
            chips_width_at(
                s,
                Chip::row().filter(|c| drawn(*c)),
                Keep::Drawn,
                Inks::of(blank(), ChipAt::cruise(blank())),
                None,
            ),
            block.lights.len(),
            "{view:?}: the fit counts it"
        );
    }
}

/// THE AUTOPILOT CHIP SAYS ITS VIEW IN ITS INK: under Cruise and Land the chip wears the
/// letter of its own column in its view's palette, Off and the harness held off the quiet
/// ink; no view reads the system accent or Claude's pill amber. CONTROL: Land's letter at the
/// chip's column is not Cruise's, so the palette, not a fixed ink, decides.
#[test]
fn the_autopilot_chip_says_its_view_in_its_ink() {
    use aterm_effects::harness_card::CardPalette;
    let now = Instant::now();
    let mut w = WindowLights::default();
    let (cruise, land, off, none) = (
        ChipAt::cruise(blank()),
        ChipAt::land(blank()),
        ChipAt::test(Some(CardPalette::Off), blank()),
        ChipAt::test(None, blank()),
    );
    let rows = [
        (
            AutopilotView::Cruise,
            cruise,
            "\u{221e}",
            cruise.letter_at(0),
            AutopilotView::Cruise.title().to_string(),
            "autopilot: cruise",
        ),
        (
            AutopilotView::Land,
            land,
            "\u{2913}",
            land.letter_at(0),
            AutopilotView::Land.title().to_string(),
            "autopilot: land",
        ),
        (
            AutopilotView::Off,
            off,
            "\u{25cb}",
            off.quiet(),
            AutopilotView::Off.title().to_string(),
            "autopilot: off",
        ),
        (
            AutopilotView::MasterOff,
            none,
            "\u{2298} off",
            none.quiet(),
            "harness off in Settings \u{b7} click: turn the harness on".to_owned(),
            "harness off in Settings",
        ),
    ];
    let text = |cells: &[RenderCell]| -> String { cells.iter().map(|c| c.ch).collect() };
    for (view, at, face, ink, title, short) in rows {
        w.note_autopilot(7, view.mode(), view, now);
        w.hover = None;
        let rest = w.block(7, blank(), now, at, Keep::Drawn);
        w.hover = Some((7, Target::from(Chip::Autopilot)));
        let block = w.block(7, blank(), now, at, Keep::Drawn);
        let n = face.chars().count();
        assert_eq!(text(&block.lights), face, "{view:?}");
        assert_eq!(block.cols[0], (0..n, Chip::Autopilot), "{view:?}");
        assert_eq!(
            text(&block.lights[..n]),
            view.label(),
            "{view:?}: its label"
        );
        assert!(
            rest.lights[..n]
                .iter()
                .all(|c| c.fg == ink && !c.bold && c.underline == UnderlineStyle::None),
            "{view:?}: its ink at rest"
        );
        // Under the pointer the one hover, whatever the view's ink: lit over a lit line.
        let lit = at.lit();
        assert!(
            block.lights[..n].iter().all(|c| {
                c.fg == lit
                    && !c.bold
                    && (c.underline, c.underline_color) == (UnderlineStyle::Single, Some(lit))
            }),
            "{view:?}: lit under the pointer"
        );
        assert_eq!(text(&block.title), format!("{title}  "), "{view:?}");
        assert_eq!(text(&block.short_title), format!("{short}  "), "{view:?}");
        assert!(!block.reason, "a hover's title gives way first");
    }
    assert_ne!(
        land.letter_at(0),
        cruise.letter_at(0),
        "CONTROL: the view's palette decides the letter"
    );

    w.hover = None;
    w.note_autopilot(7, None, AutopilotView::Cruise, now);
    w.sessions.get_mut(&7).unwrap().autopilot_pending =
        Some((Autopilot::Land, now + Duration::from_secs(1)));
    let block = w.block(7, blank(), now, ChipAt::cruise(blank()), Keep::Drawn);
    assert_eq!(text(&block.lights), "\u{25cc}", "in flight");
    w.note_autopilot(7, None, AutopilotView::Cruise, now);
    assert!(
        w.sessions[&7].autopilot_pending.is_some(),
        "not read back yet"
    );
    w.note_autopilot(7, Some(Autopilot::Land), AutopilotView::Land, now);
    assert!(w.sessions[&7].autopilot_pending.is_none(), "read back");
    assert_eq!(
        text(
            &w.block(7, blank(), now, ChipAt::cruise(blank()), Keep::Drawn)
                .lights
        ),
        "\u{2913}"
    );
    w.sessions.get_mut(&7).unwrap().autopilot_pending =
        Some((Autopilot::Off, now + Duration::from_secs(1)));
    assert!(!w.expire(now), "still in time");
    assert!(w.expire(now + Duration::from_secs(1)));
    assert!(w.sessions[&7].autopilot_pending.is_none());
    assert_eq!(
        w.fingerprint(|| now),
        0,
        "the lights' own key stays 0 at rest"
    );
}

/// A CHIP'S TITLE READS AS A VALUE, THE OFF CHIPS AS QUIET WORDS: on four built-in grounds
/// every title cell (whole, lean and short, a refusal's too) wears the glass's value ink, at
/// 7:1 or more, and `○` the quiet ink, at 4.5:1 and quieter than the value. CONTROL: the ink
/// the titles and the off chips wore before, the pane's halfway to its ground floored at 3:1
/// alone, misses 4.5:1 on some ground.
#[test]
fn a_chips_title_reads_as_a_value_and_the_off_chips_quietly() {
    use crate::chrome_band::contrast;
    let now = Instant::now();
    let rgb = |c: aterm_types::Rgb| [c.r, c.g, c.b];
    let mut old_misses = 0;
    for name in ["Default", "Aterm Night", "Aterm Day", "Linen"] {
        let scheme = aterm_types::scheme::builtin(name).expect("a built-in theme");
        let blank = RenderCell {
            ch: ' ',
            fg: rgb(scheme.foreground),
            bg: rgb(scheme.background),
            ..RenderCell::default()
        };
        let bg = blank.bg;
        let at = ChipAt::cruise(blank);
        let ink = at.value();
        let in_ink = |cells: &[RenderCell]| cells.iter().all(|c| c.fg == ink);
        let mut w = WindowLights::default();
        observe(&mut w, &screen(&"\u{2500}".repeat(60), PLAN, ""));
        w.note_autopilot(7, None, AutopilotView::Cruise, now);
        w.hover = Some((7, Target::from(Chip::Autopilot)));
        let block = w.block(7, blank, now, at, Keep::Drawn);
        assert!(
            !block.title.is_empty() && !block.short_title.is_empty(),
            "{name}: a title"
        );
        assert!(!block.lean[2].is_empty(), "{name}: the click, a lean form");
        assert!(
            in_ink(&block.title)
                && in_ink(&block.short_title)
                && block.lean.iter().all(|form| in_ink(form)),
            "{name}: every title cell in the value ink"
        );
        w.hover = None;
        w.sessions.get_mut(&7).unwrap().refused = Some((
            Light::Mode,
            Refusal::Click(ClickRefusal::Claimed),
            now + REFUSAL_SHOWN,
        ));
        let refused = w.block(7, blank, now, at, Keep::Drawn);
        assert!(refused.reason && !refused.title.is_empty(), "{name}");
        assert!(in_ink(&refused.title), "{name}: the refusal's ink");
        let halfway =
            std::array::from_fn(|k| ((u16::from(blank.fg[k]) + u16::from(blank.bg[k])) / 2) as u8);
        let (title, quiet, old) = (
            contrast(ink, bg),
            contrast(at.quiet(), bg),
            contrast(crate::claude_footer::visible_on(halfway, blank.bg), bg),
        );
        eprintln!("{name}: title {ink:?} {title:.2}:1, quiet {quiet:.2}:1, old {old:.2}:1");
        assert!(
            title >= f64::from(aterm_effects::glass_inks::VALUE_CONTRAST) - 0.01,
            "{name}: a value reads at 7:1 ({title:.2})"
        );
        assert!(quiet >= 4.5, "{name}: the off chips read ({quiet:.2})");
        assert!(
            quiet < title,
            "{name}: and stay quiet ({quiet:.2} < {title:.2})"
        );
        old_misses += usize::from(old < 4.5);
        w.note_autopilot(7, Some(Autopilot::Off), AutopilotView::Off, now);
        let off = w.block(7, blank, now, at, Keep::Drawn);
        assert_eq!(off.lights[0].fg, at.quiet(), "{name}: `○`, the quiet ink");
    }
    assert!(
        old_misses > 0,
        "CONTROL: the old halfway ink reads 4.5:1 everywhere"
    );
}

#[test]
fn the_team_chip_is_drawn_for_a_team_and_given_up_before_autopilot() {
    let now = Instant::now();
    let bare = "\u{2500}".repeat(60);
    let worker = TeamFace::Worker {
        manager: Some(3),
        label: std::sync::Arc::from("lab-sweep"),
        task: None,
    };
    let manager = TeamFace::Manager {
        mark: std::sync::Arc::from("\u{21c9}3"),
        workers: 3,
    };
    let chips = |w: &WindowLights, room: usize| -> Vec<Chip> {
        w.fitted(7, blank(), now, ChipAt::cruise(blank()), None, None, room)
            .map(|b| b.cols.iter().map(|(_, chip)| *chip).collect())
            .unwrap_or_default()
    };
    let mut w = WindowLights::default();
    observe(&mut w, &screen(&bare, BYPASS, ""));
    assert!(chips(&w, 40).is_empty(), "control: no team, no chip");
    w.sessions.get_mut(&7).unwrap().team = Some(manager.clone());
    assert_eq!(chips(&w, 40), [Chip::Team]);
    let block = w
        .fitted(7, blank(), now, ChipAt::cruise(blank()), None, None, 40)
        .unwrap();
    assert_eq!(
        block.lights.iter().map(|c| c.ch).collect::<String>(),
        "\u{21c9}3",
        "the manager's count"
    );
    w.sessions.get_mut(&7).unwrap().team = Some(TeamFace::Yours);
    let block = w
        .fitted(7, blank(), now, ChipAt::cruise(blank()), None, None, 40)
        .unwrap();
    assert!(block.lights[0].ch == '\u{270b}' && block.lights[1].wide);
    assert_eq!(block.lights.len(), cells_of("\u{270b} you"));
    observe(&mut w, &screen(&bare, PLAN, ""));
    w.note_autopilot(7, None, AutopilotView::Cruise, now);
    for face in [worker, manager, TeamFace::Yours] {
        w.sessions.get_mut(&7).unwrap().team = Some(face.clone());
        let mut wider = chips(&w, 40);
        assert_eq!(
            wider,
            [Chip::Autopilot, MODE, Chip::Team],
            "{face:?}: every chip, Team right of the light"
        );
        let mut gave_up: Vec<Chip> = Vec::new();
        for room in (0..40).rev() {
            let narrower = chips(&w, room);
            assert!(narrower.iter().all(|chip| wider.contains(chip)));
            gave_up.extend(wider.iter().filter(|chip| !narrower.contains(chip)));
            wider = narrower;
        }
        assert_eq!(
            gave_up,
            [Chip::Team, Chip::Autopilot, MODE],
            "{face:?}: Team first, then Autopilot and the light together, which every rung \
             keeps"
        );
    }
}

#[test]
fn a_team_chip_whose_press_does_nothing_promises_no_act() {
    let now = Instant::now();
    let worker = |manager: Option<u64>| TeamFace::Worker {
        manager,
        label: std::sync::Arc::from("lab-sweep"),
        task: Some(("working", now)),
    };
    let manager = TeamFace::Manager {
        mark: std::sync::Arc::from("\u{21c9}3"),
        workers: 3,
    };
    let faces = [
        (TeamFace::Yours, Some(TeamAct::HandBack)),
        (worker(Some(3)), Some(TeamAct::GoTo(3))),
        (worker(None), None),
        (manager, Some(TeamAct::Overview)),
    ];
    for (face, act) in faces {
        assert_eq!(face.act(), act, "{face:?}");
        let (whole, short) = team_title(Some(&face), now);
        assert!(!short.is_empty(), "{face:?}: it says what it is");
        assert_eq!(whole == short, act.is_none(), "{face:?}: {whole:?}");
        #[cfg(a11y_tree)]
        {
            let (label, description) = team_spoken(&face);
            assert!(
                label
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == ' ' || c == '-'),
                "{face:?}: words, no glyph: {label:?}"
            );
            assert_eq!(description.is_some(), act.is_some(), "{face:?}");
        }
    }
    assert_eq!(team_title(None, now), (String::new(), String::new()));
}

/// A NARROW RULE GIVES UP TEAM, NEVER THE LIGHT (`docs/DESIGN-yolo-binary-2026-11-04.md`
/// §2): `yolo`, on or off, keeps its place at every rung beside Autopilot, the only way
/// to yolo on a plain line, so a rule too narrow for the two draws no chip; each rung's fit counts what
/// it draws, and the chips drawn only go as the room does. CONTROL: Team is given up first.
#[test]
fn a_narrow_rule_gives_up_team_and_never_the_light() {
    let now = Instant::now();
    let bare = "\u{2500}".repeat(60);
    let mut w = WindowLights::default();
    observe(&mut w, &screen(&bare, PLAN, ""));
    w.note_autopilot(7, None, AutopilotView::Cruise, now);
    w.sessions.get_mut(&7).unwrap().team = Some(TeamFace::Worker {
        manager: Some(3),
        label: std::sync::Arc::from("lab-sweep"),
        task: None,
    });
    let fitted = |w: &WindowLights, selected: Option<Chip>, room: usize| {
        w.fitted(
            7,
            blank(),
            now,
            ChipAt::cruise(blank()),
            selected,
            None,
            room,
        )
        .map(|b| b.lights.iter().map(|c| c.ch).collect::<String>())
    };
    let team = "\u{221e} yolo \u{21e0} lab-sweep";
    assert_eq!(
        fitted(&w, None, cells_of(team)).as_deref(),
        Some(team),
        "CONTROL: every chip"
    );
    let two = cells_of("\u{221e} yolo");
    for room in two..cells_of(team) {
        assert_eq!(
            fitted(&w, None, room).as_deref(),
            Some("\u{221e} yolo"),
            "{room}: Team first"
        );
    }
    for room in 0..two {
        assert_eq!(
            fitted(&w, None, room),
            None,
            "{room}: never the light alone"
        );
    }
    observe(&mut w, &screen(&bare, BYPASS, ""));
    assert_eq!(
        fitted(&w, Some(MODE), two).as_deref(),
        Some("\u{221e} yolo")
    );
    let drawn = |w: &WindowLights, selected: Option<Chip>, room: usize| -> Vec<Chip> {
        w.fitted(
            7,
            blank(),
            now,
            ChipAt::cruise(blank()),
            selected,
            None,
            room,
        )
        .map(|b| b.cols.iter().map(|(_, chip)| *chip).collect())
        .unwrap_or_default()
    };
    for view in [AutopilotView::Cruise, AutopilotView::Land] {
        w.note_autopilot(7, view.mode(), view, now);
        for mode_row in [PLAN, ACCEPT, MANUAL, BYPASS] {
            observe(&mut w, &screen(&bare, mode_row, ""));
            for selected in [None, Some(Chip::Autopilot), Some(MODE)] {
                let mut wider = drawn(&w, selected, 24);
                assert!(
                    wider.contains(&Chip::Autopilot) && wider.contains(&MODE),
                    "{view:?} {mode_row:?} {selected:?}: control, every chip in 24 cells"
                );
                for room in (0..24).rev() {
                    let narrower = drawn(&w, selected, room);
                    assert!(
                        narrower.iter().all(|chip| wider.contains(chip))
                            && (narrower.is_empty() || narrower.contains(&MODE)),
                        "{view:?} {mode_row:?} {selected:?} {room}: {narrower:?} after \
                         {wider:?}"
                    );
                    wider = narrower;
                }
                assert!(wider.is_empty(), "control: no chip in no room");
                let s = &w.sessions[&7];
                for keep in Keep::LADDER {
                    let block = w.block_with(
                        7,
                        blank(),
                        now,
                        ChipAt::cruise(blank()),
                        selected,
                        None,
                        keep,
                        None,
                    );
                    let on = drawn_at(s, keep);
                    assert!(on(MODE), "{view:?} {mode_row:?} {keep:?}: the light kept");
                    assert_eq!(
                        chips_width_at(
                            s,
                            Chip::row().filter(|chip| on(*chip)),
                            keep,
                            Inks::of(blank(), ChipAt::cruise(blank())),
                            None,
                        ),
                        block.lights.len(),
                        "{view:?} {mode_row:?} {selected:?} {keep:?}"
                    );
                }
            }
        }
    }
}

/// A HOVER THAT SAYS WHAT A CLICK ENDS IS A REASON (`docs/DESIGN-yolo-binary-2026-11-04.md`
/// §2, Appendix "Chip"): with Claude's shells running, the light's title, off or on, adds
/// `· ends 1 shell` to its whole and keeps it in its short form, a reason, which
/// `footer::fit_rule` keeps in a rule where a plain hover's title gives way to the facts.
/// CONTROL: with no job running the short form is the head alone, and no reason. Unix only, as
/// the click's restart is (`permission_click::RESTARTS`): elsewhere no title names a click or
/// what it ends (`the_light_offers_no_click_on_a_host_with_no_restart`).
#[cfg(unix)]
#[test]
fn a_hover_that_says_what_a_click_ends_keeps_it_in_its_short_form() {
    use aterm_agent::harness::footer::{self, FooterFacts, LightsWidth, TitleFit};
    let now = Instant::now();
    let at = ChipAt::cruise(blank());
    let text = |cells: &[RenderCell]| -> String { cells.iter().map(|c| c.ch).collect() };
    let facts = FooterFacts {
        model: Some("Opus 5.5".into()),
        effort: Some("xhigh".into()),
        path: Some("~/aterm".into()),
        branch: Some("main".into()),
        ..FooterFacts::default()
    };
    for (mode, head) in [(BYPASS, "bypass permissions on"), (PLAN, "plan mode on")] {
        let on = mode == BYPASS;
        let mut w = WindowLights::default();
        observe(&mut w, &screen(&"\u{2500}".repeat(60), mode, ""));
        w.note_autopilot(7, None, AutopilotView::Cruise, now);
        w.hover = Some((7, Target::from(MODE)));
        let mut titled = Vec::new();
        for jobs in ["", "1 shell"] {
            w.sessions.get_mut(&7).unwrap().jobs = jobs.to_owned();
            let block = w.block(7, blank(), now, at, Keep::Drawn);
            let ends = if jobs.is_empty() {
                String::new()
            } else {
                format!(" \u{b7} ends {jobs}")
            };
            let does = if on {
                " \u{b7} click: settings default"
            } else {
                " \u{b7} click: yolo"
            };
            assert_eq!(
                text(&block.title),
                format!("{head}{does}{ends}  "),
                "{mode:?} {jobs:?}"
            );
            assert_eq!(
                text(&block.short_title),
                format!("{head}{ends}  "),
                "{mode:?} {jobs:?}"
            );
            assert_eq!(block.reason, !jobs.is_empty(), "{mode:?} {jobs:?}");
            let widths = LightsWidth {
                chips: block.holds,
                title: block.title.len(),
                lean: block.lean.each_ref().map(Vec::len),
                short: block.short_title.len(),
                reason: block.reason,
            };
            titled.push(
                (0..200)
                    .filter(|room| {
                        footer::fit_rule(&facts, *room, Some(widths)).title != TitleFit::None
                    })
                    .collect::<std::collections::BTreeSet<_>>(),
            );
        }
        let (plain, ends) = (&titled[0], &titled[1]);
        assert!(
            ends.difference(plain).next().is_some(),
            "{mode:?}: some rule keeps the reason's title where a plain hover's gives way"
        );
    }
}

#[test]
fn a_quiet_row_keys_as_zero() {
    let now = Instant::now();
    let mut w = WindowLights::default();
    assert_eq!(w.fingerprint(|| now), 0);
    w.hover = Some((7, Target::from(MODE)));
    let a = w.fingerprint(|| now);
    w.selected = Some((7, Target::from(MODE)));
    assert_ne!(a, 0);
    assert_ne!(a, w.fingerprint(|| now));
}
