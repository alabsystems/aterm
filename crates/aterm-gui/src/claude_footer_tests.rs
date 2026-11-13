// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// An unconfigured roster reads as it is, without the harness's state, so a default-home
/// footer keeps its own label; a configured one is only one bound to its logins.
#[test]
fn an_unconfigured_roster_reads_without_the_harness_state() {
    let dir = aterm_tempfile::TempDir::new().unwrap();
    let unasked = || -> Option<PathBuf> { panic!("an unconfigured roster asks for no state") };
    assert_eq!(
        roster_at(&dir.path().join("absent.toml"), unasked),
        Some(accounts::Roster::default())
    );
    let configured = dir.path().join(accounts::ACCOUNTS_FILE);
    std::fs::write(
        &configured,
        "[[account]]\nlabel = \"main\"\nclass = \"dedicated\"\n",
    )
    .unwrap();
    assert_eq!(
        accounts::Roster::load(&configured).map(|r| r.rows.len()),
        Ok(1),
        "configured"
    );
    assert!(
        roster_at(&configured, || None).is_none(),
        "no state to bind it in"
    );
}

#[test]
fn the_switch_and_the_label_give_words_up_in_order() {
    use crate::autopilot::{HarnessSwitch, SwitchOff};
    use aterm_phase::CliHarness;
    let paint = |face| SwitchPaint::bare(face, Some(CliHarness::ClaudeCode));
    let face = |p: SwitchPaint| switch_face(p).text();
    assert_eq!(face(paint(HarnessSwitch::On)), " \u{25cf} on ");
    for off in [
        SwitchOff::Master,
        SwitchOff::Agent(aterm_phase::Program::Claude),
    ] {
        assert_eq!(
            face(paint(HarnessSwitch::Off(off))),
            " \u{25cb} off ",
            "{off:?}"
        );
    }
    assert_eq!(
        face(paint(HarnessSwitch::Turning)),
        " \u{25cc} on ",
        "turning on: the side it turns to"
    );
    assert_eq!(face(paint(HarnessSwitch::TurningOff)), " \u{25cc} off ");
    assert_eq!(
        face(SwitchPaint {
            forced: true,
            ..paint(HarnessSwitch::On)
        }),
        "[\u{25cf} on]",
        "High Contrast brackets the switch"
    );
    for state in [
        HarnessSwitch::On,
        HarnessSwitch::Off(SwitchOff::Master),
        HarnessSwitch::Turning,
        HarnessSwitch::TurningOff,
    ] {
        for forced in [false, true] {
            let f = switch_face(SwitchPaint {
                forced,
                ..paint(state)
            });
            assert!(
                f.len <= SWITCH_SLOT,
                "{state:?}: {:?} in its slot of {SWITCH_SLOT}",
                f.text()
            );
            assert_eq!(
                f.text().contains("off"),
                f.len == SWITCH_SLOT,
                "{state:?}: `off` fills the slot, `on` a cell short"
            );
        }
    }
    let labels = |p: SwitchPaint| -> Vec<String> {
        (0..LABEL_TIERS)
            .filter_map(|t| label_face(&p, t))
            .map(|f| f.text())
            .collect()
    };
    let ver = CliHarness::ClaudeCode.version_str();
    assert_eq!(
        ver.split('.').count(),
        2,
        "the harness's version has no third digit"
    );
    let known = SwitchPaint {
        cli_ver: Inline::new("2.1.305"),
        ..paint(HarnessSwitch::On)
    };
    assert_eq!(
        labels(known),
        [format!("alab v{ver} (v2.1.305)"), format!("alab v{ver}")],
        "the lockup names the CLI: the label never does"
    );
    assert_eq!(
        labels(paint(HarnessSwitch::On)),
        [format!("alab v{ver}")],
        "an unknown CLI version is left out, never `(v?)`"
    );
    for (harness, raw, core) in [
        (CliHarness::Muse, "1.4.13-R6996.1", "1.4.13"),
        (CliHarness::Agy, "v1.4.0", "1.4.0"),
    ] {
        let p = SwitchPaint {
            cli_ver: Inline::new(raw),
            ..SwitchPaint::bare(HarnessSwitch::On, Some(harness))
        };
        let wide = label_face(&p, 0).expect("the widest").text();
        assert_eq!(
            wide,
            format!("alab v{} (v{core})", harness.version_str()),
            "{harness:?}: the numbers alone"
        );
        let bare = raw.strip_prefix('v').unwrap_or(raw);
        let whole = label_whole(&p, 0).map(|w| LabelFace::of_words(&w[0]).text());
        let want = (bare != core).then(|| format!("alab v{} (v{bare})", harness.version_str()));
        assert_eq!(whole, want, "{harness:?}: the hover says the whole build");
    }
    assert_eq!(
        label_face(&SwitchPaint::bare(HarnessSwitch::On, None), 0),
        None,
        "CONTROL: no harness, no label"
    );
    assert_eq!(
        label_whole(&known, 0),
        None,
        "Claude Code's build is its numbers: the label already says it"
    );
    assert_eq!(
        label_whole(&known, LABEL_TIERS - 1).map(|forms| forms
            .iter()
            .map(|f| LabelFace::of_words(f).text())
            .collect()),
        Some(vec!["claude v2.1.305".to_owned(), "v2.1.305".to_owned()]),
        "the last tier lays none of the build: the hover says it"
    );
    let v = LabelFace::of_words(&[("v0.5".to_owned(), LabelInk::Lit)]);
    assert_eq!(
        v.clone().right_aligned(6).map(|f| f.text()).as_deref(),
        Some("  v0.5"),
        "right-aligned"
    );
    assert_eq!(v.right_aligned(3), None, "CONTROL: never cut");
    assert_eq!(
        LabelFace::of_words(&[("whole".to_owned(), LabelInk::Muted)]).right_aligned(LABEL_MAX + 1),
        None,
        "padding must fit the buffer too"
    );
}

/// A GREYED WORD READS AS WORDS DO: a hidden fact's stub and the ghost wear the glass's quiet
/// ink, at 4.5:1 or more on Default Light's page and on Default (dark), and quieter than the
/// value. CONTROL: Claude's rule grey (#888888) floored at 3:1 alone, as the stub once was,
/// read at 3.31:1 on the light page.
#[test]
fn a_greyed_word_reads_as_words_do() {
    use aterm_types::scheme::builtin;
    let gray = [0x88; 3];
    let rgb = |c: aterm_types::Rgb| [c.r, c.g, c.b];
    for name in ["Default Light", "Default"] {
        let scheme = builtin(name).expect("a built-in");
        let (fg, bg) = (rgb(scheme.foreground), rgb(scheme.background));
        let inks = GlassInks::of(None, pack_rgb(bg), pack_rgb(fg), pack_rgb(gray));
        let (stub, value) = (
            reads_on(rgb_of(inks.quiet()), bg),
            reads_on(rgb_of(inks.value()), bg),
        );
        assert!(
            stub >= aterm_effects::cli_marks::NAME_CONTRAST,
            "{name}: the stub at {stub:.2}:1"
        );
        assert!(
            stub < value,
            "{name}: quieter than the value ({stub:.2} < {value:.2})"
        );
    }
    let bg = rgb(builtin("Default Light").expect("a built-in").background);
    let old = reads_on(visible_on(gray, bg), bg);
    assert!(
        old < aterm_effects::cli_marks::NAME_CONTRAST,
        "CONTROL: the 3:1 floor at {old:.2}:1 on Default Light"
    );
}

#[test]
fn the_brand_letter_reads_on_paper() {
    use aterm_effects::color_math::relative_luminance;
    use aterm_effects::harness_card::CardPalette;
    use aterm_types::scheme::{Appearance, builtin, builtin_names};
    let contrast = |a: [u8; 3], b: [u8; 3]| {
        let l = |c: [u8; 3]| relative_luminance(u32::from_be_bytes([0, c[0], c[1], c[2]]));
        let (la, lb) = (l(a), l(b));
        (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
    };
    let rgb = |c: aterm_types::Rgb| [c.r, c.g, c.b];
    let px = |c: u32| [(c >> 16) as u8, (c >> 8) as u8, c as u8];
    let (mut light, mut washed) = (0, f32::MAX);
    for name in builtin_names() {
        let Some(scheme) = builtin(name) else {
            continue;
        };
        let blank = RenderCell {
            fg: rgb(scheme.foreground),
            bg: rgb(scheme.background),
            ..RenderCell::default()
        };
        for palette in [CardPalette::Cruise, CardPalette::Land, CardPalette::Off] {
            for stop in 0..255 {
                let t = stop as f32 / 254.0;
                let fg = brand_letter_fg(palette, t, blank);
                let what = format!("{name} {palette:?} at {t:.3}: {fg:?}");
                if scheme.appearance == Appearance::Light {
                    light += 1;
                    let k = contrast(fg, blank.bg);
                    assert!(k >= 4.5, "{what} at {k:.2}:1");
                    if palette == CardPalette::Off {
                        let spread = fg.iter().max().unwrap_or(&0) - fg.iter().min().unwrap_or(&0);
                        assert!(spread <= 32, "{what} is not a silver");
                    } else {
                        assert!(fg.contains(&0), "{what} is not a pure hue");
                    }
                    let c = aterm_effects::spectrum::SPECTRUM_LUT[stop];
                    washed = washed.min(contrast(mix(px(c), blank.fg, 0.25), blank.bg));
                } else {
                    assert_eq!(
                        fg,
                        mix(px(palette.letter(t, true)), blank.fg, 0.25),
                        "{what}: dark, the palette's letter lifted"
                    );
                }
            }
        }
    }
    assert!(light >= 3 * 7 * 255, "{light} light letters");
    assert!(washed < 2.0, "CONTROL: {washed:.2}");
}

#[test]
fn the_footer_is_drawn_for_every_name_the_host_supervises() {
    use crate::session_timeline::AgentPublication;
    for name in [
        "claude",
        "claude-code",
        "Claude",
        "CLAUDE-CODE",
        "agy",
        "AGY",
        "muse",
        "MUSE",
        "muse-code",
        "kimi",
        "KIMI",
        "kimi-alab",
        "codex",
        "node",
        "bun",
        "zsh",
        "sh",
        "claudette",
        "claude.exe",
        "",
    ] {
        let publication = AgentPublication {
            program: Some(name.to_string()),
            ..AgentPublication::default()
        };
        let host = crate::harness_host::agent_of(&publication).is_some_and(|p| match p {
            aterm_phase::Program::Claude
            | aterm_phase::Program::Agy
            | aterm_phase::Program::Muse
            | aterm_phase::Program::Kimi => true,
            aterm_phase::Program::Codex
            | aterm_phase::Program::Grok
            | aterm_phase::Program::Generic => false,
        });
        assert_eq!(
            drawn_for(Some(name)),
            host,
            "{name:?}: the footer and the host must agree"
        );
    }
    assert!(
        drawn_for(Some("claude-code"))
            && drawn_for(Some("agy"))
            && drawn_for(Some("muse"))
            && drawn_for(Some("kimi")),
        "every hosted argv0 gets the footer"
    );
    assert!(!drawn_for(Some("codex")) && !drawn_for(None));
}

#[test]
fn the_chips_keep_a_blank_cell_on_each_side_at_every_width() {
    let blank = cell(' ', [200, 200, 200]);
    let rule = cell('\u{2500}', [90, 90, 90]);
    let cruise = format!(
        "autopilot: cruise \u{2014} {} \u{b7} click: land  ",
        crate::autopilot::mode_words(aterm_agent::supervise::Autopilot::Cruise).chip
    );
    let (off_title, off_short) = ("plan mode on \u{b7} click: yolo  ", "plan mode on  ");
    for facts in [facts(), facts_with_usage(), FooterFacts::default()] {
        for chips in [1usize, 3, 6, 8, 10, 14] {
            for (title, short) in [
                ("", ""),
                (off_title, off_short),
                (cruise.as_str(), "autopilot: cruise  "),
            ] {
                let block = crate::claude_lights::Block::of_chips(
                    row_of(&"\u{25CB}".repeat(chips), [150, 150, 150]),
                    row_of(title, [150, 150, 150]),
                    row_of(short, [150, 150, 150]),
                    false,
                );
                let widths = footer::LightsWidth {
                    chips,
                    title: title.chars().count(),
                    lean: [0; footer::LEAN_FORMS],
                    short: short.chars().count(),
                    reason: false,
                };
                for room in 0usize..160 {
                    let fit = footer::fit_rule(&facts, room, Some(widths));
                    if !fit.chips {
                        assert!(
                            chips > footer::chips_room(&facts, room),
                            "{room}/{chips}: the chips fit, yet were not drawn"
                        );
                        continue;
                    }
                    let (cells, at) = compose_rule(&fit, Some(&block), rule, blank, None, room);
                    let at = at.expect("drawn chips have a column");
                    let row = text_of(&cells);
                    assert!(cells.len() <= room, "{room}: {row:?}");
                    assert_eq!(cells[at - 1].ch, ' ', "{room}/{chips} before: {row:?}");
                    assert_eq!(cells[at + chips].ch, ' ', "{room}/{chips} after: {row:?}");
                }
            }
        }
    }
}

fn watch_model_projection(schedule: &WatchSchedule) -> i64 {
    match schedule.watched.get(&7).map(|watch| watch.job.pgid) {
        None => 0,
        Some(42) => 1,
        Some(43) => 2,
        other => panic!("unexpected foreground group: {other:?}"),
    }
}

#[test]
fn stopped_footer_watch_has_no_followup_or_idle_reads() {
    let model = aterm_models_harness::claude_footer_watch_model();
    let mut state = model.init_state();
    let now = std::time::Instant::now();
    let timeline = Arc::new(Mutex::new(SessionTimeline::default()));
    let weak = Arc::downgrade(&timeline);
    let mut schedule = WatchSchedule::default();
    schedule.watch(
        Job {
            session: 7,
            timeline,
            pgid: 42,
            wake: LogWake::default(),
        },
        now,
        false,
    );
    assert!(model.fire("AskOld", &mut state));
    assert_eq!(state["watch"], watch_model_projection(&schedule));
    assert_eq!(
        model.action_enabled("IdleRead", &state),
        schedule.due_idle(now + IDLE_RECHECK).is_some()
    );
    assert!(schedule.watched[&7].awaiting);
    assert_eq!(schedule.next(now), Some(now + FOLLOW_UPS[0]));
    let Accepted {
        latest: immediate,
        stopped,
        ..
    } = schedule.accept([Command::Stop {
        session: 7,
        pgid: None,
    }]);
    assert!(model.fire("StopSession", &mut state));
    assert!(immediate.is_empty());
    assert_eq!(stopped, [7]);
    assert_eq!(state["watch"], watch_model_projection(&schedule));
    assert!(schedule.next(now).is_none());
    assert!(schedule.due_followups(now + FOLLOW_UPS[1]).is_empty());
    assert_eq!(
        model.action_enabled("IdleRead", &state),
        schedule.due_idle(now + IDLE_RECHECK).is_some()
    );
    assert!(weak.upgrade().is_none(), "the watch released its timeline");
}

#[test]
fn stale_group_stop_preserves_replacement_footer_watch() {
    let model = aterm_models_harness::claude_footer_watch_model();
    let mut state = model.init_state();
    let now = std::time::Instant::now();
    let timeline = Arc::new(Mutex::new(SessionTimeline::default()));
    let mut schedule = WatchSchedule::default();
    schedule.watch(
        Job {
            session: 7,
            timeline: Arc::clone(&timeline),
            pgid: 42,
            wake: LogWake::default(),
        },
        now,
        true,
    );
    assert!(model.fire("AskOld", &mut state));
    let Accepted {
        latest: mut immediate,
        stopped,
        ..
    } = schedule.accept([
        Command::Request(Job {
            session: 7,
            timeline,
            pgid: 43,
            wake: LogWake::default(),
        }),
        Command::Stop {
            session: 7,
            pgid: Some(42),
        },
    ]);
    assert!(model.fire("AskNew", &mut state));
    assert!(model.fire("StopOld", &mut state));
    assert_eq!(stopped, [7]);
    assert_eq!(immediate.len(), 1);
    let replacement = immediate.pop().unwrap();
    assert_eq!(replacement.pgid, 43);
    schedule.watch(replacement, now, true);
    assert_eq!(state["watch"], watch_model_projection(&schedule));
    let Accepted {
        latest: immediate,
        stopped,
        ..
    } = schedule.accept([Command::Stop {
        session: 7,
        pgid: Some(42),
    }]);
    assert!(immediate.is_empty());
    assert!(stopped.is_empty());
    let due = schedule
        .due_idle(now + IDLE_RECHECK)
        .expect("active Claude keeps its idle refresh");
    assert_eq!(due.pgid, 43);
    assert!(model.action_enabled("IdleRead", &state));
}

#[test]
fn a_watch_lapse_keeps_the_processs_pin() {
    use std::time::{Instant, UNIX_EPOCH};
    const START: u64 = 1_790_607_848; // 2026-09-28T15:04:08Z
    const S: &str = "00000000-0000-4000-8000-000000000c01";
    const A: &str = "00000000-0000-4000-8000-000000000c02";
    const B: &str = "00000000-0000-4000-8000-000000000c03";
    let root = super::rig::home_root("footer-lapse");
    let _ = std::fs::remove_dir_all(&root);
    let cwd = root.join("work");
    let claude = root.join("claude");
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::create_dir_all(claude.join("sessions")).unwrap();
    let project = claude.join("projects").join(footer::project_slug(&cwd));
    std::fs::create_dir_all(&project).unwrap();
    let register = |session: &str| {
        std::fs::write(
            claude.join("sessions/4242.json"),
            format!(
                r#"{{"pid":4242,"sessionId":"{session}","cwd":{},"procStart":"{}","startedAt":{},"version":"2.1.284"}}"#,
                aterm_json::to_string(&cwd.display().to_string()).unwrap(),
                footer::lstart_utc(START),
                START * 1000 + 684
            ),
        )
        .unwrap();
    };
    let answer = |session: &str, model: &str, at: &str| {
        std::fs::write(
            project.join(format!("{session}.jsonl")),
            format!(
                "{{\"type\":\"assistant\",\"timestamp\":\"{at}\",\"effort\":\"high\",\"message\":{{\"model\":\"{model}\"}}}}\n"
            ),
        )
        .unwrap();
    };
    answer(S, "claude-opus-5-5", "2026-09-28T15:05:00.000Z");
    answer(A, "claude-fable-5-1", "2026-09-27T10:00:00.000Z");
    answer(B, "claude-sonnet-5", "2026-09-26T10:00:00.000Z");
    let read = |tails: &mut ProcessTails, started: Option<u64>, secs: u64, now: Instant| {
        let entry = footer::session_of_pid(&claude, 4242, started).expect("the registry");
        footer::facts_for_entry_at(
            &claude,
            4242,
            started,
            &entry,
            &LaunchFacts::default(),
            tails.cache(7, 4242, started, now),
            UNIX_EPOCH + Duration::from_secs(START + secs),
        )
        .model
    };
    let fable = Some("Fable 5.1".to_owned());
    let run = |started: Option<u64>, how: &str, lives: bool| {
        let t0 = Instant::now();
        let mut schedule = WatchSchedule::default();
        schedule.watch(
            Job {
                session: 7,
                timeline: Arc::new(Mutex::new(SessionTimeline::default())),
                pgid: 4242,
                wake: LogWake::default(),
            },
            t0,
            true,
        );
        let mut tails = ProcessTails::default();
        register(S);
        assert_eq!(
            read(&mut tails, started, 120, t0).as_deref(),
            Some("Opus 5.5")
        );
        register(A);
        assert_eq!(read(&mut tails, started, 180, t0), fable, "restored");
        let later = t0 + WATCH_FOR;
        match how {
            "lapse" => schedule.prune(later, |_, _| false),
            "stop" => {
                let Accepted { stopped, .. } = schedule.accept([Command::Stop {
                    session: 7,
                    pgid: Some(4242),
                }]);
                assert_eq!(stopped, [7]);
            }
            other => panic!("{other}"),
        }
        assert!(schedule.watched.is_empty(), "the watch is gone");
        tails.retain(
            &schedule,
            |pgid, born| lives && (pgid, born) == (4242, START),
            later,
        );
        register(B);
        read(&mut tails, started, 900, later)
    };
    assert_eq!(
        run(Some(START), "lapse", true),
        fable,
        "the pin outlived the lapse"
    );
    assert_eq!(run(Some(START), "stop", true), fable, "and the stop");
    assert_eq!(run(Some(START), "lapse", false), None);
    assert_eq!(run(None, "lapse", true), None);
    let t0 = Instant::now();
    let mut tails = ProcessTails::default();
    tails.cache(7, 4242, Some(START), t0);
    for n in 0..MAX_KEPT {
        let n64 = u64::try_from(n).unwrap();
        tails.cache(
            8 + n64,
            5000 + i32::try_from(n).unwrap(),
            Some(1),
            t0 + Duration::from_secs(1 + n64),
        );
    }
    assert_eq!(tails.kept.len(), MAX_KEPT);
    assert!(!tails.kept.contains_key(&(4242, Some(START))));
    assert!(tails.kept.contains_key(&(5000, Some(1))));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn pane_text_reuses_only_the_same_scratch_revision_and_pane() {
    let mut scratch = RenderInput::empty();
    scratch.rows = 4;
    scratch.cols = 4;
    scratch.cells = vec![
        row_of("chrome", [0, 0, 0]),
        row_of("abcd", [0, 0, 0]),
        row_of("efgh", [0, 0, 0]),
        row_of("ijkl", [0, 0, 0]),
    ];
    scratch.snapshot_seq = 5;
    let mut cache = PaneTextCache::default();
    let full = PaneTextRegion {
        strip: 1,
        row_off: 0,
        col_off: 0,
        rows: 3,
        cols: 4,
    };
    let first = cache.rows(0, 7, &scratch, full);
    assert_eq!(&**first, &["abcd", "efgh", "ijkl"]);
    let again = cache.rows(0, 7, &scratch, full);
    assert!(Arc::ptr_eq(&first, &again));

    let other_pane = cache.rows(
        1,
        7,
        &scratch,
        PaneTextRegion {
            strip: 1,
            row_off: 1,
            col_off: 1,
            rows: 2,
            cols: 2,
        },
    );
    assert_eq!(&**other_pane, &["fg", "jk"]);
    assert!(!Arc::ptr_eq(&first, &other_pane));
    let other_session = cache.rows(0, 8, &scratch, full);
    assert_eq!(&**other_session, &**first);
    assert!(!Arc::ptr_eq(&first, &other_session));

    scratch.cells[1][0].ch = 'z';
    scratch.snapshot_seq += 1;
    let changed = cache.rows(0, 8, &scratch, full);
    assert_eq!(&changed[0], "zbcd");
    assert!(!Arc::ptr_eq(&other_session, &changed));
    cache.retain_active(|index| index == 1, |_| false, |_| true);
    assert!(cache.panes[0].is_none());
    assert!(cache.panes[1].is_some());
    cache.trim(1);
    assert_eq!(cache.panes.len(), 1);
    cache.trim(0);
    assert!(cache.panes.is_empty());
}

#[test]
fn changed_claude_frame_reuses_free_row_buffers_but_preserves_held_snapshots() {
    let mut scratch = RenderInput::empty();
    scratch.rows = 2;
    scratch.cols = 4;
    scratch.cells = vec![row_of("abcd", [0, 0, 0]), row_of("efgh", [0, 0, 0])];
    scratch.snapshot_seq = 1;
    let region = PaneTextRegion {
        strip: 0,
        row_off: 0,
        col_off: 0,
        rows: 2,
        cols: 4,
    };
    let mut cache = PaneTextCache::default();
    let first = cache.rows(0, 7, &scratch, region);
    let vec_ptr = first.as_ref().as_ptr();
    let row_ptrs = [first[0].as_ptr(), first[1].as_ptr()];
    drop(first);

    scratch.cells[0][0].ch = 'z';
    scratch.snapshot_seq += 1;
    let reused = cache.rows(0, 7, &scratch, region);
    assert_eq!(&**reused, &["zbcd", "efgh"]);
    assert_eq!(reused.as_ref().as_ptr(), vec_ptr);
    assert_eq!([reused[0].as_ptr(), reused[1].as_ptr()], row_ptrs);

    let held = Arc::clone(&reused);
    scratch.cells[0][1].ch = 'y';
    scratch.snapshot_seq += 1;
    let next = cache.rows(0, 7, &scratch, region);
    assert_eq!(&**held, &["zbcd", "efgh"]);
    assert_eq!(&**next, &["zycd", "efgh"]);
    assert!(!Arc::ptr_eq(&held, &next));

    let rebuild = |cache: &PaneTextCache| cache.panes[0].as_ref().unwrap().text_gen;
    let before = rebuild(&cache);
    scratch.cells[1][0].fg = [9, 9, 9];
    scratch.snapshot_seq += 1;
    let (same, allocations) =
        crate::work_counts::allocations_in(|| cache.rows(0, 7, &scratch, region));
    assert!(Arc::ptr_eq(&same, &next), "the held snapshot, shared");
    assert_eq!(allocations, 0);
    assert_ne!(rebuild(&cache), before, "a rebuild all the same");
}

fn vendor_frame(scratch: &mut RenderInput) {
    let bottom = scratch.rows - 1;
    let rule = "\u{2500}".repeat(scratch.cols);
    scratch.cells[bottom - 3] = row_of(&rule, [40, 40, 40]);
    scratch.cells[bottom - 2] = row_of("\u{276F} ", [40, 40, 40]);
    scratch.cells[bottom - 1] = row_of(&rule, [40, 40, 40]);
    scratch.cells[bottom] = row_of(
        "  \u{23F5}\u{23F5} bypass permissions on (shift+tab to cycle) \u{00B7} \u{2190} for agents",
        [90, 90, 90],
    );
    scratch.snapshot_seq = scratch.snapshot_seq.wrapping_add(1);
}

fn cell(ch: char, fg: [u8; 3]) -> RenderCell {
    RenderCell {
        ch,
        fg,
        ..RenderCell::default()
    }
}

fn row_of(text: &str, fg: [u8; 3]) -> Vec<RenderCell> {
    text.chars().map(|c| cell(c, fg)).collect()
}

fn text_of(row: &[RenderCell]) -> String {
    read_glass(row)
}

fn cells_for(reading: &str) -> usize {
    let segments: Vec<footer::Segment> = reading
        .trim()
        .split("   ")
        .map(|fact| {
            let (mark, text) = fact.split_once(' ').expect("<mark> <text>");
            footer::Segment::plain(mark.chars().next().expect("a mark"), text)
        })
        .collect();
    footer::segments_width(&segments) + 2
}

fn facts() -> FooterFacts {
    FooterFacts {
        model: Some("Opus 5.5".into()),
        effort: Some("xhigh".into()),
        path: Some("~/aterm".into()),
        branch: Some("main".into()),
        ..FooterFacts::default()
    }
}

#[test]
fn a_reader_panic_paints_no_footer_row() {
    let rows = vec![
        "\u{2500}".repeat(60),
        "\u{276F} ".to_string(),
        "\u{2500}".repeat(60),
        "  \u{23F5}\u{23F5} bypass permissions on (shift+tab to cycle)".to_string(),
    ];
    assert_eq!(
        read_footer(9, &rows, read_rule_row),
        Some(RuleRead {
            rule: 2,
            mode_row: true
        }),
        "the real reader finds it"
    );
    let boom = |_: &[String]| -> FooterRead { panic!("stand-in reader panic") };
    assert!(read_footer(9, &rows, boom).is_none());
    assert!(read_footer(9, &rows, boom).is_none(), "every frame");
}

#[test]
fn the_rule_carries_the_owners_footer() {
    let blank = cell(' ', [200, 200, 200]);
    let rule = cell('\u{2500}', [60, 60, 60]);
    let fit_plain = footer::fit_rule(&facts(), 76, None);
    let (plain_cells, lights) = compose_rule(&fit_plain, None, rule, blank, None, 76);
    assert_eq!(lights, None);
    assert_eq!(
        plain_cells.len(),
        footer::segments_width(&fit_plain.segments) + 2
    );
    assert_eq!(plain_cells[0], blank);
    assert_eq!(plain_cells[plain_cells.len() - 1], blank);
    let set = footer::set_facts(&fit_plain.segments);
    assert!(!set.glyphs.is_empty());
    for glyph in &set.glyphs {
        let painted = plain_cells[1 + glyph.cell];
        assert_eq!(painted.bold, glyph.bold, "{glyph:?}");
        assert_eq!(
            painted.script,
            aterm_core::terminal::Script::Normal,
            "{glyph:?}"
        );
    }
    let none = footer::fit_rule(&FooterFacts::default(), 76, None);
    assert!(
        compose_rule(&none, None, rule, blank, None, 76)
            .0
            .is_empty()
    );
    let chips_at = crate::claude_lights::ChipAt::cruise(blank);
    let harness = crate::claude_lights::Block::of_autopilot(
        crate::autopilot::AutopilotView::Cruise,
        blank,
        chips_at,
    );
    let widths = footer::LightsWidth {
        chips: harness.lights.len(),
        title: harness.title.len(),
        lean: [0; footer::LEAN_FORMS],
        short: harness.short_title.len(),
        reason: harness.reason,
    };
    let fit = footer::fit_rule(&facts(), 76, Some(widths));
    assert_eq!(fit.segments, fit_plain.segments);
    let (cells, lights) = compose_rule(&fit, Some(&harness), rule, blank, None, 76);
    assert_eq!(lights, Some(1));
    assert_eq!(cells.len(), 76, "the rule's glyphs run on to the facts");
    assert_eq!(cells[0], blank);
    assert_eq!(
        &cells[1..1 + harness.lights.len()],
        harness.lights.as_slice()
    );
    assert_eq!(cells[1 + harness.lights.len()], blank);
    assert!(
        cells[2 + harness.lights.len()..76 - plain_cells.len()]
            .iter()
            .all(|&c| c == rule),
        "the rule's glyphs bridge the lights and the facts"
    );
    assert_eq!(&cells[76 - plain_cells.len()..], plain_cells.as_slice());
    assert_eq!(cells[1].fg, chips_at.letter_at(0), "cruise in its letter");
    assert_eq!(cells[1].script, aterm_core::terminal::Script::Normal);
    // Under a palette the rule's lead, its run between the chips and the facts, and its close
    // are written too, each `─` in the arc of its column.
    let palette = Some(aterm_effects::harness_card::CardPalette::Cruise);
    let (arc, lights) = compose_rule(&fit, Some(&harness), rule, blank, palette, 76);
    let lead = footer::RULE_LEAD;
    assert_eq!(lights, Some(lead + 1), "the lead, then a blank");
    assert_eq!(arc.len(), lead + 76 + 1, "the lead, the room and the close");
    let (glass, at) = (rule_glass(palette, rule, blank), rule_at_room(76));
    let facts_at = 76 - plain_cells.len() + 1;
    let marks: std::collections::HashSet<usize> = set
        .glyphs
        .iter()
        .filter(|g| g.mark)
        .map(|g| facts_at + g.cell)
        .collect();
    assert!(!marks.is_empty(), "CONTROL: the facts have marks");
    for (i, want) in cells.iter().enumerate().filter(|(_, c)| **c != rule) {
        let want = if marks.contains(&i) {
            RenderCell {
                fg: rgb_of(glass.mark(at.t(lead + i))),
                ..*want
            }
        } else {
            *want
        };
        assert_eq!(
            arc[lead + i],
            want,
            "{i}: the chips and the facts after the lead, a mark in its column's marks ink"
        );
    }
    let bridge = lead + 2 + harness.lights.len()..lead + 76 - plain_cells.len();
    for i in (0..lead).chain(bridge).chain([arc.len() - 1]) {
        assert_eq!(arc[i].ch, '\u{2500}', "{i}: the rule's own");
        assert_eq!(
            arc[i].fg,
            rgb_of(glass.rule(at.t(i))),
            "{i}: its column's arc"
        );
    }
    assert_ne!(
        arc[0].fg,
        arc[arc.len() - 1].fg,
        "CONTROL: the arc runs across the rule"
    );
    // A run of the lead and the close or fewer cells has no room: the arc lays nothing, so
    // nothing is written past the run's ends. CONTROL: one cell of room lays exactly the run.
    let none = footer::fit_rule(&facts(), 0, None);
    assert!(
        compose_rule(&none, None, rule, blank, palette, 0)
            .0
            .is_empty(),
        "no room, no cells under a palette"
    );
    let one = footer::fit_rule(&facts(), 1, None);
    assert_eq!(
        compose_rule(&one, None, rule, blank, palette, 1).0.len(),
        lead + 1 + 1,
        "CONTROL: one cell of room lays the lead, it and the close"
    );
}

/// LINE D IS ONE SCRIPT AT ONE PITCH (glass design §1.2, §4.3, §6 S7): every glyph line D's
/// facts lay is Normal script and advances exactly its cell width, so the facts' cells read,
/// cell for cell, as their words: each mark one blank before its value, a part of the account
/// unit one blank after the part before it, a fact [`footer::GAP`] blanks after the one before
/// it, a wide char two cells, a blank on each side. Over the account unit, the wall and usage,
/// a hidden fact's stub and a wide branch, at every rung `fit_rule` gives from 0 to 200 cells,
/// under a palette and none. NEGATIVE CONTROLS: one value glyph in the old small script
/// (`Script::Sub`) fails the pin, and so do the same glyphs at the old 0.8 pitch (each at four
/// fifths of its cell).
#[test]
fn line_d_is_one_script_at_one_pitch() {
    use aterm_core::terminal::Script;
    let blank = cell(' ', [200, 200, 200]);
    let rule = cell('\u{2500}', [60, 60, 60]);
    // What `segments` read as at pitch 1, a wide char's right half its blank.
    let words = |segments: &[footer::Segment]| -> String {
        let mut out = String::from(" ");
        for (i, segment) in segments.iter().enumerate() {
            if i > 0 {
                let gap = if segment.attached { 1 } else { footer::GAP };
                out.extend(std::iter::repeat_n(' ', gap));
            }
            if let Some(mark) = segment.drawn_mark() {
                out.push(mark);
                out.push(' ');
            }
            for ch in segment.text.chars() {
                out.push(ch);
                if aterm_grapheme::char_width(ch) == 2 {
                    out.push(' ');
                }
            }
        }
        out.push(' ');
        out
    };
    // Whether `laid`, the facts' cells and their outer blanks, is `reading` in Normal script.
    let one_pitch = |laid: &[RenderCell], reading: &str| -> Result<(), String> {
        let read: String = laid.iter().map(|c| c.ch).collect();
        if read != reading {
            return Err(format!("{read:?}, not {reading:?}"));
        }
        laid.iter()
            .find(|c| c.ch != ' ' && c.script != Script::Normal)
            .map_or(Ok(()), |c| Err(format!("{:?} in {:?}", c.ch, c.script)))
    };
    let unit = FooterFacts {
        account: Some("main".into()),
        subscription: Some(aterm_agent::harness::profile::SubscriptionDisplay::default()),
        quota: Some(aterm_agent::harness::quota_view::QuotaView::sample()),
        ..facts()
    };
    let stub = FooterFacts {
        off: footer::FactMask::default().with(FactKind::Branch, true),
        ..facts()
    };
    let wide = FooterFacts {
        branch: Some("\u{6F22}\u{5B57}".into()),
        ..facts()
    };
    let corpus = [facts(), unit, facts_with_usage(), stub, wide];
    let palettes = [None, Some(aterm_effects::harness_card::CardPalette::Cruise)];
    let (mut checked, mut control) = (0usize, None);
    for (f, facts) in corpus.iter().enumerate() {
        for palette in palettes {
            let glass = rule_glass(palette, rule, blank);
            for room in 0..=200 {
                let fit = footer::fit_rule(facts, room, None);
                if fit.segments.is_empty() {
                    continue;
                }
                let place = (&glass, rule_at_room(room));
                let paint = compose_rule_spans(&fit, None, rule, blank, place, room, None);
                let reading = words(&fit.segments);
                let laid = paint
                    .cells
                    .get(paint.facts_at..paint.facts_at + reading.chars().count())
                    .unwrap_or_else(|| panic!("facts {f} at {room}: the facts are laid"));
                if let Err(why) = one_pitch(laid, &reading) {
                    panic!("facts {f} under {palette:?} at {room}: {why}");
                }
                control.get_or_insert_with(|| (laid.to_vec(), reading));
                checked += 1;
            }
        }
    }
    assert!(checked > 5 * 2 * 100, "{checked} lines");
    let (laid, reading) = control.expect("a line");
    let value = laid
        .iter()
        .position(|c| c.ch.is_ascii_alphanumeric())
        .expect("a value glyph");
    let mut small = laid.clone();
    small[value].script = Script::Sub { nudge: 0 };
    assert!(
        one_pitch(&small, &reading).is_err(),
        "NEGATIVE CONTROL: a value glyph in the old small script"
    );
    let mut squeezed = vec![blank; laid.len()];
    for (c, cell) in laid.iter().enumerate().filter(|(_, c)| c.ch != ' ') {
        squeezed[(c as f32 * 0.8).round() as usize] = *cell;
    }
    assert!(
        one_pitch(&squeezed, &reading).is_err(),
        "NEGATIVE CONTROL: the same glyphs at the old 0.8 pitch"
    );
}

#[test]
fn the_facts_are_painted_where_they_are_set() {
    let blank = cell(' ', [200, 200, 200]);
    let rule = cell('\u{2500}', [60, 60, 60]);
    let fit = footer::fit_rule(&facts(), 76, None);
    let (cells, _) = compose_rule(&fit, None, rule, blank, None, 76);
    let set = footer::set_facts(&fit.segments);
    assert_eq!(cells.len(), set.cells + 2);
    let mut glyph_cells = std::collections::HashSet::new();
    for glyph in &set.glyphs {
        let painted = cells[1 + glyph.cell];
        assert_eq!(painted.ch, glyph.ch);
        assert_eq!(
            painted.script,
            aterm_core::terminal::Script::Normal,
            "{glyph:?}"
        );
        glyph_cells.insert(1 + glyph.cell);
    }
    for (c, painted) in cells.iter().enumerate() {
        if !glyph_cells.contains(&c) {
            assert_eq!(*painted, blank, "cell {c} is blank");
        }
    }
    let reading = " \u{25C6} Opus 5.5 xhigh   \u{2302} ~/aterm   \u{2387} main ";
    assert_eq!(text_of(&cells), reading);
    assert_eq!(
        cells.len(),
        reading.chars().count(),
        "one cell per character"
    );
}

#[test]
fn a_lit_facts_underline_lies_under_its_ink() {
    use aterm_core::terminal::{Script, UnderlineStyle};
    let blank = cell(' ', [200, 200, 200]);
    let rule = cell('\u{2500}', [60, 60, 60]);
    let glass = rule_glass(None, rule, blank);
    let fit = footer::RuleFit {
        segments: vec![
            footer::Segment {
                tone: footer::Tone::Lit,
                ..footer::Segment::plain(footer::ACCOUNT_MARK, "main")
            },
            footer::Segment::attached(footer::ACCOUNT_MARK, "29% left"),
            footer::Segment {
                split: Some("Opus 5.5 ".len()),
                ..footer::Segment::plain(footer::MODEL_MARK, "Opus 5.5 xhigh")
            },
            footer::Segment::plain(footer::PATH_MARK, "/private/tmp/at.SnYH/proj"),
            footer::Segment::plain(footer::BRANCH_MARK, "main"),
        ],
        title: footer::TitleFit::None,
        chips: false,
    };
    let room = 120;
    let place = (&glass, rule_at_room(room));
    let rest = compose_rule_spans(&fit, None, rule, blank, place, room, None);
    assert!(
        rest.cells
            .iter()
            .all(|c| c.underline == UnderlineStyle::None),
        "CONTROL: nothing underlined at rest"
    );
    for kind in [
        FactKind::Account,
        FactKind::Model,
        FactKind::Effort,
        FactKind::Path,
        FactKind::Branch,
    ] {
        let paint = compose_rule_spans(&fit, None, rule, blank, place, room, Some(kind));
        let span = paint
            .spans
            .iter()
            .flatten()
            .find(|span| span.kind == kind)
            .copied()
            .expect("the fact is on the line");
        let at = paint.facts_at + 1;
        let own = at + usize::from(span.cells.0)..at + usize::from(span.cells.1);
        let mut drawn: Vec<(usize, usize, [u8; 3])> = Vec::new();
        for c in own.clone() {
            let cell = paint.cells[c];
            if cell.ch == ' ' {
                continue;
            }
            assert_eq!(cell.script, Script::Normal, "{kind:?}: cell {c}");
            let cols = if paint.cells.get(c + 1).is_some_and(|n| n.wide) {
                2
            } else {
                1
            };
            drawn.push((c, c + cols, cell.fg));
            // A lit-toned word (the account's name) already wears the lit ink at rest; with
            // no palette the hovered letter ink is that same ink, so it stays.
            assert!(
                cell.fg != rest.cells[c].fg || rest.cells[c].fg == rgb_of(glass.lit()),
                "{kind:?}: cell {c} is lifted"
            );
        }
        let left = drawn.iter().map(|d| d.0).min().expect("the fact's ink");
        let right = drawn.iter().map(|d| d.1).max().expect("the fact's ink");
        assert_eq!(left..right, own, "{kind:?}: its ink is its own cells");
        for (c, cell) in paint.cells.iter().enumerate() {
            let under = (left..right).contains(&c);
            assert_eq!(
                cell.underline == UnderlineStyle::Single,
                under,
                "{kind:?}: cell {c} ({:?}), the ink {left}..{right}",
                cell.ch
            );
            if under {
                let nearest = drawn
                    .iter()
                    .min_by_key(|g| (g.0 + g.1).abs_diff(2 * c + 1))
                    .map(|g| g.2);
                assert_eq!(cell.underline_color, nearest, "{kind:?}: cell {c}'s ink");
            }
            if !own.contains(&c) {
                assert_eq!(
                    (cell.ch, cell.fg, cell.script),
                    (rest.cells[c].ch, rest.cells[c].fg, rest.cells[c].script),
                    "{kind:?}: cell {c} is as at rest"
                );
            }
        }
    }
}

/// THE ACCOUNT UNIT READS AND LIGHTS AS ONE (glass design §1.2): line D's words-only unit, the
/// fallback shown while line E is not the quota row (`quota::tests` hold the row's own),
/// `⎆ main 37% left · ↺ Sat 10:00 · ╳ ~Thu 12 Nov 04:00`, reads with one blank between its parts and
/// the facts' gap before the next fact; at rest `⎆` takes the marks' ink of its column, the
/// name and `↺` the lit ink, the figure and time the values', each `·` the quiet ink, the gap
/// the warn ink; every word regular but `↺` and the gap's, which are bold. Lit, every glyph but
/// the warning's is lifted, under one underline, and the next fact stays as at rest. Negative
/// control: the figure's ink differs from the name's.
#[test]
fn the_account_unit_reads_and_lights_as_one() {
    use aterm_core::terminal::UnderlineStyle;
    let blank = cell(' ', [200, 200, 200]);
    let rule = cell('\u{2500}', [60, 60, 60]);
    let (plain, cruise) = (
        rule_glass(None, rule, blank),
        rule_glass(
            Some(aterm_effects::harness_card::CardPalette::Cruise),
            rule,
            blank,
        ),
    );
    let facts = FooterFacts {
        account: Some("main".into()),
        subscription: Some(aterm_agent::harness::profile::SubscriptionDisplay::default()),
        quota: Some(aterm_agent::harness::quota_view::QuotaView::sample()),
        ..facts()
    };
    let room = 120;
    let fit = footer::fit_rule(&facts, room, None);
    let place = rule_at_room(room);
    let rest = compose_rule_spans(&fit, None, rule, blank, (&plain, place), room, None);
    assert_eq!(
        text_of(&rest.cells),
        " \u{25C6} Opus 5.5 xhigh   \u{2386} main 37% left \u{b7} \u{21ba} Sat 10:00 \u{b7} \
         \u{2573} ~Thu 12 Nov 04:00   \u{2302} ~/aterm   \u{2387} main "
    );
    let unit = rest
        .spans
        .iter()
        .flatten()
        .find(|span| span.kind == FactKind::Account)
        .copied()
        .expect("the unit's span");
    assert_eq!(
        rest.spans
            .iter()
            .flatten()
            .filter(|s| s.kind == FactKind::Account)
            .count(),
        1,
        "one span"
    );
    let at = rest.facts_at + 1;
    let own = at + usize::from(unit.cells.0)..at + usize::from(unit.cells.1);
    let arc = compose_rule_spans(&fit, None, rule, blank, (&cruise, place), room, None);
    let arc_at = arc.facts_at + 1;
    let arc_own = arc_at + usize::from(unit.cells.0)..arc_at + usize::from(unit.cells.1);
    let first = |ch: char| {
        arc_own
            .clone()
            .find(|&c| arc.cells[c].ch == ch)
            .unwrap_or_else(|| panic!("{ch:?} in the unit"))
    };
    assert_eq!(arc.cells[arc_own.start].ch, footer::ACCOUNT_MARK);
    assert_eq!(
        arc.cells[arc_own.start].fg,
        rgb_of(cruise.mark(place.t(arc_own.start))),
        "the mark in the marks' ink of its column"
    );
    for (ch, ink, what) in [
        ('m', cruise.lit(), "the name"),
        ('\u{21ba}', cruise.lit(), "the reset"),
        ('3', cruise.value(), "the figure"),
        ('S', cruise.value(), "the time"),
        ('\u{b7}', cruise.quiet(), "the join"),
        ('\u{2573}', cruise.warn(), "the gap"),
    ] {
        assert_eq!(arc.cells[first(ch)].fg, rgb_of(ink), "{what}");
    }
    assert_ne!(
        arc.cells[first('3')].fg,
        arc.cells[first('m')].fg,
        "NEGATIVE CONTROL: the figure's ink is not the name's"
    );
    // Words are regular; the warning's and `↺`, whose glyph has so little ink, are laid bold
    // (§4.3, `quota_view::Piece::bold`).
    for (ch, bold) in [
        (footer::ACCOUNT_MARK, false),
        ('m', false),
        ('3', false),
        ('\u{b7}', false),
        ('\u{21ba}', true),
        ('S', false),
        ('\u{2573}', true),
    ] {
        assert_eq!(arc.cells[first(ch)].bold, bold, "{ch:?}'s weight");
    }
    let lit = compose_rule_spans(
        &fit,
        None,
        rule,
        blank,
        (&plain, place),
        room,
        Some(FactKind::Account),
    );
    let warned = own
        .clone()
        .find(|&c| rest.cells[c].ch == '\u{2573}')
        .expect("the gap");
    for (c, cell) in lit.cells.iter().enumerate() {
        if own.contains(&c) {
            if cell.ch != ' ' && c < warned {
                // The name and `↺` wear the lit ink at rest, which with no palette is the
                // hovered letter ink: they stay; every other glyph lifts.
                assert!(
                    cell.fg != rest.cells[c].fg || rest.cells[c].fg == rgb_of(plain.lit()),
                    "cell {c} ({:?}) is lifted",
                    cell.ch
                );
            }
        } else if cell.underline == UnderlineStyle::None {
            assert_eq!(
                (cell.ch, cell.fg),
                (rest.cells[c].ch, rest.cells[c].fg),
                "cell {c} is as at rest"
            );
        }
    }
    let under: Vec<usize> = (0..lit.cells.len())
        .filter(|&c| lit.cells[c].underline == UnderlineStyle::Single)
        .collect();
    assert!(
        under.windows(2).all(|w| w[1] == w[0] + 1),
        "one underline: {under:?}"
    );
    let inked: Vec<usize> = own.clone().filter(|&c| lit.cells[c].ch != ' ').collect();
    assert!(
        under.len() + 2 >= inked.last().unwrap() - inked.first().unwrap(),
        "under the whole unit: {under:?}, its ink {inked:?}"
    );
    let model = lit
        .spans
        .iter()
        .flatten()
        .find(|span| span.kind == FactKind::Model)
        .expect("the model's span");
    assert!(
        under.iter().all(|&c| c >= at + usize::from(model.cells.1)),
        "the model, ahead of the unit, is not lit"
    );
}
#[test]
fn wide_names_take_two_cells() {
    let wide = FooterFacts {
        branch: Some("\u{6F22}\u{5B57}".into()),
        ..FooterFacts::default()
    };
    let blank = cell(' ', [1, 1, 1]);
    let fit = footer::fit_rule(&wide, 20, None);
    let (row, _) = compose_rule(&fit, None, cell('\u{2500}', [2, 2, 2]), blank, None, 20);
    let lead = row.iter().position(|c| c.ch == '\u{6F22}').unwrap();
    assert!(
        !row[lead].wide && row[lead + 1].wide,
        "lead, then its right half"
    );
    assert_eq!(
        row[lead + 2..].iter().find(|c| c.ch != ' ').map(|c| c.ch),
        Some('\u{5B57}')
    );
    assert_eq!(row.len(), footer::segments_width(&fit.segments) + 2);
    assert_eq!(cells_for(" \u{2387} \u{6F22}\u{5B57} "), 8);
    assert!(!footer::fit_rule(&wide, 8, None).segments.is_empty());
    assert!(footer::fit_rule(&wide, 7, None).segments.is_empty());
}

fn facts_with_usage() -> FooterFacts {
    use aterm_agent::harness::session_usage::{ModelTokens, UsageFacts, Wall};
    FooterFacts {
        usage: UsageFacts::of(
            (
                vec![ModelTokens {
                    label: "opus".into(),
                    input: 48_000_000,
                    output: 310_000,
                }],
                aterm_agent::harness::session_usage::More::default(),
            ),
            Some(Wall {
                label: "5h",
                resets: "3pm".into(),
            }),
            None,
        ),
        ..facts()
    }
}

/// THE WALL AND USAGE MARKS TAKE THE MARKS' INK: `⧗` and `Σ` are drawn as `◆` is, in the
/// glass's marks ink of their own column ([`GlassInks::mark`]), under Cruise on a dark ground,
/// and the wall says until when alone (`⧗ until 3pm`, no window name). CONTROL: the marks stand
/// at different columns and their inks differ, so the column, not one fixed ink, decides.
///
/// The wall's words are the one warning on line D a person can act on (glass design §4.1,
/// §4.3, §8 item 16): the warn ink, bold, behind `⧗`, while the usage words beside them stay a
/// regular value (the control). NEGATIVE CONTROL (run once, restored): a plain wall segment in
/// `footer::fact_segments` lays `until 3pm` regular in the value ink and fails here.
#[test]
fn the_wall_and_usage_marks_take_the_marks_ink() {
    let blank = cell(' ', [200, 200, 200]);
    let rule = cell('\u{2500}', [60, 60, 60]);
    let facts = facts_with_usage();
    let room = footer::segments_width(&footer::segments(&facts)) + 4;
    let fit = footer::fit_rule(&facts, room, None);
    let palette = Some(aterm_effects::harness_card::CardPalette::Cruise);
    let cells = compose_rule(&fit, None, rule, blank, palette, room).0;
    let text = text_of(&cells);
    assert!(
        text.contains(" \u{25C6} Opus 5.5 xhigh   \u{29D7} until 3pm   ") && !text.contains("5h"),
        "the wall follows the model, until when alone: {text:?}"
    );
    let (glass, at) = (rule_glass(palette, rule, blank), rule_at_room(room));
    let col = |mark: char| cells.iter().position(|c| c.ch == mark).unwrap();
    let model = cells[col('\u{25C6}')];
    assert_eq!(model.fg, rgb_of(glass.mark(at.t(col('\u{25C6}')))), "`◆`");
    let mut inks = vec![model.fg];
    for mark in ['\u{29D7}', '\u{03A3}'] {
        let c = col(mark);
        assert_eq!(
            cells[c],
            RenderCell {
                ch: mark,
                fg: rgb_of(glass.mark(at.t(c))),
                ..model
            },
            "{mark}"
        );
        inks.push(cells[c].fg);
    }
    inks.dedup();
    assert!(inks.len() > 1, "CONTROL: one ink at every column");
    // The words after a mark, up to the next run of blanks.
    let words_after = |mark: char| -> Vec<RenderCell> {
        cells[col(mark) + 2..]
            .iter()
            .take_while(|c| c.ch != ' ')
            .copied()
            .collect()
    };
    let wall = words_after('\u{29D7}');
    assert_eq!(
        wall.iter().map(|c| c.ch).collect::<String>(),
        "until",
        "the wall's first word"
    );
    assert!(
        wall.iter().all(|c| c.fg == rgb_of(glass.warn()) && c.bold),
        "the wall's words are a warning, the warn ink and bold: {wall:?}"
    );
    let usage = words_after('\u{03A3}');
    assert!(
        !usage.is_empty()
            && usage
                .iter()
                .all(|c| c.fg == rgb_of(glass.value()) && !c.bold),
        "CONTROL: the usage words stay a regular value: {usage:?}"
    );
    assert_ne!(glass.warn(), glass.value(), "CONTROL: two inks");
}
#[test]
fn the_offset_cache_keeps_answers_and_asks_again_after_a_failure() {
    let mut answers = Vec::new();
    let t0 = std::time::Instant::now();
    let asked = std::cell::Cell::new(0);
    let mut ask = |at: std::time::Instant, answer: Option<i64>| {
        cached_offset(&mut answers, Some("UTC"), 100, at, |_, _| {
            asked.set(asked.get() + 1);
            answer
        })
    };
    assert_eq!(ask(t0, None), None, "the read failed");
    assert_eq!(ask(t0, Some(0)), Some(0), "asked again, not the failure");
    assert_eq!(asked.get(), 2);
    assert_eq!(ask(t0 + Duration::from_secs(60), Some(7)), Some(0), "kept");
    assert_eq!(
        asked.get(),
        2,
        "an answer is not read again within the hour"
    );
    assert_eq!(
        ask(t0 + Duration::from_secs(3600), Some(7)),
        Some(7),
        "an hour on, read afresh"
    );
}

#[test]
fn a_walled_live_session_stays_watched_on_the_slow_clock() {
    let asked = std::time::Instant::now();
    let within = asked + WATCH_FOR - Duration::from_secs(1);
    let past = asked + WATCH_FOR + Duration::from_secs(1);
    assert_eq!(recheck_after(within, asked), IDLE_RECHECK);
    assert_eq!(recheck_after(past, asked), WALL_RECHECK);
    assert!(still_watched(within, asked, || panic!("not asked within")));
    assert!(still_watched(past, asked, || true));
    assert!(!still_watched(past, asked, || false));
    let mut walled = HashSet::new();
    note_wall(&mut walled, 7, true);
    assert!(walled.contains(&7));
    note_wall(&mut walled, 7, false);
    assert!(walled.is_empty());
    let timeline = Arc::new(Mutex::new(SessionTimeline::default()));
    let mut schedule = WatchSchedule::default();
    schedule.watch(
        Job {
            session: 7,
            timeline,
            pgid: 42,
            wake: LogWake::default(),
        },
        asked,
        true,
    );
    let _ = schedule.due_followups(past);
    schedule.read(7, past, true);
    schedule.prune(past, |session, job| session == 7 && job.pgid == 42);
    assert_eq!(schedule.next(past), Some(past + WALL_RECHECK));
    assert!(schedule.due_idle(past + IDLE_RECHECK).is_none());
    assert!(schedule.due_idle(past + WALL_RECHECK).is_some());
    schedule.prune(past, |_, _| false);
    assert!(schedule.watched.is_empty(), "no wall: the watch ends");
}

#[test]
fn forgetting_a_session_drops_its_fold_and_watch() {
    let timeline = Arc::new(Mutex::new(SessionTimeline::default()));
    let job = |session| Job {
        session,
        timeline: Arc::clone(&timeline),
        pgid: 1,
        wake: LogWake::default(),
    };
    let now = std::time::Instant::now();
    let mut schedule = WatchSchedule::default();
    let mut known: HashMap<u64, Identity> = HashMap::new();
    let mut folds: HashMap<u64, footer::FooterCache> = HashMap::new();
    let mut walled: HashSet<u64> = HashSet::new();
    for session in [1, 2] {
        schedule.watch(job(session), now, true);
        known.insert(
            session,
            Identity {
                pid: 1,
                started: None,
                dir: None,
                launch: LaunchFacts::default(),
                launch_for: None,
                login: None,
                argv: Vec::new(),
                task: None,
                login_checked_for: None,
                route: aterm_agent::harness::accounts::AuthRoute::Outside,
                unoffered: None,
                env_stamp: None,
                first_seen: None,
            },
        );
        folds.insert(session, footer::FooterCache::default());
        walled.insert(session);
    }
    let Accepted {
        latest: immediate,
        stopped,
        ..
    } = schedule.accept([Command::Stop {
        session: 1,
        pgid: None,
    }]);
    assert!(immediate.is_empty());
    assert_eq!(stopped, [1]);
    for session in stopped {
        forget(session, &mut known, &mut folds, &mut walled);
    }
    let due = schedule.due_followups(now + FOLLOW_UPS[FOLLOW_UPS.len() - 1]);
    assert_eq!(
        due.len(),
        FOLLOW_UPS.len(),
        "the other session's follow-ups stay owed"
    );
    assert!(
        due.iter().all(|job| job.session == 2),
        "no follow-up owed for the retired session"
    );
    for held in [
        known.contains_key(&1),
        folds.contains_key(&1),
        walled.contains(&1),
        schedule.watched.contains_key(&1),
    ] {
        assert!(!held);
    }
    assert!(known.contains_key(&2) && folds.contains_key(&2) && walled.contains(&2));
    assert!(schedule.watched.contains_key(&2));
}

#[test]
fn a_group_leaving_the_foreground_releases_its_fold() {
    let timeline = Arc::new(Mutex::new(SessionTimeline::default()));
    let now = std::time::Instant::now();
    let mut schedule = WatchSchedule::default();
    let mut known: HashMap<u64, Identity> = HashMap::new();
    let mut folds: HashMap<u64, footer::FooterCache> = HashMap::new();
    let mut walled: HashSet<u64> = HashSet::new();
    schedule.watch(
        Job {
            session: 1,
            timeline: Arc::clone(&timeline),
            pgid: 40,
            wake: LogWake::default(),
        },
        now,
        true,
    );
    folds.insert(1, footer::FooterCache::default());
    walled.insert(1);
    let mut stop = |pgid| {
        let Accepted {
            latest: immediate,
            stopped,
            ..
        } = schedule.accept([Command::Stop {
            session: 1,
            pgid: Some(pgid),
        }]);
        assert!(immediate.is_empty());
        for session in &stopped {
            forget(*session, &mut known, &mut folds, &mut walled);
        }
        (stopped, folds.contains_key(&1), walled.contains(&1))
    };
    assert_eq!(stop(39), (vec![], true, true), "an older group's stop");
    assert_eq!(
        stop(40),
        (vec![1], false, false),
        "the watched group's stop"
    );
}

fn two_accounts() -> accounts::Roster {
    let row = |label: &str, dir: Option<&str>| accounts::Row {
        label: label.into(),
        class: accounts::Class::Preferred,
        dir: dir.map(str::to_owned),
        ..accounts::Row::default()
    };
    accounts::Roster {
        rows: vec![row("main", None), row("work-2", Some("/logins/work-2"))],
    }
}

fn args_of(env: &[&str]) -> atpkg::caller_shell::ProcArgs {
    atpkg::caller_shell::ProcArgs {
        argv: vec!["claude".into()],
        env: env.iter().map(|kv| (*kv).to_owned()).collect(),
        ..Default::default()
    }
}

fn login_under(dir: &str) -> Option<accounts::ProcessLogin> {
    let login = format!("{}={dir}", accounts::LOGIN_ENV);
    login_of_args(Some(&args_of(&["HOME=/home/someone", &login])))
}

use super::rig::{claude_home, scratch_home};

#[test]
fn an_environment_that_was_not_read_names_no_login() {
    assert_eq!(login_of_args(None), None, "no read");
    assert_eq!(login_of_args(Some(&args_of(&[]))), None, "reads empty");
    let roster = two_accounts();
    let default = login_of_args(Some(&args_of(&["HOME=/home/someone"]))).expect("read");
    assert_eq!(
        accounts::footer_label(
            &roster,
            &default,
            &aterm_agent::harness::observe::Observation::Unread
        ),
        Some("main")
    );
    let named = login_under("/logins/work-2").expect("read");
    assert_eq!(
        accounts::footer_label(
            &roster,
            &named,
            &aterm_agent::harness::observe::Observation::Unread
        ),
        Some("work-2")
    );
    let own_home = login_of_args(Some(&args_of(&[
        "HOME=/home/someone",
        "CLAUDE_CONFIG_DIR=/elsewhere",
    ])))
    .expect("read");
    assert_eq!(
        accounts::footer_label(
            &roster,
            &own_home,
            &aterm_agent::harness::observe::Observation::Unread
        ),
        None
    );
}

#[cfg(target_os = "macos")]
#[test]
fn unread_exec_directory_is_retried_for_the_same_process() {
    fn recovered_args(_: i32) -> Option<atpkg::caller_shell::ProcArgs> {
        let home = super::rig::home_root("footer-exec-context");
        Some(atpkg::caller_shell::ProcArgs {
            argv: vec!["claude".to_string()],
            env: vec![
                format!("HOME={}", home.display()),
                format!("CLAUDE_CONFIG_DIR={}", home.join("claude").display()),
            ],
            ..Default::default()
        })
    }

    let pid = i32::try_from(std::process::id()).unwrap();
    let started = birth_seconds(pid).expect("this test process's kernel start");
    assert_eq!(claude_dir_of_args(None, pid), None);
    let (root, claude) = claude_home("footer-exec-context", pid, started);
    let profile =
        aterm_agent::harness::profile::path_in(&root, Some(&claude.to_string_lossy()), &claude);
    std::fs::write(
        profile,
        r#"{"oauthAccount":{"accountUuid":"recovered-account","emailAddress":"recovered@example.test","planDisplayName":"Pro"}}"#,
    )
    .unwrap();
    let timeline = Arc::new(Mutex::new(SessionTimeline::default()));
    {
        let mut held = timeline.lock().unwrap();
        held.note_foreground_group(pid);
        held.set_program(pid, Some("claude".to_string()));
    }
    let job = Job {
        session: 17,
        timeline: Arc::clone(&timeline),
        pgid: pid,
        wake: LogWake::new(false),
    };
    let asks = Asks {
        files: |_| None,
        tap: NO_TAP,
        args: |_| None,
        roster: || None,
        home: scratch_home,
        route: |_, _, _| accounts::AuthRoute::Stored,
    };
    let mut known = HashMap::new();
    let mut tails = ProcessTails::default();
    let mut fold = footer::FooterCache::default();
    let mut roster = RosterMemo::default();
    let first = resolve_and_post(&mut known, &mut tails, &mut fold, &mut roster, &job, asks);
    assert!(!first.row);
    assert!(
        known.is_empty(),
        "an unread exec context must not pin a guessed directory"
    );

    let recovered = resolve_and_post(
        &mut known,
        &mut tails,
        &mut fold,
        &mut roster,
        &job,
        Asks {
            files: |_| None,
            args: recovered_args,
            ..asks
        },
    );
    assert!(
        recovered.row,
        "the next request finds the custom directory's registry"
    );
    let identity = known.get(&job.session).unwrap();
    assert_eq!(
        identity.started,
        Some(started),
        "the process did not restart"
    );
    assert_eq!(identity.dir.as_deref(), Some(claude.as_path()));
    assert_eq!(
        timeline
            .lock()
            .unwrap()
            .claude_footer()
            .unwrap()
            .subscription
            .as_ref()
            .unwrap()
            .email
            .as_deref(),
        Some("recovered@example.test")
    );
    assert_eq!(
        claude_dir_of_args(Some(&args_of(&["HOME=/home/someone"])), pid),
        Some(PathBuf::from("/home/someone/.claude")),
        "a readable HOME still supplies the default Claude directory"
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[cfg(target_os = "macos")]
#[test]
fn readable_launcher_directory_is_provisional_until_the_registered_image_is_read() {
    thread_local! {
        static EXEC_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
        static AUTH_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }
    fn args(registered: bool) -> Option<atpkg::caller_shell::ProcArgs> {
        EXEC_READS.set(EXEC_READS.get() + 1);
        let root = super::rig::home_root("footer-readable-launcher");
        let mut env = vec![format!(
            "HOME={}",
            root.join(if registered {
                "selected-home"
            } else {
                "launcher-home"
            })
            .display()
        )];
        if registered {
            env.push(format!(
                "CLAUDE_CONFIG_DIR={}",
                root.join("claude").display()
            ));
        }
        Some(atpkg::caller_shell::ProcArgs {
            argv: vec![if registered { "claude" } else { "launcher" }.into()],
            env,
            ..Default::default()
        })
    }

    let pid = i32::try_from(std::process::id()).unwrap();
    let started = birth_seconds(pid).expect("this test process's kernel start");
    let (root, claude) = claude_home("footer-readable-launcher", pid, started);
    let launcher_dir = root.join("launcher-home/.claude");
    std::fs::create_dir_all(&launcher_dir).unwrap();
    let now = aterm_agent::supervise::limit::unix_now();
    let profile = aterm_json::json!({
        "oauthAccount": {
            "accountUuid": "selected-account",
            "emailAddress": "selected@example.test",
            "planDisplayName": "Pro"
        },
        "cachedUsageUtilization": {
            "fetchedAtMs": now * 1000,
            "accountUuid": "selected-account",
            "utilization": {
                "seven_day": { "utilization": 71, "resets_at": "2099-01-01T00:00:00Z" }
            }
        }
    });
    std::fs::write(
        claude.join(".claude.json"),
        aterm_json::to_string(&profile).unwrap(),
    )
    .unwrap();
    let timeline = Arc::new(Mutex::new(SessionTimeline::default()));
    {
        let mut held = timeline.lock().unwrap();
        held.note_foreground_group(pid);
        held.set_program(pid, Some("claude".into()));
    }
    let job = Job {
        session: 18,
        timeline: Arc::clone(&timeline),
        pgid: pid,
        wake: LogWake::new(false),
    };
    let asks = Asks {
        files: |_| None,
        tap: NO_TAP,
        args: |_| args(false),
        roster: || None,
        home: scratch_home,
        route: |_, _, _| {
            AUTH_READS.set(AUTH_READS.get() + 1);
            accounts::AuthRoute::Stored
        },
    };
    let mut known = HashMap::new();
    let mut tails = ProcessTails::default();
    let mut fold = footer::FooterCache::default();
    let mut roster = RosterMemo::default();
    let first = resolve_and_post(&mut known, &mut tails, &mut fold, &mut roster, &job, asks);
    assert!(!first.row);
    let provisional = known.get(&job.session).unwrap();
    assert_eq!(provisional.dir.as_deref(), Some(launcher_dir.as_path()));
    assert_eq!(provisional.launch_for, None);

    let selected = Asks {
        files: |_| None,
        args: |_| args(true),
        ..asks
    };
    let recovered = resolve_and_post(
        &mut known,
        &mut tails,
        &mut fold,
        &mut roster,
        &job,
        selected,
    );
    assert!(
        recovered.row,
        "the same process now names its custom directory"
    );
    let confirmed = known.get(&job.session).unwrap().clone();
    assert_eq!(confirmed.started, Some(started));
    assert_eq!(confirmed.dir.as_deref(), Some(claude.as_path()));
    assert_eq!(confirmed.launch_for, Some(Some(started)));
    let display = timeline
        .lock()
        .unwrap()
        .claude_footer()
        .unwrap()
        .subscription
        .clone()
        .unwrap();
    assert_eq!(display.email.as_deref(), Some("selected@example.test"));
    assert_eq!(display.weekly.as_deref(), Some("weekly 29% left"));
    assert_eq!(AUTH_READS.get(), 1);

    for registry_present in [true, false] {
        if !registry_present {
            std::fs::remove_file(claude.join(format!("sessions/{pid}.json"))).unwrap();
        }
        let before = EXEC_READS.get();
        let found = resolve_and_post(
            &mut known,
            &mut tails,
            &mut fold,
            &mut roster,
            &job,
            selected,
        );
        assert_eq!(found.row, registry_present);
        assert_eq!(EXEC_READS.get(), before + 1);
        assert_eq!(AUTH_READS.get(), 1);
        assert_eq!(known.get(&job.session), Some(&confirmed));
        assert_eq!(
            timeline
                .lock()
                .unwrap()
                .claude_footer()
                .unwrap()
                .subscription
                .as_ref(),
            Some(&display)
        );
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn the_roster_is_read_at_most_every_recheck() {
    let t0 = std::time::Instant::now();
    let asked = std::cell::Cell::new(0);
    let mut memo = RosterMemo::default();
    let mut at = |now: std::time::Instant, answer: Option<accounts::Roster>| {
        memo.at(now, || {
            asked.set(asked.get() + 1);
            answer
        })
        .cloned()
    };
    let two = two_accounts();
    let mut three = two.clone();
    three.rows.push(accounts::Row {
        label: "pool-a".into(),
        class: accounts::Class::Pool,
        dir: Some("/logins/pool-a".into()),
        ..accounts::Row::default()
    });
    let one = accounts::Roster {
        rows: two.rows[..1].to_vec(),
    };
    assert_eq!(at(t0, Some(two.clone())), Some(two.clone()));
    let within = t0 + ROSTER_RECHECK - Duration::from_millis(1);
    assert_eq!(at(within, Some(three.clone())), Some(two.clone()), "kept");
    assert_eq!(asked.get(), 1, "not read again within the period");
    let t1 = t0 + ROSTER_RECHECK;
    assert_eq!(at(t1, Some(three.clone())), Some(three), "read afresh");
    assert_eq!(asked.get(), 2);
    let t2 = t1 + ROSTER_RECHECK;
    assert_eq!(
        at(t2, Some(one.clone())),
        Some(one.clone()),
        "one account binds identity"
    );
    assert_eq!(
        at(t2 + Duration::from_secs(1), Some(two.clone())),
        Some(one),
        "the answer is kept for the period"
    );
    assert_eq!(asked.get(), 3);
    let t3 = t2 + ROSTER_RECHECK;
    assert_eq!(at(t3, None), None, "a load that fails");
    let t4 = t3 + ROSTER_RECHECK;
    assert_eq!(
        at(t4, Some(accounts::Roster::default())),
        Some(accounts::Roster::default()),
        "none listed"
    );
    let t5 = t4 + ROSTER_RECHECK;
    assert_eq!(at(t5, Some(two.clone())), Some(two), "listed again");
    assert_eq!(asked.get(), 6);
}

fn on_main(_: i32) -> Option<atpkg::caller_shell::ProcArgs> {
    Some(args_of(&["HOME=/home/someone"]))
}

fn on_work(_: i32) -> Option<atpkg::caller_shell::ProcArgs> {
    let login = format!("{}=/logins/work-2", accounts::LOGIN_ENV);
    Some(atpkg::caller_shell::ProcArgs {
        argv: vec!["claude".into(), "--resume".into()],
        ..args_of(&["HOME=/home/someone", &login])
    })
}

fn unread(pid: i32, claude: &std::path::Path) -> Identity {
    Identity {
        pid,
        started: None,
        dir: Some(claude.to_path_buf()),
        launch: LaunchFacts::default(),
        launch_for: None,
        login: None,
        argv: Vec::new(),
        task: None,
        login_checked_for: None,
        route: aterm_agent::harness::accounts::AuthRoute::Outside,
        unoffered: None,
        env_stamp: None,
        first_seen: None,
    }
}

fn account_after(
    identity: &mut Identity,
    roster: &mut RosterMemo,
    asks: Asks,
) -> Option<Option<String>> {
    let timeline = Arc::new(Mutex::new(SessionTimeline::default()));
    timeline.lock().unwrap().note_foreground_group(identity.pid);
    resolve_into(
        &timeline,
        identity,
        &mut footer::TailCache::default(),
        &mut footer::FooterCache::default(),
        roster,
        asks,
    );
    let timeline = timeline.lock().unwrap();
    timeline.claude_footer().map(|facts| facts.account.clone())
}

#[test]
fn selected_profile_changes_replace_subscription_even_when_registry_is_unreadable() {
    use aterm_agent::harness::profile::Bucket;

    const PID: i32 = 4243;
    const START: u64 = 1_790_607_848;
    let (root, claude) = claude_home("footer-subscription-resolve", PID, START);
    let profile =
        aterm_agent::harness::profile::path_in(&root, Some(&claude.to_string_lossy()), &claude);
    let now = aterm_agent::supervise::limit::unix_now();
    let content = |email: &str, account: &str| {
        aterm_json::json!({
            "oauthAccount": {
                "accountUuid": account,
                "emailAddress": email,
                "organizationType": "claude_max",
                "planDisplayName": "Max 5x"
            },
            "cachedUsageUtilization": {
                "fetchedAtMs": now * 1000,
                "accountUuid": "first-account",
                "utilization": {
                    "seven_day": { "utilization": 71, "resets_at": "2099-01-01T00:00:00Z" },
                    "seven_day_opus": { "utilization": 80, "resets_at": "2099-01-01T00:00:00Z" }
                }
            }
        })
    };
    std::fs::write(
        &profile,
        aterm_json::to_string(&content("first@example.com", "first-account")).unwrap(),
    )
    .unwrap();
    let timeline = Arc::new(Mutex::new(SessionTimeline::default()));
    {
        let mut held = timeline.lock().unwrap();
        held.note_foreground_group(PID);
        held.set_program(PID, Some("claude".into()));
    }
    let mut identity = unread(PID, &claude);
    identity.started = Some(START);
    let mut tail = footer::TailCache::default();
    let mut fold = footer::FooterCache::default();
    let mut memo = RosterMemo::default();
    let asks = Asks {
        files: |_| None,
        tap: NO_TAP,
        args: |_| {
            let root = super::rig::home_root("footer-subscription-resolve");
            Some(args_of(&[
                &format!("HOME={}", root.display()),
                &format!("CLAUDE_CONFIG_DIR={}", root.join("claude").display()),
            ]))
        },
        roster: || None,
        home: scratch_home,
        route: |_, _, _| accounts::AuthRoute::Stored,
    };
    let mut refresh = || {
        let (_, found) = resolve_facts(
            &timeline,
            &mut identity,
            &mut tail,
            &mut fold,
            &mut memo,
            asks,
        );
        let held = timeline.lock().unwrap();
        let facts = held.claude_footer().unwrap();
        let binding = facts.plan.map(|plan| (plan.window, plan.used_x10));
        (facts.subscription.clone().unwrap(), binding, found.timed)
    };
    let (first, plan, _) = refresh();
    assert_eq!(first.email.as_deref(), Some("first@example.com"));
    assert_eq!(first.weekly.as_deref(), Some("weekly 29% left"));
    assert_eq!(
        plan,
        Some((Bucket::SevenDay, 710)),
        "no model is named, so only the account-wide windows gate it"
    );
    {
        let mut held = timeline.lock().unwrap();
        let owner = held.claude_footer().unwrap().owner.clone().unwrap();
        let card = aterm_phase::LaunchCard {
            version: "2.1.287".into(),
            model: "Opus 5.5".into(),
            effort: None,
        };
        assert!(held.note_claude_card(PID, &owner, card));
    }
    assert_eq!(
        refresh(),
        (first.clone(), Some((Bucket::SevenDayOpus, 800)), true)
    );
    std::fs::remove_file(claude.join(format!("sessions/{PID}.json"))).unwrap();
    assert_eq!(refresh(), (first, Some((Bucket::SevenDayOpus, 800)), true));
    std::fs::write(
        &profile,
        aterm_json::to_string(&content("second@example.com", "second-account")).unwrap(),
    )
    .unwrap();
    let (second, plan, timed) = refresh();
    assert_eq!(second.email.as_deref(), Some("second@example.com"));
    assert!(
        second.weekly.is_none(),
        "the previous account's quota cannot survive"
    );
    assert_eq!(plan, None, "nor can its plan, on the kept facts");
    assert!(!timed);
    let five_hour = aterm_json::json!({
        "oauthAccount": { "accountUuid": "second-account", "emailAddress": "second@example.com" },
        "cachedUsageUtilization": {
            "fetchedAtMs": now * 1000, "accountUuid": "second-account",
            "utilization": { "five_hour": { "utilization": 40, "resets_at": "2099-01-01T00:00:00Z" } }
        }
    });
    std::fs::write(&profile, aterm_json::to_string(&five_hour).unwrap()).unwrap();
    let (shown, plan, timed) = refresh();
    assert!(shown.weekly.is_none());
    assert_eq!(plan, Some((Bucket::FiveHour, 400)));
    assert!(timed, "the plan alone keeps the clock");
    std::fs::write(&profile, "{").unwrap();
    assert_eq!(
        refresh(),
        (
            aterm_agent::harness::profile::SubscriptionDisplay::default(),
            None,
            false
        )
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_muse_login_store_switch_drops_the_previous_accounts_card_facts() {
    const PID: i32 = 4246;
    let root = crate::test_scratch::process_root("footer-muse-account-switch");
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let store = home.join(".config/muse");
    std::fs::create_dir_all(&store).unwrap();
    let auth = store.join("auth.json");
    let write_auth = |email: &str| {
        std::fs::write(
            &auth,
            format!(r#"{{"providers":{{"meta":{{"mechanism":"oauth","user_email":"{email}"}}}}}}"#),
        )
        .unwrap()
    };
    write_auth("first@example.test");
    let timeline = Arc::new(Mutex::new(SessionTimeline::default()));
    {
        let mut held = timeline.lock().unwrap();
        held.note_foreground_group(PID);
        held.set_program(PID, Some("muse".into()));
        assert!(held.set_claude_footer(
            PID,
            None,
            Some(FooterFacts {
                owner: Some(footer::FactsOwner {
                    floor: None,
                    session_id: format!("muse-{PID}"),
                }),
                subscription: Some(aterm_agent::harness::profile::SubscriptionDisplay {
                    email: Some("first@example.test".into()),
                    plan: Some("Muse Code Max Usage".into()),
                    weekly: Some("weekly 29% left".into()),
                    ..Default::default()
                },),
                ..FooterFacts::default()
            }),
        ));
    }
    let mut identity = unread(PID, &root.join("claude"));
    let mut tail = footer::TailCache::default();
    let mut fold = footer::FooterCache::default();
    let mut memo = RosterMemo::default();
    let asks = Asks {
        files: |_| None,
        tap: NO_TAP,
        args: |_| {
            let root = crate::test_scratch::process_root("footer-muse-account-switch");
            Some(atpkg::caller_shell::ProcArgs {
                argv: vec!["muse".into()],
                env: vec![format!("HOME={}", root.join("home").display())],
                ..Default::default()
            })
        },
        roster: || None,
        home: scratch_home,
        route: |_, _, _| accounts::AuthRoute::Stored,
    };
    let refresh = |identity: &mut Identity,
                   tail: &mut footer::TailCache,
                   fold: &mut footer::FooterCache,
                   memo: &mut RosterMemo,
                   now| {
        let clock = footer::ReadClock {
            read_at: std::time::UNIX_EPOCH,
            now,
            offset_at: &|_, _| Some(0),
        };
        resolve_facts_at_with_cwd(
            &timeline,
            identity,
            tail,
            fold,
            memo,
            (asks, |_| None),
            &clock,
        );
        timeline
            .lock()
            .unwrap()
            .claude_footer()
            .and_then(|facts| facts.subscription.clone())
            .unwrap()
    };
    const RESET: i64 = 1_792_947_600;
    let first = refresh(&mut identity, &mut tail, &mut fold, &mut memo, RESET - 1);
    assert_eq!(first.email.as_deref(), Some("first@example.test"));
    assert_eq!(first.plan.as_deref(), Some("Muse Code Max Usage"));
    assert_eq!(first.weekly.as_deref(), Some("weekly 29% left"));
    let clock = footer::ReadClock {
        read_at: std::time::UNIX_EPOCH,
        now: RESET - 1,
        offset_at: &|_, _| Some(0),
    };
    let reset = aterm_agent::harness::profile::WeeklyReset {
        words: "Oct 25 at 5:00 PM".into(),
        at: muse_weekly_reset_at("Oct 25 at 5:00 PM", &clock),
    };
    assert_eq!(reset.at, Some(RESET));
    {
        let mut held = timeline.lock().unwrap();
        let mut facts = held.claude_footer().cloned().expect("facts");
        let sub = facts.subscription.as_mut().expect("subscription");
        sub.weekly_reset = Some(reset.clone());
        assert!(held.set_claude_footer(PID, None, Some(facts)));
    }
    let before = refresh(&mut identity, &mut tail, &mut fold, &mut memo, RESET - 1);
    assert_eq!(before.weekly.as_deref(), Some("weekly 29% left"));
    assert_eq!(before.weekly_reset.as_ref(), Some(&reset));
    let expired = refresh(&mut identity, &mut tail, &mut fold, &mut memo, RESET);
    assert_eq!(expired.email.as_deref(), Some("first@example.test"));
    assert_eq!(expired.plan.as_deref(), Some("Muse Code Max Usage"));
    assert_eq!(
        (expired.weekly, expired.weekly_reset.as_ref()),
        (None, Some(&reset)),
        "expiry keeps the observation's occurrence, not its percentage"
    );
    let relative = aterm_agent::harness::profile::WeeklyReset {
        words: "in 1 hour".into(),
        at: muse_weekly_reset_at("in 1 hour", &clock),
    };
    let relative_at = RESET - 1 + 3600;
    assert_eq!(relative.at, Some(relative_at));
    {
        let mut held = timeline.lock().unwrap();
        let mut facts = held.claude_footer().cloned().unwrap();
        let sub = facts.subscription.as_mut().unwrap();
        sub.weekly = Some("weekly 29% left".into());
        sub.weekly_reset = Some(relative.clone());
        assert!(held.set_claude_footer(PID, None, Some(facts)));
    }
    let before = refresh(
        &mut identity,
        &mut tail,
        &mut fold,
        &mut memo,
        relative_at - 1,
    );
    assert_eq!(before.weekly.as_deref(), Some("weekly 29% left"));
    let expired = refresh(&mut identity, &mut tail, &mut fold, &mut memo, relative_at);
    assert_eq!(
        (expired.weekly, expired.weekly_reset),
        (None, Some(relative))
    );
    let unknown = aterm_agent::harness::profile::WeeklyReset {
        words: "not a reset".into(),
        at: muse_weekly_reset_at("not a reset", &clock),
    };
    assert_eq!(unknown.at, None);
    {
        let mut held = timeline.lock().unwrap();
        let mut facts = held.claude_footer().cloned().unwrap();
        let sub = facts.subscription.as_mut().unwrap();
        sub.weekly = Some("weekly 29% left".into());
        sub.weekly_reset = Some(unknown.clone());
        assert!(held.set_claude_footer(PID, None, Some(facts)));
    }
    let kept = refresh(
        &mut identity,
        &mut tail,
        &mut fold,
        &mut memo,
        relative_at + 60,
    );
    assert_eq!(kept.weekly.as_deref(), Some("weekly 29% left"));
    assert_eq!(kept.weekly_reset, Some(unknown));
    write_auth("second@example.test");
    let second = refresh(
        &mut identity,
        &mut tail,
        &mut fold,
        &mut memo,
        relative_at + 61,
    );
    assert_eq!(second.email.as_deref(), Some("second@example.test"));
    assert_eq!(
        (second.plan, second.weekly, second.weekly_reset),
        (None, None, None),
        "the previous account's tier, quota and reset cannot survive"
    );
    // THE THIRD FINDING: Muse Code signed out (its store names no login). The footer stops
    // showing the account it ran on, and its plan and week with it.
    // NEGATIVE CONTROL: with the footer keeping `prev_sub` on a `None` read, the address
    // stays: `` left: Some("second@example.test") right: None ``.
    {
        let mut held = timeline.lock().unwrap();
        let mut facts = held.claude_footer().cloned().unwrap();
        let sub = facts.subscription.as_mut().unwrap();
        sub.plan = Some("Muse Code Max Usage".into());
        sub.weekly = Some("weekly 88% left".into());
        assert!(held.set_claude_footer(PID, None, Some(facts)));
    }
    std::fs::write(&auth, r#"{"schema_version":2,"providers":{}}"#).unwrap();
    let signed_out = refresh(
        &mut identity,
        &mut tail,
        &mut fold,
        &mut memo,
        relative_at + 62,
    );
    assert_eq!(signed_out.email, None);
    assert_eq!(
        (signed_out.plan, signed_out.weekly, signed_out.weekly_figure),
        (None, None, None),
        "a signed-out login shows no account's plan or week"
    );
    // Zoneless weekly reset across a daylight change uses the target instant's local offset.
    const PDT: i64 = -7 * 3_600;
    const PST: i64 = -8 * 3_600;
    const FALL_NOW: i64 = 1_793_509_200; // 2026-10-31 22:00 PDT
    const FALL_CHANGE: i64 = 1_793_523_600; // 2026-11-01T09:00:00Z
    let dst_clock = footer::ReadClock {
        read_at: std::time::UNIX_EPOCH,
        now: FALL_NOW,
        offset_at: &|zone, unix| match zone {
            Some(_) => None,
            None => Some(if unix < FALL_CHANGE { PDT } else { PST }),
        },
    };
    let flat_clock = footer::ReadClock {
        read_at: std::time::UNIX_EPOCH,
        now: FALL_NOW,
        offset_at: &|_, _| Some(PDT),
    };
    assert_eq!(
        muse_weekly_reset_at("Nov 3 at 11:00 AM", &dst_clock),
        Some(1_793_732_400),
        "2026-11-03 11:00 PST (19:00Z)"
    );
    assert_eq!(
        muse_weekly_reset_at("Nov 3 at 11:00 AM", &flat_clock),
        Some(1_793_728_800),
        "CONTROL: flat PDT places Nov 3 an hour early"
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn subscription_requires_stored_auth_for_the_registered_image_and_directory() {
    const PID: i32 = 4245;
    const IMAGE: u64 = 1_790_607_848;
    thread_local! {
        static CHECKS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
        static ALLOWED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }
    fn args(_: i32) -> Option<atpkg::caller_shell::ProcArgs> {
        let root = super::rig::home_root("footer-subscription-auth");
        Some(atpkg::caller_shell::ProcArgs {
            argv: vec![
                "claude".into(),
                "--settings".into(),
                r#"{"apiKeyHelper":"/opt/key.sh"}"#.into(),
            ],
            env: vec![
                format!("HOME={}", root.display()),
                format!("CLAUDE_CONFIG_DIR={}", root.join("claude").display()),
            ],
            ..Default::default()
        })
    }
    let (root, claude) = claude_home("footer-subscription-auth", PID, IMAGE);
    let profile =
        aterm_agent::harness::profile::path_in(&root, Some(&claude.to_string_lossy()), &claude);
    let now = aterm_agent::supervise::limit::unix_now();
    let content = aterm_json::json!({
        "oauthAccount": { "accountUuid": "auth-account", "emailAddress": "auth@example.com", "planDisplayName": "Max 5x" },
        "cachedUsageUtilization": {
            "fetchedAtMs": now * 1000, "accountUuid": "auth-account",
            "utilization": { "seven_day": { "utilization": 71, "resets_at": "2099-01-01T00:00:00Z" } }
        }
    });
    std::fs::write(&profile, aterm_json::to_string(&content).unwrap()).unwrap();
    let timeline = Arc::new(Mutex::new(SessionTimeline::default()));
    {
        let mut held = timeline.lock().unwrap();
        held.note_foreground_group(PID);
        held.set_program(PID, Some("claude".into()));
    }
    let mut identity = unread(PID, &claude);
    identity.started = Some(IMAGE);
    let mut tail = footer::TailCache::default();
    let mut fold = footer::FooterCache::default();
    let mut memo = RosterMemo::default();
    let asks = Asks {
        files: |_| None,
        tap: NO_TAP,
        args,
        roster: || None,
        home: scratch_home,
        route: |pid, argv, cwd| {
            assert_eq!(pid, PID as u32);
            assert_eq!(argv[0], "claude");
            assert!(std::path::Path::new(cwd).is_absolute());
            CHECKS.set(CHECKS.get() + 1);
            accounts::AuthRoute::Stored.unless(!ALLOWED.get())
        },
    };
    let mut refresh = |asks| {
        resolve_facts(
            &timeline,
            &mut identity,
            &mut tail,
            &mut fold,
            &mut memo,
            asks,
        );
        timeline
            .lock()
            .unwrap()
            .claude_footer()
            .unwrap()
            .subscription
            .clone()
            .unwrap()
    };
    let unavailable = aterm_agent::harness::profile::SubscriptionDisplay::default();
    assert_eq!(
        refresh(asks),
        unavailable,
        "helper/provider/refused sources suppress every field"
    );
    assert_eq!(refresh(asks), unavailable);
    assert_eq!(
        CHECKS.get(),
        1,
        "one auth-source gather per registered image"
    );
    ALLOWED.set(true);
    assert_eq!(
        refresh(asks),
        unavailable,
        "the verdict belongs to the old image"
    );

    let registry = claude.join(format!("sessions/{PID}.json"));
    let mut row: aterm_json::Value =
        aterm_json::from_slice(&std::fs::read(&registry).unwrap()).unwrap();
    row.as_object_mut()
        .unwrap()
        .insert("startedAt".into(), aterm_json::json!((IMAGE + 60) * 1000));
    std::fs::write(&registry, aterm_json::to_string(&row).unwrap()).unwrap();
    let display = refresh(asks);
    assert_eq!(display.email.as_deref(), Some("auth@example.com"));
    assert_eq!(display.plan.as_deref(), Some("Max 5x"));
    assert_eq!(display.weekly.as_deref(), Some("weekly 29% left"));
    assert_eq!(refresh(asks), display);
    assert_eq!(CHECKS.get(), 2);

    let cwd = root.join("another-work");
    std::fs::create_dir(&cwd).unwrap();
    row.as_object_mut()
        .unwrap()
        .insert("cwd".into(), aterm_json::json!(cwd));
    std::fs::write(&registry, aterm_json::to_string(&row).unwrap()).unwrap();
    assert_eq!(refresh(asks), display);
    assert_eq!(
        CHECKS.get(),
        3,
        "different project settings need a new gather"
    );
    let unregistered = Asks {
        files: |_| None,
        args: |pid| {
            let mut args = args(pid)?;
            args.argv.push("--new-image".into());
            Some(args)
        },
        ..asks
    };
    assert_eq!(
        refresh(unregistered),
        unavailable,
        "an exec cannot borrow a prior image's verdict"
    );
    assert_eq!(CHECKS.get(), 3);
    let changed_home = Asks {
        files: |_| None,
        args: |pid| {
            let mut args = args(pid)?;
            args.env[0].push_str("/another-home");
            Some(args)
        },
        ..asks
    };
    assert_eq!(
        refresh(changed_home),
        unavailable,
        "the same argv with another HOME cannot borrow the old settings verdict"
    );
    assert_eq!(CHECKS.get(), 3);
    assert_ne!(
        exec_env_stamp(&["A=B".into(), "C=D".into()]),
        exec_env_stamp(&["A=BC=D".into()]),
        "entry boundaries are part of the environment identity"
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn weekly_quota_expires_on_the_local_clock_after_the_idle_watch_lapses() {
    const PID: i32 = 4244;
    const START: u64 = 1_790_607_848;
    const OBSERVED: i64 = 1_792_050_000;
    let (root, claude) = claude_home("footer-subscription-expiry", PID, START);
    let profile =
        aterm_agent::harness::profile::path_in(&root, Some(&claude.to_string_lossy()), &claude);
    let asks = Asks {
        files: |_| None,
        tap: NO_TAP,
        args: |_| {
            let root = super::rig::home_root("footer-subscription-expiry");
            Some(args_of(&[
                &format!("HOME={}", root.display()),
                &format!("CLAUDE_CONFIG_DIR={}", root.join("claude").display()),
            ]))
        },
        roster: || None,
        home: scratch_home,
        route: |_, _, _| accounts::AuthRoute::Stored,
    };
    for (reset, expires_after) in [
        ("2099-01-01T00:00:00Z", 3600),
        ("2026-10-15T07:50:50Z", 650),
    ] {
        let content = aterm_json::json!({
            "oauthAccount": {
                "accountUuid": "expiry-account",
                "emailAddress": "expiry@example.com",
                "organizationType": "claude_max",
                "planDisplayName": "Max 5x"
            },
            "cachedUsageUtilization": {
                "fetchedAtMs": OBSERVED * 1000,
                "accountUuid": "expiry-account",
                "utilization": { "seven_day": { "utilization": 71, "resets_at": reset } }
            }
        });
        std::fs::write(&profile, aterm_json::to_string(&content).unwrap()).unwrap();
        let timeline = Arc::new(Mutex::new(SessionTimeline::default()));
        {
            let mut held = timeline.lock().unwrap();
            held.note_foreground_group(PID);
            held.set_program(PID, Some("claude".into()));
        }
        let mut identity = unread(PID, &claude);
        identity.started = Some(START);
        let mut tail = footer::TailCache::default();
        let mut fold = footer::FooterCache::default();
        let mut memo = RosterMemo::default();
        let mut refresh = |elapsed| {
            let (_, found) = resolve_facts_at(
                &timeline,
                &mut identity,
                &mut tail,
                &mut fold,
                &mut memo,
                asks,
                &footer::ReadClock {
                    read_at: std::time::SystemTime::now(),
                    now: OBSERVED + elapsed,
                    offset_at: &|_, _| Some(0),
                },
            );
            let display = timeline
                .lock()
                .unwrap()
                .claude_footer()
                .unwrap()
                .subscription
                .clone()
                .unwrap();
            (found, display)
        };
        let (found, display) = refresh(601);
        assert_eq!(display.weekly.as_deref(), Some("weekly 29% left"));
        assert!(found.timed, "weekly without a wall still needs the clock");

        let asked = std::time::Instant::now();
        let mut schedule = WatchSchedule::default();
        schedule.watch(
            Job {
                session: 7,
                timeline: timeline.clone(),
                pgid: PID,
                wake: LogWake::default(),
            },
            asked,
            true,
        );
        let past = asked + Duration::from_secs(601);
        let _ = schedule.due_followups(past);
        schedule.read(7, past, found.row);
        schedule.prune(past, |_, _| found.timed);
        assert!(
            schedule.watched.contains_key(&7),
            "quota keeps the idle clock"
        );
        for elapsed in (661..=3661).step_by(60) {
            let at = asked + Duration::from_secs(elapsed as u64);
            assert_eq!(schedule.next(at), Some(at));
            assert!(schedule.due_idle(at).is_some());
            let (found, display) = refresh(elapsed);
            assert_eq!(display.email.as_deref(), Some("expiry@example.com"));
            assert_eq!(display.plan.as_deref(), Some("Max 5x"));
            schedule.read(7, at, found.row);
            schedule.prune(at, |_, _| found.timed);
            if elapsed > expires_after {
                assert!(display.weekly.is_none(), "stale quota cleared: {reset}");
                assert!(!found.timed);
                assert!(
                    schedule.watched.is_empty(),
                    "quota expiry releases the clock"
                );
                assert!(elapsed <= expires_after + 60, "bounded by the slow clock");
                break;
            }
            let aged = elapsed > 900;
            assert!(
                display
                    .weekly
                    .as_deref()
                    .is_some_and(|words| words.contains('\u{2264}') == aged),
                "past fifteen minutes, a lower bound: {elapsed} {:?}",
                display.weekly
            );
        }
        assert!(schedule.watched.is_empty());
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_read_that_answers_is_given_its_account() {
    const PID: i32 = 4242;
    let (root, claude) = claude_home("footer-account", PID, 1_790_607_848);
    let listed = |args| Asks {
        files: |_| None,
        tap: NO_TAP,
        args,
        roster: || Some(two_accounts()),
        home: scratch_home,
        route: |_, _, _| accounts::AuthRoute::Stored,
    };
    let unasked = |args| Asks {
        files: |_| None,
        tap: NO_TAP,
        args,
        roster: || unreachable!("no login was read: the roster is not asked"),
        home: scratch_home,
        route: |_, _, _| accounts::AuthRoute::Stored,
    };
    let read = |asks: Asks| {
        let mut memo = RosterMemo::default();
        let account = account_after(&mut unread(PID, &claude), &mut memo, asks);
        (account, memo.read.is_some())
    };
    assert_eq!(read(listed(on_work)), (Some(Some("work-2".into())), true));
    assert_eq!(
        read(listed(on_main)),
        (Some(Some("main".into())), true),
        "the default login is the row with no directory"
    );
    let nobody = |_| {
        let login = format!("{}=/logins/nobody", accounts::LOGIN_ENV);
        Some(args_of(&["HOME=/home/someone", &login]))
    };
    assert_eq!(read(listed(nobody)), (Some(None), true), "no row names it");
    let none = Asks {
        files: |_| None,
        tap: NO_TAP,
        args: on_main,
        roster: || None,
        home: scratch_home,
        route: |_, _, _| accounts::AuthRoute::Stored,
    };
    assert_eq!(read(none), (Some(None), true), "no roster: asked, none");
    assert_eq!(
        read(unasked(|_| Some(args_of(&[])))),
        (Some(None), false),
        "an environment that reads empty"
    );
    let mut identity = unread(PID, &claude);
    let mut memo = RosterMemo::default();
    assert_eq!(
        account_after(&mut identity, &mut memo, unasked(|_| None)),
        Some(None)
    );
    assert_eq!(
        identity.launch_for, None,
        "not read: nothing is kept for it"
    );
    assert_eq!(
        account_after(&mut identity, &mut memo, listed(on_main)),
        Some(Some("main".into())),
        "read at the next"
    );
    std::fs::remove_file(claude.join(format!("sessions/{PID}.json"))).unwrap();
    let nothing = Asks {
        files: |_| None,
        tap: NO_TAP,
        args: |_| unreachable!("no registry row: the process is not read"),
        roster: || unreachable!("no facts: the roster is not asked"),
        home: scratch_home,
        route: |_, _, _| accounts::AuthRoute::Stored,
    };
    assert_eq!(read(nothing), (None, false));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_read_publishes_the_excerpts_beside_the_facts() {
    const PID: i32 = 4243;
    let (root, claude) = claude_home("footer-excerpts", PID, 1_790_607_848);
    let project = claude
        .join("projects")
        .join(footer::project_slug(&root.join("work")));
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(
        project.join("00000000-0000-4000-8000-000000000a01.jsonl"),
        concat!(
            r#"{"type":"user","timestamp":"2026-09-28T15:00:00Z","message":{"content":"wire the survey"}}"#,
            "\n",
            r#"{"type":"assistant","timestamp":"2026-09-28T15:00:05Z","message":{"id":"m1","model":"claude-opus-5-5","content":[{"type":"text","text":"On it."}]}}"#,
            "\n",
        ),
    )
    .unwrap();
    let asks = Asks {
        files: |_| None,
        tap: NO_TAP,
        args: |_| Some(args_of(&["HOME=/home/someone"])),
        roster: || None,
        home: scratch_home,
        route: |_, _, _| accounts::AuthRoute::Stored,
    };
    let read = |asks: Asks| {
        let timeline = Arc::new(Mutex::new(SessionTimeline::default()));
        timeline.lock().unwrap().note_foreground_group(PID);
        resolve_into(
            &timeline,
            &mut unread(PID, &claude),
            &mut footer::TailCache::default(),
            &mut footer::FooterCache::default(),
            &mut RosterMemo::default(),
            asks,
        );
        let timeline = timeline.lock().unwrap();
        (
            timeline.claude_footer().is_some(),
            timeline.claude_excerpts().cloned(),
        )
    };
    let (facts, excerpts) = read(asks);
    assert!(facts, "the footer is published");
    let excerpts = excerpts.expect("and its excerpts");
    assert_eq!(excerpts.excerpts.prompts.len(), 1);
    assert_eq!(excerpts.excerpts.prompts[0].text, "wire the survey");
    assert_eq!(excerpts.excerpts.said.as_deref(), Some("On it."));
    assert_eq!(excerpts.task, None, "no task file named");
    std::fs::remove_file(claude.join(format!("sessions/{PID}.json"))).unwrap();
    let nothing = Asks {
        files: |_| None,
        tap: NO_TAP,
        args: |_| unreachable!("no registry row: the process is not read"),
        roster: || unreachable!("no facts: the roster is not asked"),
        home: scratch_home,
        route: |_, _, _| accounts::AuthRoute::Stored,
    };
    assert_eq!(read(nothing), (false, None));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_side_rides_beside_the_facts() {
    const PID: i32 = 4244;
    const IMAGE: u64 = 1_790_607_848;
    let (root, claude) = claude_home("footer-side", PID, IMAGE);
    let timeline = Arc::new(Mutex::new(SessionTimeline::default()));
    timeline.lock().unwrap().note_foreground_group(PID);
    let asks = Asks {
        files: |_| None,
        tap: NO_TAP,
        args: |_| Some(args_of(&["HOME=/home/someone"])),
        roster: || None,
        home: scratch_home,
        route: |_, _, _| accounts::AuthRoute::Outside,
    };
    let mut identity = unread(PID, &claude);
    let (mut tail, mut fold, mut memo) = (
        footer::TailCache::default(),
        footer::FooterCache::default(),
        RosterMemo::default(),
    );
    let mut read = || {
        let (moved, ..) = resolve_into(
            &timeline,
            &mut identity,
            &mut tail,
            &mut fold,
            &mut memo,
            asks,
        );
        let timeline = timeline.lock().unwrap();
        (
            moved.is_some(),
            Arc::clone(timeline.claude_side().expect("a side")),
            timeline.claude_footer().cloned().expect("facts"),
        )
    };
    let work = root.join("work");
    let (moved, first, facts) = read();
    assert!(moved, "the first facts");
    assert_eq!(first.cwd.as_deref(), Some(work.as_path()));
    assert_eq!(
        (first.repo_root.as_ref(), first.traffic.as_ref()),
        (None, None)
    );
    let (moved, again, same) = read();
    assert!(!moved, "nothing moved: nothing repaints");
    assert!(
        Arc::ptr_eq(&first, &again),
        "the same side, not a rebuilt one"
    );
    assert_eq!(same, facts);
    {
        let mut timeline = timeline.lock().unwrap();
        timeline.set_claude_side(PID, Some(Arc::new(FactSide::default())));
        assert_eq!(timeline.claude_footer(), Some(&facts));
        assert!(!timeline.set_claude_footer(PID, None, Some(facts.clone())));
        timeline.set_claude_side(PID, Some(Arc::clone(&first)));
    }
    std::fs::create_dir_all(work.join(".git")).unwrap();
    std::fs::write(work.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
    let (_, rooted, _) = read();
    assert!(!Arc::ptr_eq(&first, &rooted));
    assert_eq!(rooted.repo_root.as_deref(), Some(work.as_path()));
    std::fs::write(
        super::rig::transcript_of(&root, &claude),
        concat!(
            r#"{"type":"assistant","timestamp":"2026-10-19T12:00:05Z","message":{"id":"m1","model":"claude-opus-5-5","usage":{"input_tokens":10,"output_tokens":7,"cache_read_input_tokens":990}}}"#,
            "\n",
        ),
    )
    .unwrap();
    let (_, graphed, _) = read();
    assert!(!Arc::ptr_eq(&rooted, &graphed));
    let traffic = graphed.traffic.as_ref().expect("its traffic");
    assert_eq!(
        (traffic.totals, &*traffic.buckets),
        ((1_000, 7), &[(1_000, 7)][..])
    );
    let (moved, held, _) = read();
    assert!(
        !moved && Arc::ptr_eq(&graphed, &held),
        "held while nothing moved"
    );
    let elsewhere = root.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::fs::write(
        claude.join(format!("sessions/{PID}.json")),
        format!(
            r#"{{"pid":{PID},"sessionId":"{}","cwd":{},"startedAt":{},"version":"2.1.287"}}"#,
            super::rig::SESSION_ID,
            aterm_json::to_string(&elsewhere.display().to_string()).unwrap(),
            IMAGE * 1000 + 684
        ),
    )
    .unwrap();
    let (_, moved_side, _) = read();
    assert!(!Arc::ptr_eq(&held, &moved_side));
    assert_eq!(moved_side.cwd.as_deref(), Some(elsewhere.as_path()));
    let mut timeline = timeline.lock().unwrap();
    timeline.set_claude_side(PID + 1, Some(Arc::new(FactSide::default())));
    assert!(Arc::ptr_eq(
        timeline.claude_side().expect("still this group's"),
        &moved_side
    ));
    timeline.note_foreground_group(PID + 1);
    assert!(timeline.claude_side().is_none());
    drop(timeline);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_login_is_the_registered_images() {
    const PID: i32 = 4242;
    const IMAGE: u64 = 1_790_607_848;
    let (root, claude) = claude_home("footer-login", PID, IMAGE);
    let listed = |args| Asks {
        files: |_| None,
        tap: NO_TAP,
        args,
        roster: || Some(two_accounts()),
        home: scratch_home,
        route: |_, _, _| accounts::AuthRoute::Stored,
    };
    let mut memo = RosterMemo::default();
    let mut identity = Identity {
        login: login_of_args(on_main(PID).as_ref()),
        argv: vec!["shim".into()],
        ..unread(PID, &claude)
    };
    assert_eq!(
        account_after(&mut identity, &mut memo, listed(on_work)),
        Some(Some("work-2".into())),
        "the first row: read again"
    );
    assert_eq!(identity.launch_for, Some(Some(IMAGE)));
    assert_eq!(
        identity.argv,
        ["claude", "--resume"],
        "its command line too"
    );
    let same = listed(|_| unreachable!("the image was read: not again"));
    assert_eq!(
        account_after(&mut identity, &mut memo, same),
        Some(Some("work-2".into()))
    );
    let _ = claude_home("footer-login", PID, IMAGE + 60);
    assert_eq!(
        account_after(&mut identity, &mut memo, listed(on_main)),
        Some(Some("main".into())),
        "a new image: read again"
    );
    assert_eq!(identity.launch_for, Some(Some(IMAGE + 60)));
    assert_eq!(identity.argv, ["claude"]);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn only_plain_rule_glyphs_are_coverable() {
    let mut s = RenderInput::empty();
    s.rows = 1;
    s.cols = 20;
    let mut row = row_of(&"\u{2500}".repeat(20), [60, 60, 60]);
    row[3].fg = [255, 0, 0]; // a label's ink
    row[6].bg = [0, 0, 200]; // a selection
    s.cells = vec![row];
    s.clusters = vec![vec![(9, "e\u{301}".into())]];
    s.combining = vec![Vec::new()];
    s.images = vec![Vec::new()];
    let (mask, rule) = rule_mask(&s, 0, 0, 20).unwrap();
    assert_eq!(rule.fg, [60, 60, 60]);
    let mut lit = s.clone();
    for c in &mut lit.cells[0][17..] {
        c.bg = [0, 0, 200];
    }
    let (mask_lit, rule_lit) = rule_mask(&lit, 0, 0, 20).unwrap();
    assert_eq!(
        rule_lit.bg,
        [0, 0, 0],
        "the style most of the rule is drawn in"
    );
    assert_eq!(footer::rule_run(&mask_lit), Some(10..17));
    let ink: Vec<usize> = (0..20).filter(|&c| !mask[c]).collect();
    assert_eq!(ink, [3, 6, 9]);
    assert_eq!(footer::rule_run(&mask), Some(10..20));
    s.cells[0].truncate(12);
    let (mask, _) = rule_mask(&s, 0, 0, 20).unwrap();
    assert!(!mask[12] && mask[11]);
    s.cells[0] = row_of("  hello", [1, 1, 1]);
    assert!(rule_mask(&s, 0, 0, 20).is_none());
}

fn engine_fill(rows: usize) -> RenderInput {
    let mut s = RenderInput::empty();
    s.rows = rows;
    s.cols = 40;
    s.cells = (0..rows)
        .map(|r| row_of(&format!("engine row {r}"), [9, 9, 9]))
        .collect();
    s.clusters = (0..rows).map(|_| Vec::new()).collect();
    s.combining = (0..rows).map(|_| Vec::new()).collect();
    s.images = (0..rows).map(|_| Vec::new()).collect();
    s.snapshot_seq = 40;
    s.engine_fill_seq = 40;
    s.shifted_fill_seq = 40;
    s.terminal_id = 7;
    s.extract_gen = 3;
    s.row_rev_lane = 7;
    s.row_rev = (0..rows).map(|r| 100 + r as u64).collect();
    s
}

fn written(
    s: &mut RenderInput,
    edits: Vec<(usize, usize, Vec<RenderCell>, RenderCell)>,
) -> FooterUndo {
    let mut slot = FooterUndo::default();
    write_edits(
        &mut slot,
        s,
        edits
            .iter()
            .map(|(row, col, cells, ground)| (*row, *col, &cells[..], *ground)),
    );
    slot
}

#[test]
fn a_footer_written_again_allocates_nothing() {
    let mut s = engine_fill(4);
    let original = s.cells.clone();
    let painted = row_of("  \u{25C6} Opus 5.5 xhigh", [1, 1, 1]);
    let chip = row_of("\u{221e} \u{23F8} plan", [2, 2, 2]);
    let ground = cell(' ', [0, 0, 0]);
    let edits = [(3, 0, &painted[..], ground), (2, 30, &chip[..], ground)];
    let mut slot = FooterUndo::default();
    let ((), fresh) = crate::work_counts::allocations_in(|| {
        write_edits(&mut slot, &mut s, edits);
    });
    assert!(fresh > 0, "CONTROL: a fresh slot allocates its rows");
    let first = s.cells.clone();
    for frame in 0..3 {
        assert!(undo(&mut slot, &mut s), "frame {frame}");
        assert_eq!(s.cells, original, "frame {frame}: undone exactly");
        let ((), again) = crate::work_counts::allocations_in(|| {
            write_edits(&mut slot, &mut s, edits);
        });
        assert_eq!(again, 0, "frame {frame}: the rows' buffers reused");
        assert_eq!(s.cells, first, "frame {frame}: the same write");
    }
}

#[test]
fn unicode_footer_side_channels_undo_exactly_reuse_buffers_and_respect_foreign_writes() {
    let blank = cell(' ', [0, 0, 0]);
    let rule = cell('\u{2500}', [60, 60, 60]);
    let facts = FooterFacts {
        account: Some("jose\u{0301}👩‍💻".into()),
        subscription: Some(aterm_agent::harness::profile::SubscriptionDisplay {
            email: Some("jose\u{0301}👩‍💻@example.com".into()),
            plan: Some("Pro".into()),
            weekly: Some("weekly 29% left".into()),
            ..Default::default()
        }),
        ..FooterFacts::default()
    };
    let fit = footer::fit_rule(&facts, 76, None);
    let glass = rule_glass(None, rule, blank);
    let paint = compose_rule_spans(
        &fit,
        None,
        rule,
        blank,
        (&glass, rule_at_room(76)),
        76,
        None,
    );
    assert!(!paint.clusters.is_empty() && !paint.combining.is_empty());
    let mut scratch = engine_fill(3);
    scratch.cells[2] = vec![rule; 100];
    scratch.clusters.resize_with(3, Vec::new);
    scratch.combining.resize_with(3, Vec::new);
    scratch.combining[0].push((0, vec!['\u{0300}'].into_boxed_slice()));
    let original = scratch.clone();
    let mut slot = FooterUndo::default();
    let write = |slot: &mut FooterUndo, scratch: &mut RenderInput| {
        write_edits(
            slot,
            scratch,
            rule_edits(2, 10, &paint.cells, &paint.glass, blank),
        );
        slot.publish_text(scratch, 2, 10, &paint);
    };
    write(&mut slot, &mut scratch);
    let painted = scratch.clone();
    for frame in 0..3 {
        assert!(undo(&mut slot, &mut scratch), "frame {frame}");
        assert_eq!(
            scratch, original,
            "cells, metadata and reuse identity restored"
        );
        let ((), allocations) =
            crate::work_counts::allocations_in(|| write(&mut slot, &mut scratch));
        assert_eq!(allocations, 0, "frame {frame}: metadata buffers reused");
        assert_eq!(scratch, painted);
    }
    scratch.combining[2][0].1 = vec!['\u{0300}'].into_boxed_slice();
    let changed = scratch.clone();
    assert!(
        !undo(&mut slot, &mut scratch),
        "another writer changed our mark"
    );
    assert_eq!(
        scratch, changed,
        "foreign metadata and cells are left alone"
    );

    let mut occupied = original;
    occupied.combining[2].push((
        10 + paint.combining[0].0,
        vec!['\u{0300}'].into_boxed_slice(),
    ));
    let before = occupied.clone();
    write(&mut slot, &mut occupied);
    assert_eq!(
        occupied, before,
        "a skipped vendor span gets no footer metadata"
    );
    assert!(!slot.is_written());
}

#[test]
fn the_undo_gives_back_the_engine_fill_exactly() {
    let mut s = engine_fill(4);
    let original = s.cells.clone();
    let painted = row_of("  \u{25C6} Opus 5.5 xhigh", [1, 1, 1]);
    let mut slot = written(&mut s, vec![(3, 0, painted.clone(), cell(' ', [0, 0, 0]))]);
    assert!(slot.is_written());
    assert_eq!(&s.cells[3][..painted.len()], painted.as_slice());
    assert_ne!(s.snapshot_seq, s.engine_fill_seq, "the write is declared");
    assert_eq!(
        s.shifted_fill_seq, s.snapshot_seq,
        "an untouched engine fill keeps the renderer's stamp compare"
    );
    assert_eq!(s.row_rev[3], 0, "the written row is compared by content");
    assert_eq!(s.row_rev[2], 102, "every other row keeps its stamp");
    assert!(undo(&mut slot, &mut s));
    assert!(!slot.is_written(), "consumed");
    assert_eq!(s.cells, original, "cells and trimmed lengths restored");
    assert_eq!(s.snapshot_seq, 40);
    assert_eq!(s.engine_fill_seq, 40);
    assert_eq!(s.shifted_fill_seq, 40);
    assert_eq!(s.row_rev, vec![100, 101, 102, 103]);
}

#[test]
fn a_scratch_touched_since_is_left_alone() {
    let painted = row_of("  footer", [1, 1, 1]);
    let mut s = engine_fill(3);
    let mut slot = written(&mut s, vec![(2, 0, painted.clone(), cell(' ', [0, 0, 0]))]);
    s.snapshot_seq += 1; // another host write
    let before = (s.cells.clone(), s.snapshot_seq, s.row_rev.clone());
    assert!(!undo(&mut slot, &mut s));
    assert_eq!((s.cells.clone(), s.snapshot_seq, s.row_rev.clone()), before);
    assert!(!slot.is_written(), "a stale record is dropped");

    let mut s = engine_fill(3);
    let mut slot = written(&mut s, vec![(2, 0, painted, cell(' ', [0, 0, 0]))]);
    s.cells[2][3].ch = 'X';
    assert!(
        !undo(&mut slot, &mut s),
        "the painted span must still be ours"
    );
    assert_eq!(s.cells[2][3].ch, 'X');
}

#[test]
fn a_rule_with_chips_is_written_where_it_changes() {
    let rule = cell('\u{2500}', [5, 5, 5]);
    let ground = cell(' ', [0, 0, 0]);
    let fill = || {
        let mut s = engine_fill(3);
        s.cells[2] = vec![rule; 48];
        s
    };
    let mut cells = row_of(" \u{221e} \u{23F8} plan ", [2, 2, 2]);
    let glass = cells.len()..cells.len() + 24;
    cells.extend(std::iter::repeat_n(rule, glass.len()));
    cells.extend(row_of(" Opus 5.5 ", [3, 3, 3]));
    let col = 48 - 1 - cells.len();
    let split_write = |s: &mut RenderInput, glass: &std::ops::Range<usize>| {
        let mut slot = FooterUndo::default();
        let _ = crate::work_counts::take_footer_cells_written();
        write_edits(&mut slot, s, rule_edits(2, col, &cells, glass, ground));
        (slot, crate::work_counts::take_footer_cells_written())
    };
    let original = fill().cells;
    let mut whole = fill();
    let _ = crate::work_counts::take_footer_cells_written();
    let mut whole_slot = written(&mut whole, vec![(2, col, cells.clone(), ground)]);
    let whole_cost = crate::work_counts::take_footer_cells_written();
    let mut split = fill();
    let (mut slot, cost) = split_write(&mut split, &glass);
    assert_eq!(split.cells, whole.cells, "the same glass");
    assert_eq!(
        (split.snapshot_seq, &split.row_rev),
        (whole.snapshot_seq, &whole.row_rev),
        "the same tokens"
    );
    assert_eq!(cost, cells.len() - glass.len(), "the chips and the facts");
    assert_eq!(slot.rewritten_as_they_were(), 0);
    assert_eq!(whole_cost, cells.len(), "CONTROL: whole, every cell");
    assert_eq!(
        whole_slot.rewritten_as_they_were(),
        glass.len(),
        "CONTROL: whole, the middle written over itself"
    );
    assert!(undo(&mut slot, &mut split));
    assert!(undo(&mut whole_slot, &mut whole));
    assert_eq!((&split.cells, &whole.cells), (&original, &original));
    let mut bare = fill();
    let (_, cost) = split_write(&mut bare, &(0..0));
    assert_eq!(cost, cells.len());
    let (mut whole, mut split) = (fill(), fill());
    for s in [&mut whole, &mut split] {
        s.cells[2][col + glass.start + 3] = cell('x', [5, 5, 5]);
    }
    let _ = written(&mut whole, vec![(2, col, cells.clone(), ground)]);
    let _ = split_write(&mut split, &glass);
    assert_ne!(
        split.cells, whole.cells,
        "CONTROL: the skip keeps what is there"
    );
}

#[test]
fn two_writes_on_one_row_come_off_in_order() {
    let mut s = engine_fill(2);
    let original = s.cells.clone();
    let left = row_of("  left footer      ", [1, 1, 1]);
    let right = row_of("  right footer", [2, 2, 2]);
    let ground = cell(' ', [0, 0, 0]);
    let mut slot = written(&mut s, vec![(1, 0, left, ground), (1, 20, right, ground)]);
    assert_eq!(s.cells[1].len(), 34, "padded to the right pane's edge");
    assert!(undo(&mut slot, &mut s));
    assert_eq!(s.cells, original);
}

#[test]
fn a_span_in_the_rule_row_comes_off_exactly() {
    let mut s = engine_fill(4);
    s.cells[2] = row_of(&"\u{2500}".repeat(40), [60, 60, 60]);
    let original = s.cells.clone();
    let facts = footer::fit_rule(&facts(), 36, None);
    let blank = cell(' ', [200, 200, 200]);
    let (cells, _) = compose_rule(&facts, None, s.cells[2][0], blank, None, 36);
    let col = 40 - 1 - cells.len();
    let mut slot = written(&mut s, vec![(2, col, cells.clone(), blank)]);
    assert_eq!(s.cells[2].len(), 40, "nothing padded");
    assert_eq!(&s.cells[2][col..39], cells.as_slice());
    assert_eq!(
        s.cells[2][39], original[2][39],
        "the rule's last glyph stays"
    );
    assert!(s.cells[2][..col].iter().all(|c| *c == original[2][0]));
    assert_eq!(s.row_rev[2], 0, "the rule row is compared by content");
    assert!(undo(&mut slot, &mut s));
    assert_eq!(s.cells, original);
    assert_eq!(s.row_rev, vec![100, 101, 102, 103]);
    let mut s = engine_fill(2);
    let original = s.cells.clone();
    let chip = cell('\u{23F8}', [9, 9, 9]);
    let mut slot = written(&mut s, vec![(1, 30, vec![chip], blank)]);
    assert!(
        s.cells[1][12..30].iter().all(|c| *c == blank),
        "{:?}",
        &s.cells[1][12..30]
    );
    assert!(undo(&mut slot, &mut s));
    assert_eq!(s.cells, original);
}

#[test]
fn a_span_with_side_channel_content_is_not_written() {
    let mut s = engine_fill(2);
    s.clusters[1].push((4, "e\u{301}".into()));
    let before = s.cells.clone();
    let slot = written(
        &mut s,
        vec![(1, 0, row_of("  footer", [1, 1, 1]), cell(' ', [0, 0, 0]))],
    );
    assert!(!slot.is_written());
    assert_eq!(s.cells, before);
    assert_eq!(s.snapshot_seq, 40, "nothing written, nothing declared");
}

#[test]
fn only_an_untouched_engine_fill_keeps_its_stamps() {
    let mut s = engine_fill(2);
    s.snapshot_seq = 41; // e.g. the find bar, earlier this frame
    let mut slot = written(
        &mut s,
        vec![(1, 0, row_of("  footer", [1, 1, 1]), cell(' ', [0, 0, 0]))],
    );
    assert_eq!(s.shifted_fill_seq, 40, "not re-armed");
    assert_eq!(s.row_rev[1], 101, "not cleared");
    assert!(undo(&mut slot, &mut s));
    assert_eq!(s.snapshot_seq, 41, "back to the other write's own bump");
}

#[test]
fn a_blessed_chrome_prepend_keeps_the_stamps_and_its_inverse() {
    let mut s = engine_fill(3);
    let engine_cells = s.cells.clone();
    let blessed = s.host_prepend_blessing();
    assert!(blessed.keeps_lane());
    s.cells.insert(0, row_of("strip", [7, 7, 7]));
    s.clusters.insert(0, Vec::new());
    s.combining.insert(0, Vec::new());
    s.images.insert(0, Vec::new());
    s.rows += 1;
    s.snapshot_seq += 1;
    s.note_host_row_prepend(1, blessed);
    let mut slot = written(
        &mut s,
        vec![(3, 0, row_of("  footer", [1, 1, 1]), cell(' ', [0, 0, 0]))],
    );
    assert_eq!(
        s.shifted_fill_seq, s.snapshot_seq,
        "the stamp compare stays on under the strip"
    );
    assert_eq!(s.row_rev[3], 0, "the written row is compared by content");
    assert!(undo(&mut slot, &mut s));
    let mut pool = Vec::new();
    assert!(
        s.undo_host_row_prepend(3, &mut pool, 1),
        "the strip's inverse still runs"
    );
    assert_eq!(s.cells, engine_cells);
    assert_eq!(s.snapshot_seq, s.engine_fill_seq);
}

/// A drifted mode row reaches aterm.log by its shape: its first glyph (the pill a fixture
/// needs), its cells and its length, the build only where it is a word. The row is the
/// session's screen (a task's name, an IDE selection), so its words never do (DESIGN §3.6).
#[test]
fn a_drifted_mode_row_reaches_the_log_by_its_shape() {
    let screen = |mode_row: &str| -> Vec<String> {
        let rule = "\u{2500}".repeat(60);
        vec![rule.clone(), "\u{276F} ".into(), rule, mode_row.into()]
    };
    let row = "  \u{23F5}\u{23F5} QZP7E7B1 mode on (shift+tab to cycle)";
    let rows = screen(row);
    assert_eq!(
        unread_pill_row(&rows).as_deref(),
        Some(row),
        "CONTROL: the row is drift"
    );
    let what = "a mode row this aterm does not read, so the lights stand down on it";
    let shape = format!(
        "first=U+23F5 cols={} row={}B",
        footer::text_width(row),
        row.len()
    );
    for (version, build) in [(Some("2.1.999"), "2.1.999"), (Some("QZP7E7B2 build"), "?")] {
        let logged = crate::test_log::logged_while(|| {
            note_vendor_drift_in(&Mutex::default(), 0xE7B, version, &rows);
        });
        assert_eq!(
            logged,
            [format!(
                "claude footer: Claude Code {build} drew {what}: {shape}"
            )],
            "the row's shape and the build's word"
        );
    }
}

#[test]
fn only_an_unreadable_pill_row_is_drift() {
    let screen = |mode_row: &str| -> Vec<String> {
        let rule = "\u{2500}".repeat(60);
        vec![rule.clone(), "\u{276F} ".into(), rule, mode_row.into()]
    };
    let renamed = "  \u{23F5}\u{23F5} turbo mode on (shift+tab to cycle)";
    assert_eq!(unread_pill_row(&screen(renamed)).as_deref(), Some(renamed));
    for fine in [
        "  \u{23F5}\u{23F5} auto mode on (shift+tab to cycle)",
        "  \u{23F8} manual mode on \u{00B7} ? for shortcuts",
        "  ? for shortcuts",
        "  \u{23F5}\u{23F5} bypass permissi",
    ] {
        assert_eq!(unread_pill_row(&screen(fine)), None, "{fine:?}");
    }
    assert_eq!(unread_pill_row(&["$ ls".to_owned()]), None, "no composer");
}

fn claude_rows(name: &str, cols: usize) -> Vec<String> {
    let path = aterm_tempfile::manifest_dir()
        .join("../aterm-phase/src/fixtures")
        .join(format!("claude-2.1.283-{name}.txt"));
    let rows: Vec<String> = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
        .lines()
        .skip(1)
        .map(|row| {
            let rule = row.starts_with('\u{2500}') && row.trim_end().ends_with('\u{2500}');
            if !rule {
                return row.to_owned();
            }
            let label: String = row.trim_start_matches('\u{2500}').to_owned();
            let n = cols - label.chars().count();
            format!("{}{label}", "\u{2500}".repeat(n))
        })
        .collect();
    assert!(rows.len() <= 24, "{name}: an 80x24 capture");
    rows
}

fn painted(row: &str) -> String {
    const RULE: &str = "\x1b[38;2;90;90;90m";
    const LABEL: &str = "\x1b[38;2;200;120;255m";
    const PILL: &str = "\x1b[38;2;255;110;90m";
    const DIM: &str = "\x1b[38;2;130;130;130m";
    if row.starts_with('\u{2500}') {
        let rule: String = row.chars().take_while(|c| *c == '\u{2500}').collect();
        let rest = &row[rule.len()..];
        let trail = rest
            .chars()
            .rev()
            .take_while(|c| *c == '\u{2500}')
            .map(char::len_utf8)
            .sum::<usize>();
        let (label, end) = rest.split_at(rest.len() - trail);
        return format!("{RULE}{rule}{LABEL}{label}{RULE}{end}\x1b[0m");
    }
    if footer::is_mode_row(row) {
        let at = row.find(" on").map_or(row.len(), |i| i + 3);
        return format!("{PILL}{}{DIM}{}\x1b[0m", &row[..at], &row[at..]);
    }
    row.to_owned()
}

fn feed_rows(app: &App, session: u64, rows: &[String]) {
    let mut out = String::from("\x1b[?2004h\x1b[2J");
    for (i, row) in rows.iter().enumerate() {
        out.push_str(&format!("\x1b[{};1H{}", i + 1, painted(row)));
    }
    let caret = rows
        .iter()
        .rposition(|r| r.starts_with(['\u{276F}', '>']))
        .unwrap_or(0);
    out.push_str(&format!("\x1b[{};3H", caret + 1));
    let term = app.pool.get(session).expect("session").term.clone();
    crate::term_lock(&term).process(out.as_bytes());
}

fn publish(app: &App, session: u64, facts: FooterFacts) {
    let entry = app.pool.get(session).expect("session");
    let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
    timeline.note_foreground_group(4242);
    assert!(timeline.set_claude_footer(4242, None, Some(facts)));
}

fn frame(app: &mut App, wid: WindowId) {
    frame_with_clock(app, wid, None);
}

fn frame_with_clock(
    app: &mut App,
    wid: WindowId,
    clock: Option<&dyn Fn() -> footer::ReadClock<'static>>,
) {
    let prepared = app.prepare_terminal_capture_grid_with_cursor_fx_and_plan_outcome(
        wid,
        crate::app_render::ComposedCursorFxClock::Advance(std::time::Instant::now()),
    );
    let crate::app_render::CapturePreparation::Ready((grid, plan)) = prepared else {
        panic!("the headless capture must produce a frame");
    };
    app.splice_tab_strip(wid);
    let route = VisibleContentRoute::Terminal {
        composed: grid.composed,
    };
    match clock {
        Some(clock) => app.splice_claude_footer_at(wid, &plan, route, clock),
        None => app.splice_claude_footer(wid, &plan, route),
    }
}

fn glass(cols: u16) -> (App, WindowId, u64) {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let session = app.focused_session_id(wid).expect("the front session");
    if cols != 80 {
        assert!(app.apply_term_resize(wid, 24, cols));
    }
    (app, wid, session)
}

fn strip(app: &App, wid: WindowId) -> usize {
    app.windows[&wid].input_scratch.cells.len() - usize::from(app.windows[&wid].rows)
}

#[test]
fn the_facts_go_into_the_rule_and_claudes_row_stays_whole() {
    let footer_text = " \u{25C6} Opus 5.5 xhigh   \u{2302} ~/aterm   \u{2387} main \u{2500}";
    for (name, cols, default_mode) in [
        ("footer-bypass-idle", 80, false),
        ("footer-bypass-idle", 131, false),
        ("footer-bypass-idle", 150, false),
        ("footer-busy", 80, false),
        ("footer-busy", 131, false),
        ("footer-plan", 80, false),
        ("footer-manual", 150, false),
        ("footer-accept-edits", 80, false),
        ("footer-auto-draft", 131, false),
        ("footer-bypass-idle", 80, true),
    ] {
        let label = format!(
            "{name}@{cols}{}",
            if default_mode { " (default mode)" } else { "" }
        );
        let (mut app, wid, session) = glass(cols);
        let mut rows = claude_rows(name, usize::from(cols));
        let bottom = aterm_phase::phase::composer_bottom(&rows).expect("the composer");
        if default_mode {
            rows[bottom + 1] = "  ? for shortcuts".into();
        }
        feed_rows(&app, session, &rows);
        frame(&mut app, wid);
        let strip = strip(&app, wid);
        let vendor: Vec<Vec<RenderCell>> = app.windows[&wid].input_scratch.cells.clone();
        publish(&app, session, facts());
        frame(&mut app, wid);
        let glass = &app.windows[&wid].input_scratch.cells;
        let rule = &glass[strip + bottom];
        assert!(
            text_of(rule).trim_end().ends_with(footer_text),
            "{label}: {:?}",
            text_of(rule)
        );
        let at = rule.iter().position(|c| c.ch == '\u{25C6}').unwrap();
        assert_eq!(rule.len(), usize::from(cols), "{label}");
        let set = footer_text.trim_end_matches('\u{2500}');
        assert_eq!(
            at,
            usize::from(cols) - cells_for(set),
            "{label}: right-aligned, the rule's last glyph after it"
        );
        let o = rule.iter().position(|c| c.ch == 'O').unwrap();
        let glyph = *vendor[strip + bottom].last().unwrap();
        let blank = app.windows[&wid].input_scratch.implicit_blank;
        assert_eq!(
            rule[o].fg,
            rgb_of(rule_glass(None, glyph, blank).value()),
            "{label}: the value, in the glass's value ink"
        );
        assert_eq!(
            rule[o].script,
            aterm_core::terminal::Script::Normal,
            "{label}: terminal text"
        );
        for (c, (was, now)) in vendor[strip + bottom].iter().zip(rule).enumerate() {
            if was != now {
                assert_eq!(*was, glyph, "{label}: col {c} was not a plain rule glyph");
            }
        }
        assert_eq!(
            rule.last(),
            Some(&glyph),
            "{label}: the rule's last glyph stays"
        );
        for r in bottom + 1..rows.len() {
            assert_eq!(
                glass[strip + r],
                vendor[strip + r],
                "{label}: row {r} under the rule is Claude's, every cell"
            );
        }
        for r in 0..bottom {
            assert_eq!(glass[strip + r], vendor[strip + r], "{label}: row {r}");
        }
    }
    // Give the usage toggle room alongside the standing recovery disclosure.
    let (mut app, wid, session) = glass(256);
    let rows = claude_rows("footer-bypass-idle", 256);
    let bottom = aterm_phase::phase::composer_bottom(&rows).expect("the composer");
    feed_rows(&app, session, &rows);
    publish(&app, session, facts_with_usage());
    frame(&mut app, wid);
    let rule =
        |app: &App| text_of(&app.windows[&wid].input_scratch.cells[strip(app, wid) + bottom]);
    let shown = rule(&app);
    assert!(
        shown.contains('\u{29D7}') && shown.contains('\u{03A3}'),
        "{shown:?}"
    );
    app.config.claude_footer = Some(crate::app_config::ClaudeFooterConfig {
        usage: Some(false),
        ..Default::default()
    });
    frame(&mut app, wid);
    let hidden = rule(&app);
    assert!(
        hidden.contains('\u{29D7}') && !hidden.contains('\u{03A3}'),
        "{hidden:?}"
    );
}

#[test]
fn no_composer_no_footer() {
    let (mut app, wid, session) = glass(80);
    let boxed: Vec<String> = [
        "\u{25CF} done",
        "",
        &"\u{2594}".repeat(80),
        "   Change effort level?",
        "",
        "   \u{276F} 1. Yes, switch to high",
        "     2. No, go back",
    ]
    .iter()
    .map(|r| (*r).to_owned())
    .collect();
    feed_rows(&app, session, &boxed);
    publish(&app, session, facts());
    frame(&mut app, wid);
    assert!(
        !app.windows[&wid]
            .input_scratch
            .cells
            .iter()
            .any(|row| row.iter().any(|c| c.ch == '\u{25C6}')),
        "no footer anywhere"
    );
    feed_rows(&app, session, &claude_rows("footer-bypass-idle", 80));
    frame(&mut app, wid);
    assert!(
        app.windows[&wid]
            .input_scratch
            .cells
            .iter()
            .any(|row| row.iter().any(|c| c.ch == '\u{25C6}'))
    );
}

#[test]
fn a_split_panes_rule_carries_its_own_footer() {
    let (mut app, wid, session) = glass(100);
    let _other = app.split_active_stub_tab(wid);
    let (pane_rows, cols) = {
        let term = crate::term_lock(&app.pool.get(session).unwrap().term);
        (usize::from(term.rows()), usize::from(term.cols()))
    };
    assert!(cols < 60, "a half-width pane: {cols}");
    let last = usize::from(app.windows[&wid].rows) - 1;
    assert!(
        pane_rows < last + 1,
        "a split pane gives a row to its title"
    );
    let rule = "\u{2500}".repeat(cols);
    let rows: Vec<String> = (0..pane_rows - 4)
        .map(|_| String::new())
        .chain([
            rule.clone(),
            "\u{276F} ".to_owned(),
            rule,
            "  \u{23F5}\u{23F5} bypass permissions on".to_owned(),
        ])
        .collect();
    feed_rows(&app, session, &rows);
    frame(&mut app, wid);
    let strip = strip(&app, wid);
    let before = app.windows[&wid].input_scratch.cells[strip + last - 1].clone();
    let mode_before = app.windows[&wid].input_scratch.cells[strip + last].clone();
    let deep = FooterFacts {
        path: Some("~/src/github.com/someone/aterm".into()),
        ..facts()
    };
    publish(&app, session, deep);
    frame(&mut app, wid);
    let row = &app.windows[&wid].input_scratch.cells[strip + last - 1];
    let text = text_of(&row[..cols]);
    assert!(
        text.ends_with(
            " \u{25C6} Opus 5.5 xhigh   \u{2302} \u{2026}/aterm   \u{2387} main \u{2500}"
        ),
        "{text:?}"
    );
    assert_eq!(&row[cols..], &before[cols..], "the sibling's cells");
    assert_eq!(
        app.windows[&wid].input_scratch.cells[strip + last],
        mode_before,
        "Claude's row"
    );
}

#[test]
fn a_launch_card_fills_only_an_open_footer() {
    let (mut app, wid, session) = glass(80);
    let owner = footer::FactsOwner {
        floor: Some(1_790_607_848),
        session_id: "00000000-0000-4000-8000-000000000004".into(),
    };
    let open = FooterFacts {
        path: Some("~/aterm".into()),
        version: Some("2.1.283".into()),
        model_open: true,
        effort_open: true,
        owner: Some(owner.clone()),
        ..FooterFacts::default()
    };
    let rows = claude_rows("footer-bypass-idle", 80);
    let bottom = aterm_phase::phase::composer_bottom(&rows).unwrap();
    super::probe::drawer(&app, session, 4242, "claude", "2.1.283");
    feed_rows(&app, session, &rows);
    publish(&app, session, open.clone());
    let plan = app.active_visible_leaf_plan(wid).unwrap();
    let fp_before = app.claude_footer_fp(wid, &plan);
    frame(&mut app, wid);
    let strip = strip(&app, wid);
    let rule = |app: &App| text_of(&app.windows[&wid].input_scratch.cells[strip + bottom]);
    assert!(
        rule(&app).ends_with(" \u{25C6} Opus 5.5 xhigh   \u{2302} ~/aterm \u{2500}"),
        "{:?}",
        rule(&app)
    );
    assert_ne!(
        app.claude_footer_fp(wid, &plan),
        fp_before,
        "noting it repaints"
    );
    let mut scrolled = rows.clone();
    for r in scrolled.iter_mut().take(5) {
        r.clear();
    }
    feed_rows(&app, session, &scrolled);
    frame(&mut app, wid);
    assert!(
        rule(&app).contains("\u{25C6} Opus 5.5 xhigh"),
        "{:?}",
        rule(&app)
    );
    let entry = app.pool.get(session).unwrap();
    {
        let mut timeline = entry.ctx.timeline.lock().unwrap();
        assert!(timeline.set_claude_footer(
            4242,
            None,
            Some(FooterFacts {
                owner: Some(footer::FactsOwner {
                    floor: Some(1_790_607_999),
                    ..owner.clone()
                }),
                ..open.clone()
            })
        ));
        assert!(timeline.claude_card().is_none());
    }
    frame(&mut app, wid);
    assert!(!rule(&app).contains('\u{25C6}'), "{:?}", rule(&app));
    feed_rows(&app, session, &rows);
    publish(
        &app,
        session,
        FooterFacts {
            model: Some("Fable 5.1".into()),
            model_open: false,
            ..open.clone()
        },
    );
    frame(&mut app, wid);
    assert!(
        rule(&app).contains("\u{25C6} Fable 5.1 xhigh"),
        "{:?}",
        rule(&app)
    );
    let (mut app, wid, session) = glass(80);
    super::probe::drawer(&app, session, 4242, "claude", "2.1.284");
    feed_rows(&app, session, &rows);
    publish(
        &app,
        session,
        FooterFacts {
            version: None,
            ..open
        },
    );
    frame(&mut app, wid);
    let text = text_of(&app.windows[&wid].input_scratch.cells[strip + bottom]);
    assert!(!text.contains("Opus"), "{text:?}");
    let entry = app.pool.get(session).unwrap();
    assert!(
        entry.ctx.timeline.lock().unwrap().claude_card().is_none(),
        "not kept"
    );
}

/// CLAUDE'S EFFORT HINT FILLS AN OPEN EFFORT: with the transcript naming no effort yet,
/// Claude's `◐ medium · /effort` on its mode row, drawn by the holder, gives `◆` its effort,
/// kept as the launch card's, with no card on the screen (a resume) or under a card that names
/// none, and `◆` keeps it once Claude drops the hint. CONTROL: an effort the transcript
/// decided is not filled, nor is one from a hint the holder did not draw; the rule names the
/// model in both.
#[test]
fn claudes_effort_hint_fills_an_open_effort() {
    let open = FooterFacts {
        model: Some("Opus 5.5".into()),
        path: Some("~/aterm".into()),
        version: Some("2.1.283".into()),
        effort_open: true,
        owner: Some(footer::FactsOwner {
            floor: Some(1_790_607_848),
            session_id: "00000000-0000-4000-8000-000000000007".into(),
        }),
        ..FooterFacts::default()
    };
    let mut carded = claude_rows("footer-bypass-idle", 80);
    let bottom = aterm_phase::phase::composer_bottom(&carded).unwrap();
    carded[bottom - 3].clear();
    let banner = carded
        .iter_mut()
        .find(|r| r.contains("with xhigh effort"))
        .expect("the card's model row");
    *banner = banner.replace(" with xhigh effort", "");
    // The frame with nothing above its composer: no launch card, as on a resume.
    let mut bare = carded.clone();
    for r in &mut bare[..bottom - 2] {
        r.clear();
    }
    let jobs = "  \u{23F5}\u{23F5} bypass permissions on \u{B7} 1 shell";
    let decided = FooterFacts {
        effort_open: false,
        ..open.clone()
    };
    for (what, screen, facts, holder) in [
        ("no card", &bare, open.clone(), true),
        ("a card naming none", &carded, open.clone(), true),
        ("CONTROL: decided", &bare, decided, true),
        ("CONTROL: not the holder's", &bare, open, false),
    ] {
        let mut hinted = screen.clone();
        hinted[bottom + 1] = format!("{jobs:<62}\u{25D0} medium \u{B7} /effort");
        let mut dropped = screen.clone();
        dropped[bottom + 1] = jobs.to_owned();
        let (mut app, wid, session) = glass(80);
        if holder {
            super::probe::drawer(&app, session, 4242, "claude", "2.1.283");
        }
        feed_rows(&app, session, &hinted);
        publish(&app, session, facts);
        frame(&mut app, wid);
        let strip = strip(&app, wid);
        let rule = |app: &App| text_of(&app.windows[&wid].input_scratch.cells[strip + bottom]);
        let kept = |app: &App| {
            let entry = app.pool.get(session).unwrap();
            let timeline = entry.ctx.timeline.lock().unwrap();
            timeline.claude_card().and_then(|card| card.effort.clone())
        };
        if what.starts_with("CONTROL") {
            assert!(
                rule(&app).contains("\u{25C6} Opus 5.5"),
                "{what}: {:?}",
                rule(&app)
            );
            assert!(!rule(&app).contains("medium"), "{what}: {:?}", rule(&app));
            assert_eq!(kept(&app), None, "{what}");
            continue;
        }
        assert!(
            rule(&app).contains("\u{25C6} Opus 5.5 medium"),
            "{what}: the hint's effort: {:?}",
            rule(&app)
        );
        assert_eq!(kept(&app).as_deref(), Some("medium"), "{what}: the card's");
        frame(&mut app, wid);
        assert_eq!(
            kept(&app).as_deref(),
            Some("medium"),
            "{what}: still, a frame on"
        );
        feed_rows(&app, session, &dropped);
        frame(&mut app, wid);
        assert!(
            rule(&app).contains("\u{25C6} Opus 5.5 medium"),
            "{what}: kept once the hint goes: {:?}",
            rule(&app)
        );
    }
}

/// A foreign change to a hint cannot erase the effort retained from the holder's hint,
/// even while its own unchanged launch card remains admitted. The real holder stop and
/// reselect fence keeps that card but excludes the foreign mode row. CONTROL: redrawing
/// the same hint as the holder admits it and updates the retained effort.
#[test]
fn a_foreign_hint_cannot_erase_the_kept_effort() {
    use aterm_core::terminal::{CutKind, HolderCut};

    let (mut app, wid, session) = glass(80);
    let mut rows = claude_rows("footer-bypass-idle", 80);
    let bottom = aterm_phase::phase::composer_bottom(&rows).unwrap();
    rows[bottom - 3].clear();
    let banner = rows
        .iter_mut()
        .find(|r| r.contains("with xhigh effort"))
        .expect("the card's model row");
    *banner = banner.replace(" with xhigh effort", "");
    let jobs = "  ⏵⏵ bypass permissions on · 1 shell";
    let hint = |level| format!("{jobs:<62}◐ {level} · /effort");
    rows[bottom + 1] = hint("medium");
    let token = super::probe::drawer(&app, session, 4242, "claude", "2.1.283");
    feed_rows(&app, session, &rows);
    publish(
        &app,
        session,
        FooterFacts {
            model: Some("Opus 5.5".into()),
            version: Some("2.1.283".into()),
            effort_open: true,
            owner: Some(footer::FactsOwner {
                floor: Some(1_790_607_848),
                session_id: "00000000-0000-4000-8000-000000000008".into(),
            }),
            ..FooterFacts::default()
        },
    );
    let kept = |app: &App| {
        let entry = app.pool.get(session).unwrap();
        let timeline = entry.ctx.timeline.lock().unwrap();
        timeline.claude_card().and_then(|card| card.effort.clone())
    };
    frame(&mut app, wid);
    assert_eq!(kept(&app).as_deref(), Some("medium"), "the holder's hint");
    let rewrite = format!("\x1b[{};1H\x1b[2K{}", bottom + 2, painted(&hint("high")));
    let term = app.pool.get(session).unwrap().term.clone();
    {
        let mut term = crate::term_lock(&term);
        term.process(format!("\x1b[{};1H", bottom + 2).as_bytes());
        term.note_holder_cut(HolderCut {
            to: None,
            kind: CutKind::Group,
            from_alive: true,
        });
        term.process(rewrite.as_bytes());
        term.process(format!("\x1b[{};80H", bottom + 2).as_bytes());
        term.note_holder_cut(HolderCut {
            to: Some(token),
            kind: CutKind::Group,
            from_alive: true,
        });
        let base = term.main_grid().visible_to_absolute(0);
        let holder = term.holder_view();
        assert!(
            holder.admits_rows(base + 1, base + 4),
            "the own card: {holder:?}"
        );
        let row = base + u64::try_from(bottom + 1).unwrap();
        assert!(
            !holder.admits_rows(row, row + 1),
            "the foreign hint: {holder:?}"
        );
    }
    frame(&mut app, wid);
    assert_eq!(
        kept(&app).as_deref(),
        Some("medium"),
        "a foreign hint preserves it"
    );
    crate::term_lock(&term).process(rewrite.as_bytes());
    frame(&mut app, wid);
    assert_eq!(
        kept(&app).as_deref(),
        Some("high"),
        "CONTROL: the holder's redraw"
    );
}

/// A DEAD HOLDER'S COMPOSER WEARS NO GLASS: Claude Code exits, leaving its composer (its rules,
/// its raw mode row) with `Resume this session with:` and the shell's relaunch line under it;
/// the relaunched process is the supervised holder, its facts published, before it draws.
/// Until it does, the old composer's rows read exactly as the dead process drew them: no
/// lockup, no facts, its raw mode row, no icon; the new process's own composer below wears the
/// glass. CONTROLS: the dead holder's composer wore the glass while it held the tab; a fence
/// tainted at the cut, and a holder that is no drawer, keep the glass on the composer the new
/// process draws.
#[test]
fn a_dead_holders_composer_wears_no_glass() {
    use aterm_core::terminal::{CutKind, HolderCut};
    let rows = claude_rows("footer-bypass-idle", 80);
    let bottom = aterm_phase::phase::composer_bottom(&rows).unwrap();
    let top = bottom - 2;
    let last = rows.len() - 1;
    assert_eq!(bottom + 1, last, "the composer ends the screen");
    let facts = |version: &str, id: &str| FooterFacts {
        model: Some("Opus 5.5".into()),
        path: Some("~/aterm".into()),
        version: Some(version.into()),
        owner: Some(footer::FactsOwner {
            floor: Some(1_790_607_848),
            session_id: id.into(),
        }),
        ..FooterFacts::default()
    };
    // The composer `rows[top..=last]` drawn from the cursor down, the caret on its prompt.
    let composer = || {
        let mut out = String::new();
        for (i, row) in rows[top..=last].iter().enumerate() {
            if i > 0 {
                out.push_str("\r\n");
            }
            out.push_str(&painted(row));
        }
        out + &format!("\x1b[{};3H", top + 2)
    };
    for (what, cut) in [
        ("a drawer", Some(false)),
        ("CONTROL: a tainted fence", Some(true)),
        ("CONTROL: no drawer", None),
    ] {
        let (mut app, wid, session) = glass(80);
        let _host = super::probe::hosted(&mut app);
        let ctx = app.pool.get(session).unwrap().ctx.clone();
        crate::session_timeline::claim_supervisor(
            &ctx,
            &crate::harness_host::holder(),
            None,
            None,
            crate::metrics::now_us(),
        )
        .expect("claim");
        super::probe::drawer(&app, session, 4242, "claude", "2.1.283");
        ctx.timeline
            .lock()
            .unwrap()
            .set_program(4242, Some("claude".into()));
        feed_rows(&app, session, &rows);
        publish(
            &app,
            session,
            facts("2.1.283", "00000000-0000-4000-8000-00000000000c"),
        );
        frame(&mut app, wid);
        let strip = strip(&app, wid);
        let text = |app: &App, r: usize| {
            text_of(&app.windows[&wid].input_scratch.cells[strip + r])
                .trim_end()
                .to_owned()
        };
        let icon = |app: &App| {
            app.windows[&wid]
                .claude_card
                .painted_layers()
                .iter()
                .any(|l| l.id == top_mark_id(session) && !l.content.items().is_empty())
        };
        assert!(
            text(&app, top).contains(" claude ") && icon(&app),
            "{what}: CONTROL: the holder's composer wears the lockup: {:?}",
            text(&app, top)
        );
        assert!(
            text(&app, bottom).contains("\u{25C6} Opus 5.5"),
            "{what}: CONTROL: and the facts: {:?}",
            text(&app, bottom)
        );
        let term = app.pool.get(session).unwrap().term.clone();
        {
            // Claude Code's last words under its composer; it exits, and the shell takes the
            // tab back and runs the relaunch line.
            let mut t = crate::term_lock(&term);
            t.process(
                format!(
                    "\x1b[{};1H\r\nResume this session with:\r\nclaude --resume \
                     00000000-0000-4000-8000-00000000000c\r\n",
                    last + 1
                )
                .as_bytes(),
            );
            t.note_holder_cut(HolderCut {
                to: None,
                kind: CutKind::Group,
                from_alive: false,
            });
            t.process(b"% claude\r\n");
            if cut == Some(true) {
                // The cut lays the new holder's span on the relaunch line's own ink.
                t.process(format!("\x1b[{};1H", last).as_bytes());
            }
        }
        {
            let mut timeline = ctx.timeline.lock().unwrap();
            timeline.note_foreground_group(100);
            assert!(timeline.set_claude_footer(100, None, None));
        }
        if cut.is_some() {
            super::probe::drawer(&app, session, 4343, "claude", "2.1.290");
        }
        {
            let mut timeline = ctx.timeline.lock().unwrap();
            timeline.note_foreground_group(4343);
            timeline.set_program(4343, Some("claude".into()));
            assert!(timeline.set_claude_footer(
                4343,
                None,
                Some(facts("2.1.290", "00000000-0000-4000-8000-00000000000d"))
            ));
        }
        let holder = crate::term_lock(&term).holder_view();
        assert_eq!(
            (holder.token != 0, holder.tainted),
            (cut.is_some(), cut == Some(true)),
            "{what}: the fence at the cut: {holder:?}"
        );
        let moved = 4;
        if cut == Some(false) {
            frame(&mut app, wid);
            for (r, drawn) in rows.iter().enumerate().take(last + 1).skip(top) {
                assert_eq!(
                    text(&app, r - moved),
                    drawn.trim_end(),
                    "{what}: the dead holder's row {r} as it drew it"
                );
            }
            assert!(!icon(&app), "{what}: no lockup's icon over it");
        }
        crate::term_lock(&term).process(composer().as_bytes());
        frame(&mut app, wid);
        assert!(
            text(&app, bottom).contains("\u{25C6} Opus 5.5"),
            "{what}: the new holder's composer wears the facts: {:?}",
            text(&app, bottom)
        );
        if cut.is_some() {
            assert!(
                text(&app, top).contains(" claude ") && icon(&app),
                "{what}: and the lockup: {:?}",
                text(&app, top)
            );
        }
        if cut == Some(false) {
            let moved = moved + rows[top..=last].len() - 1;
            for (r, drawn) in rows.iter().enumerate().take(last + 1).skip(top) {
                assert_eq!(
                    text(&app, r - moved),
                    drawn.trim_end(),
                    "{what}: the dead holder's row {r} still as it drew it"
                );
            }
        }
    }
}

/// A LIVE HOLDER'S COMPOSER KEEPS ITS GLASS ACROSS A SUSPEND: Claude Code draws its composer,
/// the caret on its prompt, and is suspended (the shell's group in front, Claude alive) and
/// resumed (its own group back in front, the same drawer): the composer it drew still wears
/// the lockup and the facts. CONTROL: resumed with the caret still on its prompt, the lay
/// finds ink under the caret and the fence is tainted. The clean reselect: the caret stands
/// past the composer's last ink by the time the resume is seen (where Claude's redraw leaves
/// it, after its last row, before it puts the caret back), so the reselected mark's closed
/// span ends at the prompt row (exclusive) and its new span opens under the mode row: only the
/// composer's top rule is in a span, its prompt row and bottom rule are in none, so no one span
/// holds it whole, yet every row of it is still the holder's own or no one's. NEGATIVE CONTROL: with `FenceFrame::disowns` asking one span
/// to hold the rules whole, the clean reselect strips the live holder's lockup.
#[test]
fn a_live_holders_composer_keeps_its_glass_across_a_suspend() {
    use aterm_core::terminal::{CutKind, HolderCut};
    let rows = claude_rows("footer-bypass-idle", 80);
    let bottom = aterm_phase::phase::composer_bottom(&rows).unwrap();
    let top = bottom - 2;
    let last = rows.len() - 1;
    assert_eq!(bottom + 1, last, "the composer ends the screen");
    let facts = || FooterFacts {
        model: Some("Opus 5.5".into()),
        path: Some("~/aterm".into()),
        version: Some("2.1.283".into()),
        owner: Some(footer::FactsOwner {
            floor: Some(1_790_607_848),
            session_id: "00000000-0000-4000-8000-00000000000e".into(),
        }),
        ..FooterFacts::default()
    };
    // The cursor after the mode row's last glyph: no ink from it on.
    let past_ink = format!("\x1b[{};{}H", last + 1, rows[last].chars().count() + 1);
    for (what, park) in [
        ("CONTROL: the caret on its prompt", None),
        ("the caret past the composer's ink", Some(past_ink.as_str())),
    ] {
        let (mut app, wid, session) = glass(80);
        let _host = super::probe::hosted(&mut app);
        let ctx = app.pool.get(session).unwrap().ctx.clone();
        crate::session_timeline::claim_supervisor(
            &ctx,
            &crate::harness_host::holder(),
            None,
            None,
            crate::metrics::now_us(),
        )
        .expect("claim");
        let token = super::probe::drawer(&app, session, 4242, "claude", "2.1.283");
        ctx.timeline
            .lock()
            .unwrap()
            .set_program(4242, Some("claude".into()));
        feed_rows(&app, session, &rows);
        publish(&app, session, facts());
        frame(&mut app, wid);
        let strip = strip(&app, wid);
        let text = |app: &App, r: usize| {
            text_of(&app.windows[&wid].input_scratch.cells[strip + r])
                .trim_end()
                .to_owned()
        };
        let icon = |app: &App| {
            app.windows[&wid]
                .claude_card
                .painted_layers()
                .iter()
                .any(|l| l.id == top_mark_id(session) && !l.content.items().is_empty())
        };
        assert!(
            text(&app, top).contains(" claude ") && icon(&app),
            "{what}: CONTROL: the holder's composer wears the lockup: {:?}",
            text(&app, top)
        );
        assert!(
            text(&app, bottom).contains("\u{25C6} Opus 5.5"),
            "{what}: CONTROL: and the facts: {:?}",
            text(&app, bottom)
        );
        let term = app.pool.get(session).unwrap().term.clone();
        // Ctrl+Z: the shell's group in front, Claude stopped, not gone.
        crate::term_lock(&term).note_holder_cut(HolderCut {
            to: None,
            kind: CutKind::Group,
            from_alive: true,
        });
        {
            let mut timeline = ctx.timeline.lock().unwrap();
            timeline.note_foreground_group(100);
            assert!(timeline.set_claude_footer(100, None, None));
        }
        if let Some(park) = park {
            crate::term_lock(&term).process(park.as_bytes());
        }
        // `fg`: the same drawer in front again.
        assert_eq!(
            super::probe::drawer(&app, session, 4242, "claude", "2.1.283"),
            token,
            "{what}: the same drawer"
        );
        ctx.timeline
            .lock()
            .unwrap()
            .set_program(4242, Some("claude".into()));
        publish(&app, session, facts());
        let holder = crate::term_lock(&term).holder_view();
        assert_eq!(
            (holder.token, holder.tainted),
            (token, park.is_none()),
            "{what}: the fence at the resume: {holder:?}"
        );
        if park.is_some() {
            let base = u64::try_from(app.windows[&wid].input_scratch.base_y).unwrap();
            let (lo, hi) = (base + top as u64, base + bottom as u64 + 1);
            assert!(
                !holder.admits_rows(lo, hi) && holder.admits_rows(lo, lo + 1),
                "{what}: the rules straddle the spans, the top one the holder's: {holder:?}"
            );
        }
        frame(&mut app, wid);
        assert!(
            text(&app, top).contains(" claude ") && icon(&app),
            "{what}: the resumed holder's composer wears the lockup: {:?}",
            text(&app, top)
        );
        assert!(
            text(&app, bottom).contains("\u{25C6} Opus 5.5"),
            "{what}: and the facts: {:?}",
            text(&app, bottom)
        );
    }
}

/// CLAUDE'S EFFORT HINT FILLS FACTS THAT COME AFTER IT: on a slow first launch the facts are
/// published only once Claude has dropped its hint; the level a frame read off the holder's
/// own row is still the open effort's, and `◆` shows it. CONTROL: a hint the holder did not
/// draw is not kept, and the rule names the model alone.
#[test]
fn claudes_effort_hint_fills_facts_published_after_it_went() {
    let open = FooterFacts {
        model: Some("Opus 5.5".into()),
        path: Some("~/aterm".into()),
        version: Some("2.1.283".into()),
        effort_open: true,
        owner: Some(footer::FactsOwner {
            floor: Some(1_790_607_848),
            session_id: "00000000-0000-4000-8000-000000000008".into(),
        }),
        ..FooterFacts::default()
    };
    let mut bare = claude_rows("footer-bypass-idle", 80);
    let bottom = aterm_phase::phase::composer_bottom(&bare).unwrap();
    for r in &mut bare[..bottom - 2] {
        r.clear();
    }
    let jobs = "  \u{23F5}\u{23F5} bypass permissions on \u{B7} 1 shell";
    let mut hinted = bare.clone();
    hinted[bottom + 1] = format!("{jobs:<62}\u{25D0} medium \u{B7} /effort");
    let mut dropped = bare;
    dropped[bottom + 1] = jobs.to_owned();
    for holder in [true, false] {
        let (mut app, wid, session) = glass(80);
        if holder {
            super::probe::drawer(&app, session, 4242, "claude", "2.1.283");
        }
        feed_rows(&app, session, &hinted);
        frame(&mut app, wid);
        feed_rows(&app, session, &dropped);
        frame(&mut app, wid);
        publish(&app, session, open.clone());
        frame(&mut app, wid);
        let strip = strip(&app, wid);
        let rule = text_of(&app.windows[&wid].input_scratch.cells[strip + bottom]);
        assert!(rule.contains("\u{25C6} Opus 5.5"), "{holder}: {rule:?}");
        assert_eq!(
            rule.contains("\u{25C6} Opus 5.5 medium"),
            holder,
            "{holder}: the hint's effort only off the holder's row: {rule:?}"
        );
    }
}

/// A LAUNCH NO FRAME WATCHED NAMES ITS EFFORT: where no frame is drawn while Claude shows its
/// start-up hint (a background tab, an occluded window, another Space), the outputs that draw
/// and drop it read it off the live screen for the holder, so the first frame after it went
/// shows `◆ Opus 5.5 medium`, the facts published before the hint or after it went.
/// CONTROL: with no drawer, no row is the holder's and the rule names the model alone.
#[test]
fn a_launch_no_frame_watched_names_its_effort() {
    let open = FooterFacts {
        model: Some("Opus 5.5".into()),
        path: Some("~/aterm".into()),
        version: Some("2.1.283".into()),
        effort_open: true,
        owner: Some(footer::FactsOwner {
            floor: Some(1_790_607_848),
            session_id: "00000000-0000-4000-8000-00000000000b".into(),
        }),
        ..FooterFacts::default()
    };
    let mut bare = claude_rows("footer-bypass-idle", 80);
    let bottom = aterm_phase::phase::composer_bottom(&bare).unwrap();
    for r in &mut bare[..bottom - 2] {
        r.clear();
    }
    let jobs = "  \u{23F5}\u{23F5} bypass permissions on \u{B7} 1 shell";
    let mut hinted = bare.clone();
    hinted[bottom + 1] = format!("{jobs:<62}\u{25D0} medium \u{B7} /effort");
    let mut dropped = bare;
    dropped[bottom + 1] = jobs.to_owned();
    for (what, holder, facts_first) in [
        ("facts before the hint", true, true),
        ("facts after it went", true, false),
        ("CONTROL: no drawer", false, true),
    ] {
        let (mut app, wid, session) = glass(80);
        if holder {
            super::probe::drawer(&app, session, 4242, "claude", "2.1.283");
        }
        if facts_first {
            publish(&app, session, open.clone());
        }
        for rows in [&hinted, &dropped] {
            feed_rows(&app, session, rows);
            app.on_output_wake(session, std::time::Instant::now());
        }
        if !facts_first {
            publish(&app, session, open.clone());
        }
        frame(&mut app, wid);
        let strip = strip(&app, wid);
        let rule = text_of(&app.windows[&wid].input_scratch.cells[strip + bottom]);
        assert!(rule.contains("\u{25C6} Opus 5.5"), "{what}: {rule:?}");
        assert_eq!(
            rule.contains("\u{25C6} Opus 5.5 medium"),
            holder,
            "{what}: the hint read off the holder's row with no frame: {rule:?}"
        );
    }
}

/// THE EFFORT HINT IS LOOKED FOR WHILE A START-UP SHOWS IT: a holder whose hint no read found
/// (a default session, its mode row with none) has its live screen read at each output for
/// `EFFORT_HINT_LOOK` from the first, and at none after; a new holder (a relaunch) is looked
/// for again. CONTROL: inside the window each output reads.
#[test]
fn the_effort_hint_is_looked_for_while_a_start_up_shows_it() {
    use crate::session_timeline::EFFORT_HINT_LOOK;
    use crate::work_counts::take_effort_hint_reads;
    let mut rows = claude_rows("footer-bypass-idle", 80);
    let bottom = aterm_phase::phase::composer_bottom(&rows).unwrap();
    for r in &mut rows[..bottom - 2] {
        r.clear();
    }
    rows[bottom + 1] = "  \u{23F5}\u{23F5} bypass permissions on \u{B7} 1 shell".to_owned();
    let (mut app, _wid, session) = glass(80);
    super::probe::drawer(&app, session, 4242, "claude", "2.1.283");
    feed_rows(&app, session, &rows);
    let at = std::time::Instant::now();
    let _ = take_effort_hint_reads();
    app.on_output_wake(session, at);
    app.on_output_wake(session, at + EFFORT_HINT_LOOK / 2);
    assert_eq!(take_effort_hint_reads(), 2, "CONTROL: each output reads");
    app.on_output_wake(session, at + EFFORT_HINT_LOOK);
    app.on_output_wake(session, at + EFFORT_HINT_LOOK * 10);
    assert_eq!(take_effort_hint_reads(), 0, "past the start-up, none");
    super::probe::drawer(&app, session, 4343, "claude", "2.1.283");
    app.on_output_wake(session, at + EFFORT_HINT_LOOK * 11);
    assert_eq!(take_effort_hint_reads(), 1, "a new holder is looked for");
}

/// A RELAUNCH OF THE SAME CONVERSATION KEEPS ITS EFFORT WITH NO FRAME: an update's relaunch
/// resumes the conversation with the same settings, its facts leaving the effort open for
/// the model; where no frame was drawn while the new process showed its start-up hint (a
/// background tab, an occluded window), `◆` still shows the effort the old process's rule
/// showed, across the shell in front between them. CONTROL: a process that takes up another
/// conversation, or opens it on another model, shows the model alone.
#[test]
fn a_relaunch_of_the_same_conversation_keeps_its_effort_without_a_frame() {
    let conversation = "00000000-0000-4000-8000-000000000009";
    let open = FooterFacts {
        model: Some("Opus 5.5".into()),
        path: Some("~/aterm".into()),
        version: Some("2.1.283".into()),
        effort_open: true,
        owner: Some(footer::FactsOwner {
            floor: Some(1_790_607_848),
            session_id: conversation.into(),
        }),
        ..FooterFacts::default()
    };
    let mut bare = claude_rows("footer-bypass-idle", 80);
    let bottom = aterm_phase::phase::composer_bottom(&bare).unwrap();
    for r in &mut bare[..bottom - 2] {
        r.clear();
    }
    let jobs = "  \u{23F5}\u{23F5} bypass permissions on \u{B7} 1 shell";
    let mut hinted = bare.clone();
    hinted[bottom + 1] = format!("{jobs:<62}\u{25D0} medium \u{B7} /effort");
    let mut dropped = bare;
    dropped[bottom + 1] = jobs.to_owned();
    for (what, resumed, model, carried) in [
        ("the same conversation", conversation, "Opus 5.5", true),
        (
            "CONTROL: another conversation",
            "00000000-0000-4000-8000-00000000000a",
            "Opus 5.5",
            false,
        ),
        ("CONTROL: another model", conversation, "Sonnet 5", false),
    ] {
        let (mut app, wid, session) = glass(80);
        super::probe::drawer(&app, session, 4242, "claude", "2.1.283");
        feed_rows(&app, session, &hinted);
        publish(&app, session, open.clone());
        frame(&mut app, wid);
        let strip = strip(&app, wid);
        let rule = |app: &App| text_of(&app.windows[&wid].input_scratch.cells[strip + bottom]);
        assert!(
            rule(&app).contains("\u{25C6} Opus 5.5 medium"),
            "{what}: before the relaunch: {:?}",
            rule(&app)
        );
        let entry = app.pool.get(session).unwrap();
        {
            // Claude Code exits to the shell, which runs the update's resume line.
            let mut timeline = entry.ctx.timeline.lock().unwrap();
            timeline.note_foreground_group(100);
            assert!(timeline.set_claude_footer(100, None, None));
        }
        super::probe::drawer(&app, session, 4343, "claude", "2.1.290");
        feed_rows(&app, session, &hinted);
        feed_rows(&app, session, &dropped);
        {
            let mut timeline = entry.ctx.timeline.lock().unwrap();
            assert!(timeline.set_claude_footer(
                4343,
                Some(1_790_608_000),
                Some(FooterFacts {
                    model: Some(model.into()),
                    version: Some("2.1.290".into()),
                    effort_for: Some(model.into()),
                    owner: Some(footer::FactsOwner {
                        floor: Some(1_790_608_000),
                        session_id: resumed.into(),
                    }),
                    ..open.clone()
                })
            ));
        }
        frame(&mut app, wid);
        assert!(
            rule(&app).contains(&format!("\u{25C6} {model}")),
            "{what}: {:?}",
            rule(&app)
        );
        assert_eq!(
            rule(&app).contains("medium"),
            carried,
            "{what}: the effort carried only to the same conversation's model: {:?}",
            rule(&app)
        );
    }
}

/// AN UNWATCHED LANDING OF A LAUNCH WITH NO TURN KEEPS ITS EFFORT: a ⬆️ landing relaunches a
/// session that has had no turn without `--resume` (there is no transcript to resume), so the
/// new process starts another conversation and no effort the old one showed carries to it.
/// Where no frame is drawn after the landing (an idle tab's unattended upgrade), the new
/// process's own start-up hint, read off the live screen by the outputs that draw and drop
/// it, keeps `◆ Opus 5.5 medium`, as the old process's watched hint gave it. CONTROL: a
/// relaunch that draws no hint shows the model alone: neither the old holder's level nor the
/// old conversation's effort passes to the new one.
#[test]
fn an_unwatched_landing_of_a_launch_with_no_turn_keeps_its_effort() {
    let facts = |conversation: &str, version: &str, floor: u64| FooterFacts {
        model: Some("Opus 5.5".into()),
        path: Some("~/aterm".into()),
        version: Some(version.into()),
        effort_open: true,
        owner: Some(footer::FactsOwner {
            floor: Some(floor),
            session_id: conversation.into(),
        }),
        ..FooterFacts::default()
    };
    let mut bare = claude_rows("footer-bypass-idle", 80);
    let bottom = aterm_phase::phase::composer_bottom(&bare).unwrap();
    for r in &mut bare[..bottom - 2] {
        r.clear();
    }
    let jobs = "  \u{23F5}\u{23F5} bypass permissions on \u{B7} 1 shell";
    let mut hinted = bare.clone();
    hinted[bottom + 1] = format!("{jobs:<62}\u{25D0} medium \u{B7} /effort");
    let mut dropped = bare;
    dropped[bottom + 1] = jobs.to_owned();
    for (what, hint, kept) in [
        ("the relaunch's own hint", true, true),
        ("CONTROL: a relaunch that draws no hint", false, false),
    ] {
        let (mut app, wid, session) = glass(80);
        super::probe::drawer(&app, session, 4242, "claude", "2.1.283");
        feed_rows(&app, session, &hinted);
        app.on_output_wake(session, std::time::Instant::now());
        publish(
            &app,
            session,
            facts(
                "00000000-0000-4000-8000-00000000000c",
                "2.1.283",
                1_790_607_848,
            ),
        );
        frame(&mut app, wid);
        let strip = strip(&app, wid);
        let rule = |app: &App| text_of(&app.windows[&wid].input_scratch.cells[strip + bottom]);
        assert!(
            rule(&app).contains("\u{25C6} Opus 5.5 medium"),
            "{what}: watched, before the landing: {:?}",
            rule(&app)
        );
        let timeline = std::sync::Arc::clone(&app.pool.get(session).unwrap().ctx.timeline);
        {
            // Claude Code exits to the shell, which runs the landing's line: no `--resume`.
            let mut timeline = timeline.lock().unwrap();
            timeline.note_foreground_group(100);
            assert!(timeline.set_claude_footer(100, None, None));
        }
        super::probe::drawer(&app, session, 4343, "claude", "2.1.290");
        let drawn: &[&Vec<String>] = if hint {
            &[&hinted, &dropped]
        } else {
            &[&dropped]
        };
        for rows in drawn {
            feed_rows(&app, session, rows);
            app.on_output_wake(session, std::time::Instant::now());
        }
        {
            let mut timeline = timeline.lock().unwrap();
            assert!(timeline.set_claude_footer(
                4343,
                Some(1_790_608_000),
                Some(facts(
                    "00000000-0000-4000-8000-00000000000d",
                    "2.1.290",
                    1_790_608_000
                )),
            ));
        }
        frame(&mut app, wid);
        assert!(
            rule(&app).contains("\u{25C6} Opus 5.5"),
            "{what}: {:?}",
            rule(&app)
        );
        assert_eq!(
            rule(&app).contains("medium"),
            kept,
            "{what}: the new conversation's effort is its own hint's: {:?}",
            rule(&app)
        );
    }
}

#[test]
fn a_card_quoted_under_the_drawers_own_is_not_kept() {
    for quoted in ["2.1.200", "2.1.283"] {
        let (mut app, wid, session) = glass(80);
        let open = FooterFacts {
            path: Some("~/aterm".into()),
            version: Some("2.1.283".into()),
            model_open: true,
            effort_open: true,
            owner: Some(footer::FactsOwner {
                floor: Some(1_790_607_848),
                session_id: "00000000-0000-4000-8000-000000000006".into(),
            }),
            ..FooterFacts::default()
        };
        let rows = claude_rows("footer-bypass-idle", 80);
        let bottom = aterm_phase::phase::composer_bottom(&rows).unwrap();
        super::probe::drawer(&app, session, 4242, "claude", "2.1.283");
        feed_rows(&app, session, &rows);
        publish(&app, session, open);
        frame(&mut app, wid);
        let strip = strip(&app, wid);
        let rule = |app: &App| text_of(&app.windows[&wid].input_scratch.cells[strip + bottom]);
        assert!(
            rule(&app).contains("\u{25C6} Opus 5.5 xhigh"),
            "CONTROL: the drawer's own card: {:?}",
            rule(&app)
        );
        let mut quoting = rows.clone();
        quoting[10] = format!(
            "  \u{23BF}  \u{2590}\u{259B}\u{2588}\u{2588}\u{2588}\u{259B}\u{2588}   Claude Code \
             v{quoted}"
        );
        quoting[11] = "     Haiku 4.5 \u{00B7} API Usage Billing".into();
        feed_rows(&app, session, &quoting);
        frame(&mut app, wid);
        let read = app.windows[&wid]
            .claude_footer_texts
            .borrow()
            .footer_card(0, session)
            .map(|(_, read)| (read.banner, read.card.model.clone()));
        assert_eq!(
            read,
            Some((Some(10), "Haiku 4.5".to_string())),
            "{quoted}: the footer's reader takes the quote"
        );
        assert!(
            rule(&app).contains("\u{25C6} Opus 5.5 xhigh"),
            "{quoted}: the quote is not kept: {:?}",
            rule(&app)
        );
        let entry = app.pool.get(session).unwrap();
        let kept = entry.ctx.timeline.lock().unwrap().claude_card().cloned();
        assert_eq!(
            kept.map(|card| (card.version, card.model)),
            Some(("2.1.283".to_string(), "Opus 5.5".to_string())),
            "{quoted}"
        );
    }
}

#[test]
fn a_newer_launch_card_replaces_the_kept_one() {
    let open = FooterFacts {
        path: Some("~/aterm".into()),
        version: Some("2.1.283".into()),
        model_open: true,
        effort_open: true,
        owner: Some(footer::FactsOwner {
            floor: Some(1_790_607_848),
            session_id: "00000000-0000-4000-8000-000000000005".into(),
        }),
        ..FooterFacts::default()
    };
    let rows = claude_rows("footer-bypass-idle", 80);
    let bottom = aterm_phase::phase::composer_bottom(&rows).unwrap();
    let stale: Vec<String> = rows
        .iter()
        .map(|r| r.replace("Opus 5.5 with xhigh effort", "Haiku 4.5"))
        .collect();
    assert_ne!(stale, rows, "the fixture's card names Opus 5.5");
    let (mut app, wid, session) = glass(80);
    feed_rows(&app, session, &stale);
    super::probe::drawer(&app, session, 4242, "claude", "2.1.283");
    publish(&app, session, open.clone());
    frame(&mut app, wid);
    let strip = strip(&app, wid);
    let rule = |app: &App| text_of(&app.windows[&wid].input_scratch.cells[strip + bottom]);
    assert!(
        !rule(&app).contains('\u{25C6}'),
        "the predecessor's card is not kept: {:?}",
        rule(&app)
    );
    feed_rows(&app, session, &rows);
    frame(&mut app, wid);
    assert!(
        rule(&app).contains("\u{25C6} Opus 5.5 xhigh   \u{2302} ~/aterm"),
        "{:?}",
        rule(&app)
    );
    feed_rows(&app, session, &stale);
    frame(&mut app, wid);
    assert!(
        rule(&app).contains("\u{25C6} Haiku 4.5   \u{2302} ~/aterm"),
        "{:?}",
        rule(&app)
    );
    let mut scrolled = stale.clone();
    for r in scrolled.iter_mut().take(5) {
        r.clear();
    }
    feed_rows(&app, session, &scrolled);
    frame(&mut app, wid);
    assert!(
        rule(&app).contains("\u{25C6} Haiku 4.5"),
        "{:?}",
        rule(&app)
    );
    let (mut app, wid, session) = glass(80);
    super::probe::drawer(&app, session, 4242, "claude", "2.1.283");
    feed_rows(&app, session, &stale);
    publish(&app, session, open);
    frame(&mut app, wid);
    let rule = text_of(&app.windows[&wid].input_scratch.cells[strip + bottom]);
    assert!(
        rule.contains("\u{25C6} Haiku 4.5   \u{2302} ~/aterm"),
        "CONTROL: {rule:?}"
    );
}

/// A MOVE THE PERSON HOLDS IS SAID BY THE ⬆️, NOT THE BOTTOM RULE: the rule carries the
/// facts alone (no `⇣ Claude Code 2.1.286 is ready · waits while you use this tab`), the
/// row's arrival repaints the footer for the top rule's ⬆️, and the ⬆️'s words say when its
/// press's restart lands: at once over the person's typing (the person pressing is the one
/// typing), then once their draft is sent. CONTROL: no row, no arrow.
#[test]
fn a_move_the_person_holds_is_said_by_the_arrow_not_the_bottom_rule() {
    use aterm_agent::harness::upgrade::Phase;
    use aterm_agent::harness::upgrade_drive::Row;
    let (mut app, wid, session) = glass(150);
    super::probe::drawer(&app, session, 4242, "claude", "2.1.285");
    app.pool
        .get(session)
        .unwrap()
        .ctx
        .timeline
        .lock()
        .unwrap()
        .set_program(4242, Some("claude".into()));
    let rows = claude_rows("footer-bypass-idle", 150);
    let bottom = aterm_phase::phase::composer_bottom(&rows).unwrap();
    feed_rows(&app, session, &rows);
    publish(&app, session, facts());
    let tab = {
        let store = app.store.read().unwrap_or_else(|p| p.into_inner());
        store
            .by_local(session)
            .expect("the front session")
            .sid
            .as_str()
            .to_string()
    };
    let now = crate::upgrade_host::now_s();
    let held = Row {
        session: "conv".into(),
        tab,
        from: "2.1.285".into(),
        to: "2.1.286".into(),
        source: "managed".into(),
        phase: Phase::Announced {
            at_s: now - 600,
            asks: 1,
        },
        behind_since: now - (5 * 3_600 + 38 * 60),
        wait: "attended".into(),
        wait_since: now - 20,
        wait_seen: now - 5,
        person_named: true,
        ready: true,
        ..Row::default()
    };
    let up = |app: &App| {
        let ctx = app.pool.get(session).expect("session").ctx.clone();
        let timeline = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
        timeline
            .upgrade()
            .map(TopUp::of)
            .and_then(|up| up_said(&up, "", None))
    };
    assert_eq!(up(&app), None, "CONTROL: no row, no arrow");
    let plan = app.active_visible_leaf_plan(wid).unwrap();
    let fp_before = app.claude_footer_fp(wid, &plan);
    app.apply_agent_upgrades(vec![held.clone()]);
    assert_ne!(
        app.claude_footer_fp(wid, &plan),
        fp_before,
        "the row repaints, for the arrow"
    );
    assert_eq!(up(&app).as_deref(), Some("update to v2.1.286 \u{b7} now"));
    frame(&mut app, wid);
    let strip = strip(&app, wid);
    let text = text_of(&app.windows[&wid].input_scratch.cells[strip + bottom]);
    let facts_text = "\u{25C6} Opus 5.5 xhigh   \u{2302} ~/aterm   \u{2387} main \u{2500}";
    assert!(!text.contains('\u{21E3}'), "{text:?}");
    assert!(!text.contains("waits"), "{text:?}");
    assert!(text.ends_with(&format!(" {facts_text}")), "{text:?}");
    app.apply_agent_upgrades(vec![Row {
        wait: "draft".into(),
        ..held
    }]);
    assert_eq!(
        up(&app).as_deref(),
        Some("update to v2.1.286 \u{b7} once your draft is sent or cleared")
    );
}

#[test]
fn no_facts_keys_like_no_footer() {
    let none = FactMask::default();
    assert_eq!(fingerprint(None, None, true, none), 0);
    assert_ne!(fingerprint(Some(&facts()), None, false, none), 0);
    let mut other = facts();
    other.branch = Some("feature".into());
    assert_ne!(
        fingerprint(Some(&facts()), None, true, none),
        fingerprint(Some(&other), None, true, none)
    );
    assert_ne!(
        fingerprint(Some(&facts()), None, true, none),
        fingerprint(
            Some(&facts()),
            None,
            true,
            none.with(FactKind::Branch, true)
        )
    );
}

#[test]
fn the_account_moves_the_repaint_key() {
    let on = |label: &str| FooterFacts {
        account: Some(label.into()),
        ..facts()
    };
    for usage in [true, false] {
        let key = |facts: &FooterFacts| fingerprint(Some(facts), None, usage, FactMask::default());
        assert_ne!(key(&on("main")), key(&on("work-2")), "another account");
        assert_ne!(key(&on("main")), key(&facts()), "one where there was none");
        assert_eq!(key(&on("main")), key(&on("main")), "the same account");
    }
    let (app, wid, session) = glass(80);
    let plan = app.active_visible_leaf_plan(wid).unwrap();
    publish(&app, session, on("main"));
    let main = app.claude_footer_fp(wid, &plan);
    publish(&app, session, on("work-2"));
    assert_ne!(app.claude_footer_fp(wid, &plan), main);
}

#[test]
fn progress_read_availability_moves_the_repaint_key_without_changing_history() {
    let mut facts = facts();
    facts.account_progress =
        Some(aterm_agent::harness::account_progress::AccountProgress::unknown(100));
    let retained = facts.account_progress.clone();
    for usage in [false, true] {
        let key = |facts: &FooterFacts| fingerprint(Some(facts), None, usage, FactMask::default());
        let before = key(&facts);
        facts.account_progress_busy = true;
        assert_ne!(key(&facts), before);
        assert_eq!(facts.account_progress, retained);
        facts.account_progress_busy = false;
        assert_eq!(key(&facts), before);
    }
}

#[test]
fn footer_uses_the_cell_frames_viewport_and_blank_after_terminal_moves() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let plan = app.active_visible_leaf_plan(wid).unwrap();
    let front = app.front_terminal(wid).unwrap().clone();
    let (rows, cols) = {
        let ws = &app.windows[&wid];
        (usize::from(ws.rows), usize::from(ws.cols))
    };
    {
        let entry = app.pool.get(front.session).unwrap();
        let mut timeline = entry.ctx.timeline.lock().unwrap();
        timeline.note_foreground_group(42);
        assert!(timeline.set_claude_footer(42, None, Some(facts())));
    }
    {
        let mut term = crate::term_lock(&front.term);
        term.resize(rows as u16, cols as u16);
        term.set_default_foreground(aterm_core::terminal::Rgb {
            r: 10,
            g: 20,
            b: 30,
        });
        term.set_default_background(aterm_core::terminal::Rgb {
            r: 40,
            g: 50,
            b: 60,
        });
        for _ in 0..rows + 4 {
            term.process(b"history\r\n");
        }
        let ws = app.windows.get_mut(&wid).unwrap();
        term.cell_frame_into(&mut ws.input_scratch, rows, cols);
        vendor_frame(&mut ws.input_scratch);
    }
    let extracted_blank = app.windows[&wid].input_scratch.implicit_blank;
    assert_eq!(extracted_blank.fg, [10, 20, 30]);
    assert_eq!(extracted_blank.bg, [40, 50, 60]);
    assert_eq!(app.windows[&wid].input_scratch.display_offset, 0);
    {
        let mut term = crate::term_lock(&front.term);
        term.process(b"\x1b]10;rgb:aa/bb/cc\x1b\\\x1b]11;rgb:dd/ee/ff\x1b\\");
        term.scroll_display(1);
        assert!(term.grid().display_offset() > 0);
        assert_ne!(term.implicit_blank_render_cell(), extracted_blank);
    }
    app.splice_claude_footer(
        wid,
        &plan,
        VisibleContentRoute::Terminal { composed: false },
    );
    let footer = &app.windows[&wid].input_scratch.cells[rows - 2];
    let mark = footer
        .iter()
        .position(|c| c.ch == '\u{25C6}')
        .expect("the extracted live frame still paints");
    assert_eq!(footer[mark + 2].bg, extracted_blank.bg);

    let text_before = Arc::clone(
        &app.windows[&wid].claude_footer_texts.borrow().panes[0]
            .as_ref()
            .unwrap()
            .rows,
    );
    app.splice_claude_footer(
        wid,
        &plan,
        VisibleContentRoute::Terminal { composed: false },
    );
    let text_after = Arc::clone(
        &app.windows[&wid].claude_footer_texts.borrow().panes[0]
            .as_ref()
            .unwrap()
            .rows,
    );
    assert!(Arc::ptr_eq(&text_before, &text_after));
    assert_eq!(
        app.windows[&wid].input_scratch.cells[rows - 2][mark].ch,
        '\u{25C6}'
    );

    {
        let mut term = crate::term_lock(&front.term);
        let ws = app.windows.get_mut(&wid).unwrap();
        term.cell_frame_into(&mut ws.input_scratch, rows, cols);
        vendor_frame(&mut ws.input_scratch);
        assert!(ws.input_scratch.display_offset > 0);
        term.scroll_display(-1);
        assert_eq!(term.grid().display_offset(), 0);
    }
    app.splice_claude_footer(
        wid,
        &plan,
        VisibleContentRoute::Terminal { composed: false },
    );
    let row = &app.windows[&wid].input_scratch.cells[rows - 2];
    assert!(
        row.iter().all(|c| c.ch == '\u{2500}'),
        "history frame keeps the vendor's rule"
    );
}

#[test]
fn footer_source_uses_leaf_index_for_splits_and_view_for_mixed_tabs() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let mut plan = app.active_visible_leaf_plan(wid).unwrap();
    let leaf = plan.leaves.remove(0);
    let mut sibling = leaf.clone();
    sibling.focused = false;
    let ws = app.windows.get_mut(&wid).unwrap();
    ws.input_scratch.terminal_id = 11;
    ws.composed_focus_scratch.terminal_id = 22;
    ws.unfocused_pane_scratch.insert(1, RenderInput::empty());
    ws.unfocused_pane_scratch.get_mut(&1).unwrap().terminal_id = 33;
    ws.leaf_render_cache
        .entry(leaf.view)
        .or_default()
        .input
        .terminal_id = 44;

    let id = |route: VisibleContentRoute, index: usize, leaf: &crate::tab_model::VisibleLeaf| {
        frame_source(ws, route, index, leaf).map(|s| s.terminal_id)
    };
    assert_eq!(
        id(VisibleContentRoute::Terminal { composed: false }, 0, &leaf),
        Some(11)
    );
    assert_eq!(
        id(VisibleContentRoute::Terminal { composed: true }, 0, &leaf),
        Some(22)
    );
    assert_eq!(
        id(
            VisibleContentRoute::Terminal { composed: true },
            1,
            &sibling
        ),
        Some(33)
    );
    assert_eq!(id(VisibleContentRoute::Heterogeneous, 0, &leaf), Some(44));
    assert_eq!(
        id(
            VisibleContentRoute::Terminal { composed: true },
            2,
            &sibling
        ),
        None
    );
}

#[test]
fn agy_launch_card_and_composer_footer_render_on_the_glass() {
    let (mut app, wid, session) = glass(120);
    {
        let entry = app.pool.get(session).expect("session");
        let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
        timeline.note_foreground_group(4242);
        timeline.set_program(4242, Some("agy".into()));
        assert!(timeline.set_claude_footer(
            4242,
            None,
            Some(FooterFacts {
                model_open: true,
                effort_open: true,
                path: Some("~/aterm".into()),
                branch: Some("main".into()),
                owner: Some(footer::FactsOwner {
                    floor: None,
                    session_id: "agy-4242".into(),
                }),
                ..FooterFacts::default()
            }),
        ));
    }
    let host = crate::harness_host::HostHandle::for_supervised_test(
        Vec::new(),
        std::sync::Arc::new(|_| crate::harness_host::BodyEnd::Stopped),
    );
    app.harness = Some(host.clone());
    let ctx = app.pool.get(session).expect("session").ctx.clone();
    crate::session_timeline::claim_supervisor(
        &ctx,
        &crate::harness_host::holder(),
        None,
        None,
        crate::metrics::now_us(),
    )
    .expect("claim");

    let rule = "\u{2500}".repeat(120);
    let rows = vec![
        String::new(),
        "      \u{2584}\u{2580}\u{2580}\u{2584}        Antigravity CLI 1.4.0".into(),
        "     \u{2580}\u{2580}\u{2580}\u{2580}\u{2580}\u{2580}       redacted-user@example.test (Google AI Ultra)".into(),
        "    \u{2580}\u{2580}\u{2580}\u{2580}\u{2580}\u{2580}\u{2580}\u{2580}      Gemini 4 Argon (Medium)".into(),
        "   \u{2584}\u{2580}\u{2580}    \u{2580}\u{2580}\u{2584}     ~/aterm".into(),
        "  \u{2584}\u{2580}\u{2580}      \u{2580}\u{2580}\u{2584}".into(),
        String::new(),
        rule.clone(),
        "> ".into(),
        rule,
        "? for shortcuts                                                                                  Gemini 4 Argon \u{00B7} medium".into(),
    ];
    super::probe::drawer(&app, session, 4242, "agy", "1.4.2");
    feed_rows(&app, session, &rows);
    frame(&mut app, wid);

    let strip = strip(&app, wid);
    let card_line = text_of(&app.windows[&wid].input_scratch.cells[strip + 5]);
    assert!(
        card_line.contains(&format!(
            "enhanced by alab agy harness v{}",
            aterm_phase::CliHarness::Agy.version_str()
        )) && !card_line.contains("autopilot"),
        "agy card line on row top+4: {card_line:?}"
    );
    assert!(
        app.windows[&wid]
            .claude_card
            .painted_layers()
            .iter()
            .any(|l| l.slot == aterm_core::render::FxSlot::Under && !l.content.items().is_empty()),
        "agy card rainbow border and trail drawn"
    );
    assert!(
        app.windows[&wid]
            .claude_card
            .painted_layers()
            .iter()
            .any(|l| l.slot == aterm_core::render::FxSlot::Over && !l.content.items().is_empty()),
        "agy card kitty and stars drawn"
    );
    let top_rule = text_of(&app.windows[&wid].input_scratch.cells[strip + 7]);
    assert!(
        top_rule.starts_with("\u{2500}\u{2500}    agy \u{2500}")
            && top_rule.ends_with(&format!(
                "{} {} (v1.4.2) \u{2500}\u{2500}",
                super::probe::LIT_SWITCH,
                super::probe::harness_label(aterm_phase::CliHarness::Agy)
            )),
        "agy top rule has its lockup and the harness switch: {top_rule:?}"
    );
    assert!(
        app.windows[&wid]
            .claude_card
            .painted_layers()
            .iter()
            .any(|l| l.id == top_mark_id(session) && !l.content.items().is_empty()),
        "agy's icon drawn over its lockup's cells"
    );
    let bottom_rule = text_of(&app.windows[&wid].input_scratch.cells[strip + 9]);
    assert!(
        bottom_rule.contains('\u{221E}'),
        "agy composer rule has the autopilot chip: {bottom_rule:?}"
    );
    assert!(
        bottom_rule.contains("\u{25C6} Gemini 4 Argon medium   \u{2302} ~/aterm   \u{2387} main"),
        "agy composer rule has model, effort, path, and branch: {bottom_rule:?}"
    );
    host.shutdown_and_join();
}

#[test]
fn agy_follows_its_own_status_row_once_its_card_is_gone() {
    let rule = "\u{2500}".repeat(120);
    let screen = |card: bool, model: &str| -> Vec<String> {
        let mut rows = vec![String::new()];
        if card {
            rows.extend([
                "      \u{2584}\u{2580}\u{2580}\u{2584}        Antigravity CLI 1.4.0".to_string(),
                "     \u{2580}\u{2580}\u{2580}\u{2580}\u{2580}\u{2580}       redacted-user@example.test (Google AI Ultra)".to_string(),
                "    \u{2580}\u{2580}\u{2580}\u{2580}\u{2580}\u{2580}\u{2580}\u{2580}      Gemini 4 Argon (Medium)".to_string(),
                "   \u{2584}\u{2580}\u{2580}    \u{2580}\u{2580}\u{2584}     ~/aterm".to_string(),
                "  \u{2584}\u{2580}\u{2580}      \u{2580}\u{2580}\u{2584}".to_string(),
            ]);
        } else {
            rows.extend([
                "\u{25CF} Switched the model.".to_string(),
                String::new(),
                String::new(),
                String::new(),
                String::new(),
            ]);
        }
        rows.extend([
            String::new(),
            rule.clone(),
            "> ".into(),
            rule.clone(),
            format!("? for shortcuts{}{model}", " ".repeat(60)),
        ]);
        rows
    };
    let agy_facts = |app: &App, session: u64| {
        let entry = app.pool.get(session).expect("session");
        let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
        timeline.note_foreground_group(4242);
        timeline.set_program(4242, Some("agy".into()));
        assert!(timeline.set_claude_footer(
            4242,
            None,
            Some(FooterFacts {
                model_open: true,
                effort_open: true,
                path: Some("~/aterm".into()),
                owner: Some(footer::FactsOwner {
                    floor: None,
                    session_id: "agy-4242".into(),
                }),
                ..FooterFacts::default()
            }),
        ));
    };
    let bottom = |app: &App, wid: WindowId| {
        text_of(&app.windows[&wid].input_scratch.cells[strip(app, wid) + 9])
    };
    let (mut app, wid, session) = glass(120);
    agy_facts(&app, session);
    super::probe::drawer(&app, session, 4242, "agy", "1.4.2");
    feed_rows(
        &app,
        session,
        &screen(true, "Gemini 4 Argon \u{00B7} medium"),
    );
    frame(&mut app, wid);
    assert!(
        bottom(&app, wid).contains("\u{25C6} Gemini 4 Argon medium"),
        "{:?}",
        bottom(&app, wid)
    );
    feed_rows(&app, session, &screen(false, "Gemini 4 Pro \u{00B7} high"));
    frame(&mut app, wid);
    assert!(
        bottom(&app, wid).contains("\u{25C6} Gemini 4 Pro high"),
        "its own status row: {:?}",
        bottom(&app, wid)
    );
    let (mut app, wid, session) = glass(120);
    agy_facts(&app, session);
    feed_rows(&app, session, &screen(false, "Gemini 4 Pro \u{00B7} high"));
    super::probe::drawer(&app, session, 4242, "agy", "1.4.2");
    frame(&mut app, wid);
    assert!(
        !bottom(&app, wid).contains("Gemini 4 Pro"),
        "CONTROL: not the drawer's row: {:?}",
        bottom(&app, wid)
    );
}

/// AN ENTRY KEPT FOR ANOTHER REASON IS NOT A MASK CHOICE: a Muse session whose
/// facts entry exists only for an unrelated mark (`top_seen`, a slash press, a menu
/// note) still starts from Muse's starting mask, and a choice that only restates
/// that mask is kept as the person's when the program changes under it.
#[test]
fn an_unrelated_facts_entry_never_freezes_a_muse_sessions_starting_mask() {
    let (mut app, _wid, session) = glass(120);
    let starting = FactMask::MODEL
        .union(FactMask::EFFORT)
        .union(FactMask::PATH);
    {
        let entry = app.pool.get(session).expect("session");
        let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
        timeline.note_foreground_group(5151);
        timeline.set_program(5151, Some("muse".into()));
    }
    assert_eq!(app.fact_mask(session), starting, "no entry yet");
    app.claude_facts.entry(session).or_default().top_seen = Some(5151);
    assert_eq!(
        app.fact_mask(session),
        starting,
        "an entry kept for top_seen chose nothing"
    );
    app.set_fact_mask(session, starting);
    {
        let entry = app.pool.get(session).expect("session");
        let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
        timeline.set_program(5151, Some("claude".into()));
    }
    assert_eq!(
        app.fact_mask(session),
        starting,
        "the restated starting mask is the person's now, not the program's"
    );
}

#[test]
fn muse_launch_card_and_composer_footer_render_on_the_glass() {
    let (mut app, wid, session) = glass(120);
    {
        let entry = app.pool.get(session).expect("session");
        let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
        timeline.note_foreground_group(5151);
        timeline.set_program(5151, Some("muse".into()));
        assert!(timeline.set_claude_footer(
            5151,
            None,
            Some(FooterFacts {
                model_open: true,
                effort_open: true,
                path: Some("~/aterm".into()),
                branch: Some("main".into()),
                owner: Some(footer::FactsOwner {
                    floor: None,
                    session_id: "muse-5151".into(),
                }),
                ..FooterFacts::default()
            }),
        ));
    }
    let host = crate::harness_host::HostHandle::for_supervised_test(
        Vec::new(),
        std::sync::Arc::new(|_| crate::harness_host::BodyEnd::Stopped),
    );
    app.harness = Some(host.clone());
    let ctx = app.pool.get(session).expect("session").ctx.clone();
    crate::session_timeline::claim_supervisor(
        &ctx,
        &crate::harness_host::holder(),
        None,
        None,
        crate::metrics::now_us(),
    )
    .expect("claim");

    let rule = "\u{2500}".repeat(120);
    let rows = vec![
        String::new(),
        "  Muse Code 0.9.3".into(),
        String::new(),
        rule.clone(),
        "\u{276F} ".into(),
        rule,
        "  gpt-5.4 \u{00B7} high \u{00B7} ~/aterm \u{00B7} Auto-review".into(),
    ];
    super::probe::drawer(&app, session, 5151, "muse", "0.9.3");
    feed_rows(&app, session, &rows);
    frame(&mut app, wid);

    let strip = strip(&app, wid);
    let card_line = text_of(&app.windows[&wid].input_scratch.cells[strip + 2]);
    assert!(
        card_line.contains(&format!(
            "enhanced by alab muse harness v{} \u{00B7} alab.systems",
            aterm_phase::CliHarness::Muse.version_str()
        )) && !card_line.contains("autopilot"),
        "muse card line on row top+1: {card_line:?}"
    );
    assert!(
        app.windows[&wid]
            .claude_card
            .painted_layers()
            .iter()
            .any(|l| l.slot == aterm_core::render::FxSlot::Under && !l.content.items().is_empty()),
        "muse card rainbow border and trail drawn"
    );
    assert!(
        app.windows[&wid]
            .claude_card
            .painted_layers()
            .iter()
            .any(|l| l.slot == aterm_core::render::FxSlot::Over && !l.content.items().is_empty()),
        "muse card kitty and stars drawn"
    );
    let top_rule = text_of(&app.windows[&wid].input_scratch.cells[strip + 3]);
    assert!(
        top_rule.starts_with("\u{2500}\u{2500}    muse \u{2500}")
            && top_rule.ends_with(&format!(
                "{} {} (v0.9.3) \u{2500}\u{2500}",
                super::probe::LIT_SWITCH,
                super::probe::harness_label(aterm_phase::CliHarness::Muse)
            )),
        "muse top rule has its lockup and the harness switch: {top_rule:?}"
    );
    assert!(
        app.windows[&wid]
            .claude_card
            .painted_layers()
            .iter()
            .any(|l| l.id == top_mark_id(session) && !l.content.items().is_empty()),
        "muse's icon drawn over its lockup's cells"
    );
    let bottom_rule = text_of(&app.windows[&wid].input_scratch.cells[strip + 5]);
    assert!(
        bottom_rule.contains('\u{221E}'),
        "muse composer rule has the autopilot chip: {bottom_rule:?}"
    );
    assert!(
        bottom_rule
            .contains("\u{25C6} gpt-5.4 off high off   \u{2302} ~/aterm off   \u{2387} main"),
        "muse composer rule starts model, effort, and path off (its own status \
         line below already prints them) and branch on: {bottom_rule:?}"
    );
    host.shutdown_and_join();
}

#[test]
fn a_narrow_pane_keeps_muses_lockup_switch_and_facts() {
    let frames =
        aterm_phase::prompt::fixtures::reflow_frames(aterm_phase::muse::fixtures::IDLE_NARROW);
    let pane = |rows: &[String], cols: u16| {
        let (mut app, wid, session) = glass(cols);
        {
            let entry = app.pool.get(session).expect("session");
            let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            timeline.note_foreground_group(5252);
            timeline.set_program(5252, Some("muse".into()));
            assert!(timeline.set_claude_footer(
                5252,
                None,
                Some(FooterFacts {
                    model_open: true,
                    effort_open: true,
                    path: Some("~/aterm".into()),
                    owner: Some(footer::FactsOwner {
                        floor: None,
                        session_id: "muse-5252".into(),
                    }),
                    ..FooterFacts::default()
                }),
            ));
        }
        let host = crate::harness_host::HostHandle::for_supervised_test(
            Vec::new(),
            std::sync::Arc::new(|_| crate::harness_host::BodyEnd::Stopped),
        );
        app.harness = Some(host.clone());
        let ctx = app.pool.get(session).expect("session").ctx.clone();
        crate::session_timeline::claim_supervisor(
            &ctx,
            &crate::harness_host::holder(),
            None,
            None,
            crate::metrics::now_us(),
        )
        .expect("claim");
        super::probe::drawer(&app, session, 5252, "muse", "1.4.11");
        feed_rows(&app, session, rows);
        frame(&mut app, wid);
        let strip = strip(&app, wid);
        let cells = &app.windows[&wid].input_scratch.cells;
        let (top, bottom) = (text_of(&cells[strip + 3]), text_of(&cells[strip + 5]));
        let icon = app.windows[&wid]
            .claude_card
            .painted_layers()
            .iter()
            .any(|l| l.id == top_mark_id(session) && !l.content.items().is_empty());
        host.shutdown_and_join();
        (top, bottom, icon)
    };
    assert_eq!(frames.len(), 9, "the sweep's widths");
    for (cols, rows) in &frames {
        let (top, bottom, icon) = pane(rows, *cols);
        assert!(
            bottom.contains("\u{221E}")
                && (!bottom.contains("lab-stub") || bottom.contains("\u{25C6} lab-stub off")),
            "{cols}: the chip on the bottom rule, the model only as its off stub \
             (Muse's own line already prints it): {bottom:?}"
        );
        if *cols >= 32 {
            assert!(
                top.starts_with("\u{2500}\u{2500}    muse \u{2500}")
                    && top.contains(" \u{25CF} on ")
                    && top.ends_with(" \u{2500}\u{2500}")
                    && !top.contains("Voice"),
                "{cols}: the lockup over the label, the switch at the right: {top:?}"
            );
            assert!(icon, "{cols}: the icon over the lockup's cells");
        } else {
            assert_eq!(top, rows[3], "{cols}: the cut label as Muse Code drew it");
            assert!(!icon, "{cols}: no lockup over the cut label");
        }
        let mut bare = rows.clone();
        bare[3] = rows[3]
            .chars()
            .map(|c| if c == '\u{2500}' { c } else { ' ' })
            .collect();
        let (top, bottom, icon) = pane(&bare, *cols);
        assert_eq!(
            (bottom.contains("\u{221E}"), top.contains("muse") || icon),
            (*cols >= 40, false),
            "CONTROL {cols}: fewer than ten `─` and no label are no composer: {top:?} {bottom:?}"
        );
    }
}

#[test]
fn muse_signed_in_cards_populate_the_footer() {
    let (mut app, wid, session) = glass(160);
    // The plan the pane's footer holds: the rule shows it only in the account's hover title.
    let plan_of = |app: &App| {
        app.windows[&wid]
            .claude_footer_texts
            .borrow()
            .footers
            .iter()
            .find(|f| f.session == session)
            .and_then(|f| f.shown().subscription.as_ref()?.plan.clone())
    };
    let email_of = |app: &App| {
        app.windows[&wid]
            .claude_footer_texts
            .borrow()
            .footers
            .iter()
            .find(|f| f.session == session)
            .and_then(|f| f.shown().subscription.as_ref()?.email.clone())
    };
    const RESET: i64 = 1_792_947_600;
    let now = std::cell::Cell::new(RESET - 1);
    let clock_reads = std::cell::Cell::new(0usize);
    let clock = || {
        clock_reads.set(clock_reads.get() + 1);
        footer::ReadClock {
            read_at: std::time::UNIX_EPOCH,
            now: now.get(),
            offset_at: &|_, _| Some(0),
        }
    };
    let frame = |app: &mut App, wid| frame_with_clock(app, wid, Some(&clock));
    {
        let entry = app.pool.get(session).expect("session");
        let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
        timeline.note_foreground_group(6161);
        timeline.set_program(6161, Some("muse".into()));
        assert!(timeline.set_claude_footer(
            6161,
            None,
            Some(FooterFacts {
                model_open: true,
                effort_open: true,
                path: Some("~/aterm".into()),
                branch: Some("main".into()),
                owner: Some(footer::FactsOwner {
                    floor: None,
                    session_id: "muse-6161".into(),
                }),
                subscription: Some(aterm_agent::harness::profile::SubscriptionDisplay {
                    email: Some("redacted-user@example.test".into()),
                    ..Default::default()
                },),
                ..FooterFacts::default()
            }),
        ));
        assert!(!timeline.muse_cards_need_account());
    }
    let host = crate::harness_host::HostHandle::for_supervised_test(
        Vec::new(),
        std::sync::Arc::new(|_| crate::harness_host::BodyEnd::Stopped),
    );
    app.harness = Some(host.clone());
    let ctx = app.pool.get(session).expect("session").ctx.clone();
    crate::session_timeline::claim_supervisor(
        &ctx,
        &crate::harness_host::holder(),
        None,
        None,
        crate::metrics::now_us(),
    )
    .expect("claim");

    // This account/usage fixture models a reporting supervisor, so unavailable facts remain
    // distinguishable from missing continuation evidence in the strict guard below.
    crate::autopilot_activity::report(&ctx, "working", None);
    let rule = "\u{2500}".repeat(160);
    let labelled = format!(
        "\u{2500}\u{2500} Voice input (\u{2325} + v to start) {}",
        "\u{2500}".repeat(128)
    );
    let footer_row = "  gpt-5.4 \u{00B7} high \u{00B7} ~/aterm \u{00B7} Auto-review";
    let bare = vec![
        String::new(),
        "  Muse Code 0.9.3".into(),
        String::new(),
        labelled.clone(),
        "\u{276F} ".into(),
        rule.clone(),
        footer_row.into(),
    ];
    feed_rows(&app, session, &bare);
    frame(&mut app, wid);
    let strip = strip(&app, wid);
    let bottom = text_of(&app.windows[&wid].input_scratch.cells[strip + 5]);
    assert!(
        bottom.contains("\u{2386} redacted-user@example.test")
            && !bottom.contains("29% left")
            && email_of(&app).as_deref() == Some("redacted-user@example.test"),
        "CONTROL: no card shown yet: {bottom:?}"
    );

    let mut carded = vec![
        String::new(),
        "  Muse Code 0.9.3".into(),
        "\u{250C}".to_owned() + &"\u{2500}".repeat(55) + "\u{2510}",
        "\u{2502}  Session usage                                        \u{2502}".into(),
        "\u{2502}  Subscription \u{00B7} Muse Code Max Usage                   \u{2502}".into(),
        "\u{2502}    Weekly         71% used \u{00B7} Resets Oct 25 at 5:00 PM \u{2502}".into(),
        "\u{2514}".to_owned() + &"\u{2500}".repeat(55) + "\u{2518}",
        labelled.clone(),
        "\u{276F} ".into(),
        rule.clone(),
        footer_row.into(),
    ];
    feed_rows(&app, session, &carded);
    frame(&mut app, wid);
    frame(&mut app, wid);
    let top = text_of(&app.windows[&wid].input_scratch.cells[strip + 7]);
    assert!(
        !top.contains("Voice input") && top.starts_with("\u{2500}\u{2500}    muse \u{2500}"),
        "the lockup at the top rule's left, where its idle label was: {top:?}"
    );
    let bottom = text_of(&app.windows[&wid].input_scratch.cells[strip + 9]);
    assert!(
        bottom.contains("\u{2386} redacted-user@example.test 29% left")
            && !bottom.contains("Muse Code Max Usage")
            && !bottom.contains("unavailable"),
        "the usage card populates its weekly beside the account: {bottom:?}"
    );
    assert_eq!(
        plan_of(&app).as_deref(),
        Some("Muse Code Max Usage"),
        "and its tier, for the hover"
    );
    let mut account_only = carded.clone();
    let status = vec![
        "┌".to_owned() + &"─".repeat(83) + "┐",
        format!("│{:<83}│", "  MUSE CODE 1.4.11 / first-session IDLE"),
        format!(
            "│{:<83}│",
            "  ACCOUNT        Redacted User (redacted-user@example.test)"
        ),
        "└".to_owned() + &"─".repeat(83) + "┘",
    ];
    drop(account_only.splice(7..7, status));
    let account_only_bottom = strip + account_only.len() - 2;
    feed_rows(&app, session, &account_only);
    frame(&mut app, wid);
    frame(&mut app, wid);
    let bottom = text_of(&app.windows[&wid].input_scratch.cells[account_only_bottom]);
    assert!(
        bottom.contains("\u{2386} redacted-user@example.test")
            && email_of(&app).as_deref() == Some("redacted-user@example.test")
            && plan_of(&app).as_deref() != Some("Muse Code Max Usage"),
        "a named status card cannot borrow another card's plan: {bottom:?}"
    );
    feed_rows(&app, session, &carded);
    frame(&mut app, wid);
    frame(&mut app, wid);
    let bottom = text_of(&app.windows[&wid].input_scratch.cells[strip + 9]);
    assert!(
        plan_of(&app).as_deref() == Some("Muse Code Max Usage")
            && bottom.contains("\u{2386} redacted-user@example.test 29% left"),
        "usage-only bootstrap remains available before any switch: {bottom:?}"
    );
    let already_read = clock_reads.get();
    frame(&mut app, wid);
    assert_eq!(
        clock_reads.get(),
        already_read,
        "a settled reset does not read the clock again"
    );
    now.set(RESET);
    let timeline = &app.pool.get(session).expect("session").ctx.timeline;
    let mut identity = unread(6161, std::path::Path::new("unused"));
    identity.dir = None;
    resolve_facts_at_with_cwd(
        timeline,
        &mut identity,
        &mut footer::TailCache::default(),
        &mut footer::FooterCache::default(),
        &mut RosterMemo::default(),
        (
            Asks {
                files: |_| None,
                tap: NO_TAP,
                args: |_| None,
                roster: || None,
                home: || None,
                route: |_, _, _| accounts::AuthRoute::Outside,
            },
            |_| None,
        ),
        &footer::ReadClock {
            read_at: std::time::UNIX_EPOCH,
            now: now.get(),
            offset_at: &|_, _| Some(0),
        },
    );
    {
        let held = timeline.lock().unwrap();
        let sub = held.claude_footer().unwrap().subscription.as_ref().unwrap();
        assert!(sub.weekly.is_none());
        assert_eq!(
            sub.weekly_reset.as_ref().and_then(|reset| reset.at),
            Some(RESET)
        );
    }
    frame(&mut app, wid);
    app.windows
        .get_mut(&wid)
        .unwrap()
        .claude_footer_texts
        .borrow_mut()
        .trim(0);
    frame(&mut app, wid);
    let bottom = text_of(&app.windows[&wid].input_scratch.cells[strip + 9]);
    assert!(
        bottom.contains("\u{2386} redacted-user@example.test") && !bottom.contains("29% left"),
        "an unchanged expired card cannot graft its quota again: {bottom:?}"
    );
    assert_eq!(
        clock_reads.get(),
        already_read,
        "expiry tombstones need no frame clock"
    );
    carded[5] = carded[5].replace("Oct 25", "Oct 26");
    feed_rows(&app, session, &carded);
    frame(&mut app, wid);
    frame(&mut app, wid);
    let bottom = text_of(&app.windows[&wid].input_scratch.cells[strip + 9]);
    assert!(
        bottom.contains("\u{2386} redacted-user@example.test 29% left"),
        "a new window is admitted: {bottom:?}"
    );
    {
        let entry = app.pool.get(session).expect("session");
        let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
        let mut facts = timeline.claude_footer().cloned().expect("facts");
        facts.subscription.as_mut().expect("subscription").email =
            Some("second@example.test".into());
        assert!(timeline.set_claude_footer(6161, None, Some(facts)));
        assert!(timeline.muse_cards_need_account());
        let sub = timeline
            .claude_footer()
            .unwrap()
            .subscription
            .as_ref()
            .unwrap();
        assert_eq!(
            (&sub.plan, &sub.weekly, &sub.weekly_reset),
            (&None, &None, &None)
        );
    }
    frame(&mut app, wid);
    frame(&mut app, wid);
    let bottom = text_of(&app.windows[&wid].input_scratch.cells[strip + 9]);
    assert!(
        bottom.contains("\u{2386} second@example.test")
            && email_of(&app).as_deref() == Some("second@example.test")
            && plan_of(&app).as_deref() != Some("Muse Code Max Usage")
            && !bottom.contains("29% left"),
        "anonymous old usage cannot be attributed to the new login: {bottom:?}"
    );
    app.windows
        .get_mut(&wid)
        .expect("window")
        .claude_footer_texts
        .borrow_mut()
        .trim(0);
    let mut rebuilt = carded.clone();
    rebuilt[10] = "  gpt-5.4 · high · ~/other · Auto-review".into();
    feed_rows(&app, session, &rebuilt);
    frame(&mut app, wid);
    frame(&mut app, wid);
    let bottom = text_of(&app.windows[&wid].input_scratch.cells[strip + 9]);
    assert!(
        bottom.contains("\u{2386} second@example.test")
            && email_of(&app).as_deref() == Some("second@example.test")
            && plan_of(&app).as_deref() != Some("Muse Code Max Usage")
            && !bottom.contains("29% left"),
        "a new pane/text generation cannot reattribute the old card: {bottom:?}"
    );
    let stale = vec![
        String::new(),
        "  Muse Code 0.9.3".into(),
        "\u{250C}".to_owned() + &"\u{2500}".repeat(83) + "\u{2510}",
        "\u{2502}  MUSE CODE 1.4.11 / redacted-session                                            IDLE \u{2502}".into(),
        "\u{2502}  ACCOUNT        Redacted User (redacted-user@example.test)                          \u{2502}".into(),
        "\u{2502}  BILLING        Subscription \u{00B7} Muse Code Max Usage                                  \u{2502}".into(),
        "\u{2514}".to_owned() + &"\u{2500}".repeat(83) + "\u{2518}",
        "\u{250C}".to_owned() + &"\u{2500}".repeat(55) + "\u{2510}",
        "\u{2502}  Session usage                                        \u{2502}".into(),
        "\u{2502}  Subscription \u{00B7} Muse Code Max Usage                   \u{2502}".into(),
        "\u{2502}    Weekly         71% used \u{00B7} Resets Oct 25 at 5:00 PM \u{2502}".into(),
        "\u{2514}".to_owned() + &"\u{2500}".repeat(55) + "\u{2518}",
        labelled,
        "\u{276F} ".into(),
        rule,
        footer_row.into(),
    ];
    let stale_bottom_row = strip + stale.len() - 2;
    feed_rows(&app, session, &stale);
    frame(&mut app, wid);
    frame(&mut app, wid);
    let bottom = text_of(&app.windows[&wid].input_scratch.cells[stale_bottom_row]);
    assert!(
        bottom.contains("\u{2386} second@example.test")
            && email_of(&app).as_deref() == Some("second@example.test")
            && !bottom.contains("redacted-user")
            && !bottom.contains("29% left"),
        "the previous account's cards stay historical after a switch: {bottom:?}"
    );
    let mut current = stale.clone();
    current[4] = format!(
        "│{:<83}│",
        "  ACCOUNT        Second User (second@example.test)"
    );
    current[5] = format!("│{:<83}│", "  BILLING        Subscription · Muse Code Pro");
    feed_rows(&app, session, &current);
    frame(&mut app, wid);
    frame(&mut app, wid);
    let bottom = text_of(&app.windows[&wid].input_scratch.cells[stale_bottom_row]);
    assert!(
        bottom.contains("\u{2386} second@example.test")
            && email_of(&app).as_deref() == Some("second@example.test")
            && plan_of(&app).as_deref() == Some("Muse Code Pro")
            && plan_of(&app).as_deref() != Some("Muse Code Max Usage")
            && !bottom.contains("29% left"),
        "only the matching status card's own fields are attributable: {bottom:?}"
    );
    {
        let entry = app.pool.get(session).expect("session");
        let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
        assert!(timeline.clear_claude_footer());
        timeline.note_foreground_group(6162);
        timeline.set_program(6162, Some("muse".into()));
        assert!(
            timeline.muse_cards_need_account(),
            "clearing facts and changing process cannot clear surviving old glass"
        );
    }
    host.shutdown_and_join();
}

#[test]
fn a_pane_with_no_context_reading_draws_no_context_fact() {
    const COLS: usize = 240;
    const READING: &str = "ctx 84% left";
    let mark = footer::CONTEXT_MARK;
    let rule = "\u{2500}".repeat(COLS);
    let indicator = format!(
        "{:>COLS$}",
        "Context left until auto-compact: 84%",
        COLS = COLS
    );
    let over_composer = |shown: bool| {
        if shown {
            indicator.clone()
        } else {
            String::new()
        }
    };
    let claude_screen = |ctx: bool, mode_row_end: &str| -> Vec<String> {
        let hints = "  \u{23F5}\u{23F5} bypass permissions on (shift+tab to cycle)";
        let pad = COLS - hints.chars().count() - mode_row_end.chars().count();
        vec![
            String::new(),
            " \u{2590}\u{259B}\u{2588}\u{2588}\u{2588}\u{259C}\u{258C}   Claude Code v2.1.92".into(),
            "\u{259D}\u{259C}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{259B}\u{2598}  Opus 4.6 \u{00B7} Claude Max".into(),
            "  \u{2598}\u{2598} \u{259D}\u{259D}    ~/aterm".into(),
            over_composer(ctx),
            rule.clone(),
            "\u{276F} ".into(),
            rule.clone(),
            format!("{hints}{}{mode_row_end}", " ".repeat(pad)),
        ]
    };
    let agy_screen = vec![
        "      \u{2584}\u{2580}\u{2580}\u{2584}        Antigravity CLI 1.4.0".to_string(),
        "    \u{2580}\u{2580}\u{2580}\u{2580}\u{2580}\u{2580}\u{2580}\u{2580}      Gemini 4 Argon (Medium)".into(),
        String::new(),
        rule.clone(),
        "> ".into(),
        rule.clone(),
        "? for shortcuts".into(),
    ];
    let muse_screen = vec![
        String::new(),
        "  Muse Code 0.9.3".to_string(),
        String::new(),
        rule.clone(),
        "\u{276F} ".into(),
        rule.clone(),
        "  gpt-5.4 \u{00B7} high \u{00B7} ~/aterm \u{00B7} Auto-review".into(),
    ];
    let bottom = |app: &App, wid: WindowId, rows: &[String]| {
        let at = rows
            .iter()
            .rposition(|r| r.starts_with('\u{2500}'))
            .expect("the composer's bottom rule");
        text_of(&app.windows[&wid].input_scratch.cells[strip(app, wid) + at])
    };
    let named = |app: &App, session: u64, pgid: i32, program: &str| {
        let entry = app.pool.get(session).expect("session");
        let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
        timeline.note_foreground_group(pgid);
        assert!(timeline.set_program(pgid, Some(program.into())));
        entry.ctx.timeline.clone()
    };
    let context = |timeline: &Arc<Mutex<SessionTimeline>>| {
        timeline
            .lock()
            .unwrap()
            .claude_footer()
            .expect("the pane's facts")
            .context
            .clone()
    };
    let asks = Asks {
        files: |_| None,
        tap: NO_TAP,
        args: |_| Some(args_of(&["HOME=/home/someone"])),
        roster: || None,
        home: scratch_home,
        route: |_, _, _| accounts::AuthRoute::Stored,
    };
    // No reading, no context fact (nor a word for a value the rule does not know); the
    // reading's arrival after it is the control that the facts are drawn there.
    let fresh = |shown: &str, program: &str| {
        assert!(
            !shown.contains(mark) && !shown.contains("unavailable"),
            "{program}: the facts, and no context fact without a reading: {shown:?}"
        );
    };
    let drawn = |shown: &str, program: &str, reading: &str| {
        assert!(
            shown.contains(&format!("{mark} {reading}")),
            "{program}: a reading is drawn: {shown:?}"
        );
    };

    const PID: i32 = 4261;
    const START: u64 = 1_790_607_861;
    let (root, home) = claude_home("footer-no-context-reading", PID, START);
    let (mut app, wid, session) = glass(COLS as u16);
    let timeline = named(&app, session, PID, "claude");
    let mut identity = unread(PID, &home);
    identity.started = Some(START);
    let mut tail = footer::TailCache::default();
    let mut fold = footer::FooterCache::default();
    let mut memo = RosterMemo::default();
    resolve_facts(
        &timeline,
        &mut identity,
        &mut tail,
        &mut fold,
        &mut memo,
        asks,
    );
    assert_eq!(context(&timeline), None, "claude: no reading, no fact");
    let screen = claude_screen(false, "");
    feed_rows(&app, session, &screen);
    frame(&mut app, wid);
    fresh(&bottom(&app, wid, &screen), "claude");
    let screen = claude_screen(true, "");
    feed_rows(&app, session, &screen);
    frame(&mut app, wid);
    drawn(&bottom(&app, wid, &screen), "claude", READING);
    resolve_facts(
        &timeline,
        &mut identity,
        &mut tail,
        &mut fold,
        &mut memo,
        asks,
    );
    assert_eq!(
        context(&timeline).as_deref(),
        Some(READING),
        "claude: the reading stays through the next registry read"
    );
    let screen = claude_screen(false, "61% until auto-compact");
    feed_rows(&app, session, &screen);
    frame(&mut app, wid);
    drawn(&bottom(&app, wid, &screen), "claude", "ctx 61% left");
    std::fs::remove_dir_all(root).unwrap();

    for (pgid, program, screen) in [(7001, "agy", &agy_screen), (7002, "muse", &muse_screen)] {
        let (mut app, wid, session) = glass(COLS as u16);
        let timeline = named(&app, session, pgid, program);
        feed_rows(&app, session, screen);
        frame(&mut app, wid);
        assert_eq!(context(&timeline), None, "{program}: no reading, no fact");
        fresh(&bottom(&app, wid, screen), program);
        {
            let mut held = timeline.lock().unwrap();
            let mut facts = held.claude_footer().cloned().expect("the frame's facts");
            facts.context = Some(READING.into());
            assert!(held.set_claude_footer(pgid, None, Some(facts)));
        }
        frame(&mut app, wid);
        drawn(&bottom(&app, wid, screen), program, READING);
    }

    const NO_PROCESS: i32 = 999_983;
    let (app, _, session) = glass(COLS as u16);
    let timeline = named(&app, session, NO_PROCESS, "agy");
    let mut identity = unread(NO_PROCESS, &home);
    identity.dir = None;
    let fill = |identity: &mut Identity| {
        resolve_facts(
            &timeline,
            identity,
            &mut footer::TailCache::default(),
            &mut footer::FooterCache::default(),
            &mut RosterMemo::default(),
            asks,
        );
    };
    fill(&mut identity);
    assert_eq!(
        context(&timeline),
        None,
        "agy, resolved: no reading, no fact"
    );
    {
        let mut held = timeline.lock().unwrap();
        let mut facts = held.claude_footer().cloned().expect("the resolver's facts");
        facts.context = Some(READING.into());
        assert!(held.set_claude_footer(NO_PROCESS, None, Some(facts)));
    }
    fill(&mut identity);
    assert_eq!(
        context(&timeline).as_deref(),
        Some(READING),
        "agy, resolved: the reading stays through the next fill"
    );
}

#[test]
fn kimi_facts_resolve_outside_claudes_registry() {
    const PID: i32 = 999_984;
    let (root, home) = claude_home("footer-kimi-facts", PID, 1_790_607_861);
    let (app, _, session) = glass(160);
    let timeline = {
        let entry = app.pool.get(session).expect("session");
        let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
        timeline.note_foreground_group(PID);
        assert!(timeline.set_program(PID, Some("kimi".into())));
        entry.ctx.timeline.clone()
    };
    let mut identity = unread(PID, &home);
    identity.dir = None;
    resolve_facts(
        &timeline,
        &mut identity,
        &mut footer::TailCache::default(),
        &mut footer::FooterCache::default(),
        &mut RosterMemo::default(),
        Asks {
            files: |_| None,
            tap: NO_TAP,
            args: |_| Some(args_of(&["HOME=/home/someone"])),
            roster: || None,
            home: scratch_home,
            route: |_, _, _| accounts::AuthRoute::Stored,
        },
    );
    let facts = timeline
        .lock()
        .unwrap()
        .claude_footer()
        .cloned()
        .expect("Kimi's resolved facts");
    assert_eq!(
        facts.owner.map(|owner| owner.session_id),
        Some(format!("kimi-{PID}"))
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_context_reading_goes_when_claude_code_stops_showing_it() {
    const COLS: usize = 160;
    const PID: i32 = 4262;
    let mark = footer::CONTEXT_MARK;
    let rule = "\u{2500}".repeat(COLS);
    let footer_row = |left: &str, end: &str| {
        let pad = COLS - 2 - left.chars().count() - end.chars().count();
        format!("{left}{}{end}", " ".repeat(pad))
    };
    let rest = "  \u{23F8} manual mode on \u{00B7} ? for shortcuts";
    let screen = |caret: &str, under: Vec<String>| -> Vec<String> {
        let mut rows = vec![
            "\u{23FA} Hi! I am the local lab stub, not a model. (reply 1)".to_string(),
            String::new(),
            rule.clone(),
            caret.to_string(),
            rule.clone(),
        ];
        rows.extend(under);
        rows
    };
    let shown = screen("\u{276F}", vec![footer_row(rest, "61% until auto-compact")]);
    let notice = screen(
        "\u{276F} h",
        vec![footer_row(
            "  \u{23F8} manual mode on",
            "\u{25D0} medium \u{00B7} /effort",
        )],
    );
    let menu = screen(
        "\u{276F} /",
        vec![
            "  \u{276F} /add-dir                 Add a new working directory".to_string(),
            "    /agents                  Manage agent configurations".to_string(),
            "    /autocompact             Set how full the context gets before auto-summarizing"
                .to_string(),
        ],
    );
    let mut boxed = aterm_phase::prompt::fixtures::bash_one_row();
    let last = boxed.len() - 1;
    boxed[last] = footer_row(rest, "");
    let gone = screen("\u{276F}", vec![footer_row(rest, "")]);
    let bottom = |app: &App, wid: WindowId, rows: &[String]| {
        let at = rows
            .iter()
            .rposition(|r| r.starts_with('\u{2500}'))
            .expect("the composer's bottom rule");
        text_of(&app.windows[&wid].input_scratch.cells[strip(app, wid) + at])
    };
    let (mut app, wid, session) = glass(COLS as u16);
    let timeline = {
        let entry = app.pool.get(session).expect("session");
        let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
        timeline.note_foreground_group(PID);
        assert!(timeline.set_program(PID, Some("claude".into())));
        assert!(timeline.set_claude_footer(
            PID,
            None,
            Some(FooterFacts {
                subscription: Some(aterm_agent::harness::profile::SubscriptionDisplay {
                    email: Some("owner@example.com".into()),
                    ..Default::default()
                }),
                ..FooterFacts::default()
            }),
        ));
        entry.ctx.timeline.clone()
    };
    let context = || {
        timeline
            .lock()
            .unwrap()
            .claude_footer()
            .expect("the pane's facts")
            .context
            .clone()
    };
    let mut show = |rows: &[String]| {
        feed_rows(&app, session, rows);
        frame(&mut app, wid);
        bottom(&app, wid, rows)
    };
    let reading = format!("{mark} ctx 61% left");
    let drawn = show(&shown);
    assert!(drawn.contains(&reading), "the indicator, read: {drawn:?}");
    for (what, rows) in [
        ("the effort notice in the slot", &notice),
        ("the / menu in the mode row's place", &menu),
        ("a box over the composer", &boxed),
    ] {
        let drawn = show(rows);
        assert!(
            drawn.contains(&reading),
            "{what}: the reading stays: {drawn:?}"
        );
        assert_eq!(context().as_deref(), Some("ctx 61% left"), "{what}");
    }
    let drawn = show(&gone);
    assert!(
        drawn.contains(footer::ACCOUNT_MARK) && !drawn.contains(mark),
        "the slot empty: the fact goes with the indicator: {drawn:?}"
    );
    assert_eq!(context(), None, "and from the session's facts");
    let drawn = show(&shown);
    assert!(
        drawn.contains(&reading),
        "shown again, drawn again: {drawn:?}"
    );
}

#[test]
fn autopilot_off_keeps_the_harness_lit_in_white_for_claude_agy_and_muse() {
    let rule = "\u{2500}".repeat(160);
    let claude_fixture = vec![
        String::new(),
        " \u{2590}\u{259B}\u{2588}\u{2588}\u{2588}\u{259C}\u{258C}   Claude Code v2.1.92".into(),
        "\u{259D}\u{259C}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{259B}\u{2598}  Opus 4.6 \u{00B7} Claude Max".into(),
        "  \u{2598}\u{2598} \u{259D}\u{259D}    ~/aterm".into(),
        String::new(),
        rule.clone(),
        "\u{276F} ".into(),
        rule.clone(),
        format!(
            "{:<136}{}",
            "  \u{23F5}\u{23F5} bypass permissions on (shift+tab to cycle)",
            "84% until auto-compact"
        ),
    ];
    let agy_fixture = vec![
        "      \u{2584}\u{2580}\u{2580}\u{2584}        Antigravity CLI 1.4.0".into(),
        "     \u{2580}\u{2580}\u{2580}\u{2580}\u{2580}\u{2580}       redacted-user@example.test (Google AI Ultra)".into(),
        "    \u{2580}\u{2580}\u{2580}\u{2580}\u{2580}\u{2580}\u{2580}\u{2580}      Gemini 4 Argon (Medium)".into(),
        "   \u{2584}\u{2580}\u{2580}    \u{2580}\u{2580}\u{2584}     ~/aterm".into(),
        "  \u{2584}\u{2580}\u{2580}      \u{2580}\u{2580}\u{2584}".into(),
        "\u{25CF} # Heading".into(),
        rule.clone(),
        "> ".into(),
        rule.clone(),
        "? for shortcuts                                                                                  Gemini 4 Argon \u{00B7} medium".into(),
    ];
    let muse_fixture = vec![
        String::new(),
        "  Muse Code 0.9.3".into(),
        String::new(),
        format!(
            "\u{2500}\u{2500} Voice input (\u{2325} + v to start) {}",
            "\u{2500}".repeat(128)
        ),
        "\u{276F} ".into(),
        rule,
        "  gpt-5.4 \u{00B7} high \u{00B7} ~/aterm \u{00B7} Auto-review".into(),
    ];

    for (prog, build, rows, top_rule_row, card_row) in [
        ("claude", "2.1.92", claude_fixture, 5usize, 4usize),
        ("agy", "1.4.2", agy_fixture, 6usize, 4usize),
        ("muse", "0.9.3", muse_fixture, 3usize, 2usize),
    ] {
        let (mut app, wid, session) = glass(160);
        {
            let entry = app.pool.get(session).expect("session");
            let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            timeline.note_foreground_group(7000);
            timeline.set_program(7000, Some(prog.into()));
            assert!(timeline.set_claude_footer(
                7000,
                None,
                Some(FooterFacts {
                    model_open: true,
                    effort_open: true,
                    path: Some("~/aterm".into()),
                    branch: Some("main".into()),
                    owner: Some(footer::FactsOwner {
                        floor: None,
                        session_id: format!("{prog}-7000"),
                    }),
                    subscription: Some(
                        aterm_agent::harness::profile::SubscriptionDisplay::default(),
                    ),
                    context: Some("ctx 84% left".into()),
                    goal: Some("implement vision".into()),
                    ..FooterFacts::default()
                }),
            ));
        }
        let host = crate::harness_host::HostHandle::for_supervised_test(
            Vec::new(),
            std::sync::Arc::new(|_| crate::harness_host::BodyEnd::Stopped),
        );
        app.harness = Some(host.clone());
        let ctx = app.pool.get(session).expect("session").ctx.clone();
        crate::session_timeline::claim_supervisor(
            &ctx,
            &crate::harness_host::holder(),
            None,
            None,
            crate::metrics::now_us(),
        )
        .expect("claim");

        super::probe::drawer(&app, session, 7000, prog, build);
        feed_rows(&app, session, &rows);
        frame(&mut app, wid);

        let strip = strip(&app, wid);
        let missing = text_of(&app.windows[&wid].input_scratch.cells[strip + top_rule_row + 2]);
        assert!(
            missing.contains("status unavailable"),
            "{prog}: an enabled mode explains its missing supervisor report: {missing:?}"
        );
        crate::autopilot_activity::report(&ctx, "working", None);
        frame(&mut app, wid);
        let expected_lockup = format!("\u{2500}\u{2500}    {prog} \u{2500}");
        let harness = match prog {
            "agy" => aterm_phase::CliHarness::Agy,
            "muse" => aterm_phase::CliHarness::Muse,
            _ => aterm_phase::CliHarness::ClaudeCode,
        };
        let label = super::probe::harness_label(harness);
        let expected_switch = &format!("{} {label}", super::probe::LIT_SWITCH);
        let brand = format!(
            "enhanced by alab {} harness v{}",
            harness.cli(),
            harness.version_str()
        );
        let sid = app.pool.get(session).expect("session").ctx.self_id.clone();
        let chip_line = |app: &App| {
            let head = format!("autopilot sid={} ", sid.as_str());
            app.drawn_chip_lines()
                .into_iter()
                .find(|l| l.starts_with(&head))
                .expect("an autopilot line for the session")
        };
        let line_on = chip_line(&app);
        assert!(
            line_on.contains(" drawn=\"") && !line_on.contains(" why="),
            "{prog}: the harness on draws its chip: {line_on}"
        );
        let top_on = text_of(&app.windows[&wid].input_scratch.cells[strip + top_rule_row]);
        assert!(
            top_on.starts_with(&expected_lockup) && top_on.contains(expected_switch),
            "{prog}: active harness lays the lockup at the top rule's left and the switch at its right: {top_on:?}"
        );
        let bottom_on = text_of(&app.windows[&wid].input_scratch.cells[strip + top_rule_row + 2]);
        assert!(
            bottom_on.contains("working"),
            "{prog}: the current supervisor report replaces missing evidence: {bottom_on:?}"
        );
        assert!(
            bottom_on.contains("\u{25D0} ctx 84% left") && !bottom_on.contains("unavailable"),
            "{prog}: standardized bottom rule carries the context, and no value it does not know: {bottom_on:?}"
        );
        if prog == "agy" {
            assert!(
                bottom_on.contains("\u{2386} redacted-user@example.test")
                    && !bottom_on.contains("Google AI Ultra"),
                "agy: the launch card's account populates the standardized footer, its plan the hover: {bottom_on:?}"
            );
        }
        if prog == "muse" {
            assert!(
                !top_on.contains("Voice input"),
                "muse: the lockup takes the idle voice label's cells: {top_on:?}"
            );
            let row = &app.windows[&wid].input_scratch.cells[strip + top_rule_row];
            let at = row
                .windows(4)
                .position(|w| w.iter().map(|c| c.ch).eq("muse".chars()))
                .expect("the muse word");
            for letter in &row[at..at + 4] {
                assert!(
                    letter.fg[2] > 150 && letter.fg[0] < 80,
                    "muse: the name is Meta blue, not purple: {:?}",
                    letter.fg
                );
            }
        }
        let card_on = text_of(&app.windows[&wid].input_scratch.cells[strip + card_row]);
        if prog == "agy" {
            assert!(
                card_on.contains(&brand),
                "agy: active harness draws launch card banner: {card_on:?}"
            );
        } else {
            assert!(
                card_on.contains(&brand) && !card_on.contains("autopilot"),
                "{prog}: active harness draws launch card banner: {card_on:?}"
            );
        }
        assert!(
            app.windows[&wid]
                .claude_card
                .painted_layers()
                .iter()
                .any(
                    |l| l.slot == aterm_core::render::FxSlot::Over && !l.content.items().is_empty()
                ),
            "{prog}: active harness draws rainbow kitty"
        );

        assert!(app.set_session_autopilot(
            session,
            Some(aterm_agent::supervise::Autopilot::Off),
            crate::autopilot::Surface::Chip
        ));
        assert_eq!(
            app.session_autopilot(session).1.mode(),
            Some(aterm_agent::supervise::Autopilot::Off),
            "{prog}: the session's word is written Off"
        );
        frame(&mut app, wid);

        let top_off = text_of(&app.windows[&wid].input_scratch.cells[strip + top_rule_row]);
        let card_off = text_of(&app.windows[&wid].input_scratch.cells[strip + card_row]);
        let bottom_off = text_of(&app.windows[&wid].input_scratch.cells[strip + top_rule_row + 2]);
        assert!(
            !bottom_off.contains("working") && !bottom_off.contains("status unavailable"),
            "{prog}: Off suppresses continuation status: {bottom_off:?}"
        );
        let line_off = chip_line(&app);
        assert!(
            line_off.contains(" face=off drawn=\"\u{25CB}\" "),
            "{prog}: the off chip is drawn: {line_off}"
        );
        assert!(
            top_off.contains(expected_switch)
                && top_off.starts_with(&expected_lockup)
                && !top_off.contains("alab kitty"),
            "{prog}: Autopilot Off keeps the harness's switch lit on the white top rule, the \
             lockup still at its left and no wordmark: {top_off:?}"
        );
        assert!(
            card_off.contains(&brand) && !card_off.contains("autopilot"),
            "{prog}: Autopilot Off wears the white card: {card_off:?}"
        );
        if prog == "agy" {
            let md = text_of(&app.windows[&wid].input_scratch.cells[strip + 5]);
            assert_eq!(
                md.trim_end(),
                "\u{25CF} Heading",
                "agy: Autopilot Off keeps transcript markdown rendering active while the harness is on"
            );
            assert!(
                top_off.contains(" \u{25CE} implement vision "),
                "agy: active harness shows the goal on the top rule: {top_off:?}"
            );
            app.harness_policy = crate::harness_policy_cell::HarnessPolicyCell::new(
                crate::app_config::HarnessPolicy::read("[harness]\nenabled = false\n"),
            );
            frame(&mut app, wid);
            let top_switch_off =
                text_of(&app.windows[&wid].input_scratch.cells[strip + top_rule_row]);
            assert!(
                top_switch_off.contains(&format!("{} {label}", super::probe::OFF_SWITCH))
                    && !top_switch_off.starts_with(&expected_lockup)
                    && !top_switch_off.contains("implement vision"),
                "agy: toggled-off harness leaves only the off switch button on the top rule: {top_switch_off:?}"
            );
            let raw_md = text_of(&app.windows[&wid].input_scratch.cells[strip + 5]);
            assert_eq!(
                raw_md.trim_end(),
                "\u{25CF} # Heading",
                "agy: toggled-off harness restores raw transcript row"
            );
            app.harness_policy = crate::harness_policy_cell::HarnessPolicyCell::new(
                crate::app_config::HarnessPolicy::read("[harness]\n"),
            );
            frame(&mut app, wid);
            let top_switch_on =
                text_of(&app.windows[&wid].input_scratch.cells[strip + top_rule_row]);
            assert!(
                top_switch_on.contains(expected_switch)
                    && !top_switch_on.contains("turning off")
                    && top_switch_on.contains(" \u{25CE} implement vision "),
                "agy: turning the harness back on immediately shows ● on and restores the goal: {top_switch_on:?}"
            );
        }

        assert!(app.set_session_autopilot(
            session,
            Some(aterm_agent::supervise::Autopilot::Cruise),
            crate::autopilot::Surface::Chip
        ));
        assert_eq!(
            app.session_autopilot(session).1.mode(),
            Some(aterm_agent::supervise::Autopilot::Cruise),
            "{prog}: the session's word is written Cruise again"
        );
        frame(&mut app, wid);

        let top_reenabled = text_of(&app.windows[&wid].input_scratch.cells[strip + top_rule_row]);
        assert!(
            top_reenabled.contains(expected_switch),
            "{prog}: re-enabled harness restores composer top rule branding: {top_reenabled:?}"
        );
        let card_reenabled = text_of(&app.windows[&wid].input_scratch.cells[strip + card_row]);
        assert!(
            card_reenabled.contains(&brand),
            "{prog}: re-enabled harness restores launch card banner: {card_reenabled:?}"
        );

        assert!(
            !app.windows[&wid].claude_lights.border_toggles.is_empty(),
            "{prog}: border_toggles registered when On"
        );
        assert!(app.set_session_autopilot(
            session,
            Some(aterm_agent::supervise::Autopilot::Off),
            crate::autopilot::Surface::Chip
        ));
        frame(&mut app, wid);
        assert!(
            !app.windows[&wid].claude_lights.border_toggles.is_empty(),
            "{prog}: border_toggles still registered under Autopilot Off: the harness's switch"
        );

        host.shutdown_and_join();
    }
}

#[test]
fn the_composer_border_wears_the_cards_palette() {
    use aterm_agent::supervise::Autopilot;
    let rule = "\u{2500}".repeat(120);
    let rows: Vec<String> = vec![
        String::new(),
        " \u{2590}\u{259B}\u{2588}\u{2588}\u{2588}\u{259C}\u{258C}   Claude Code v2.1.92".into(),
        "\u{259D}\u{259C}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{259B}\u{2598}  Opus 4.6 \u{00B7} Claude Max".into(),
        "  \u{2598}\u{2598} \u{259D}\u{259D}    ~/aterm".into(),
        String::new(),
        rule.clone(),
        "\u{276F} ".into(),
        rule,
        "  \u{23F5}\u{23F5} bypass permissions on (shift+tab to cycle)".into(),
    ];
    let (mut app, wid, session) = glass(120);
    {
        let entry = app.pool.get(session).expect("session");
        let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
        timeline.note_foreground_group(7000);
        timeline.set_program(7000, Some("claude".into()));
        assert!(timeline.set_claude_footer(
            7000,
            None,
            Some(FooterFacts {
                model_open: true,
                effort_open: true,
                path: Some("~/aterm".into()),
                branch: Some("main".into()),
                owner: Some(footer::FactsOwner {
                    floor: None,
                    session_id: "claude-7000".into(),
                }),
                ..FooterFacts::default()
            }),
        ));
    }
    let host = crate::harness_host::HostHandle::for_supervised_test(
        Vec::new(),
        std::sync::Arc::new(|_| crate::harness_host::BodyEnd::Stopped),
    );
    app.harness = Some(host.clone());
    let ctx = app.pool.get(session).expect("session").ctx.clone();
    let claim = || {
        crate::session_timeline::claim_supervisor(
            &ctx,
            &crate::harness_host::holder(),
            None,
            None,
            crate::metrics::now_us(),
        )
        .expect("claim");
    };
    let release = || {
        crate::session_timeline::release_supervisor(
            &ctx,
            None,
            crate::metrics::now_us(),
            aterm_slog::Actor::Ctl,
        );
    };
    feed_rows(&app, session, &rows);
    let under = |app: &mut App, word: Autopilot| {
        app.set_session_autopilot(session, Some(word), crate::autopilot::Surface::Chip);
        frame(app, wid);
        let top = strip(app, wid) + 5;
        let cells = &app.windows[&wid].input_scratch.cells[top];
        let run: Vec<[u8; 3]> = cells
            .iter()
            .filter(|c| c.ch == '\u{2500}')
            .map(|c| c.fg)
            .collect();
        (run, text_of(cells))
    };

    app.config.claude_footer = Some(crate::app_config::ClaudeFooterConfig {
        card: Some(false),
        ..Default::default()
    });
    let label = super::probe::harness_label(aterm_phase::CliHarness::ClaudeCode);
    let lit = format!("{} {label}", super::probe::LIT_SWITCH);
    let (bare, text) = under(&mut app, Autopilot::Cruise);
    assert!(
        bare.windows(2).all(|w| w[0] == w[1]) && !text.contains("alab kitty"),
        "CONTROL: unclaimed bare, the rule untinted: {text:?}"
    );
    let plan = app.active_visible_leaf_plan(wid).expect("plan");
    let unclaimed = app.claude_footer_fp(wid, &plan);
    assert_eq!(app.claude_footer_fp(wid, &plan), unclaimed, "no churn");
    claim();
    assert_ne!(
        app.claude_footer_fp(wid, &plan),
        unclaimed,
        "the claim landing repaints the border with the card off"
    );
    let (claimed, text) = under(&mut app, Autopilot::Cruise);
    assert!(
        text.contains(&lit) && claimed.windows(2).any(|w| w[0] != w[1]),
        "claimed, tinted: {text:?}"
    );
    app.config.claude_footer = None;
    let blue_violet = |c: [u8; 3]| {
        i32::from(c[2]) >= i32::from(c[0]) + 24 && i32::from(c[2]) >= i32::from(c[1]) + 24
    };
    let warm = |c: [u8; 3]| c[0] >= c[1] && c[0] >= c[2];
    let blue = |c: [u8; 3]| c[0] <= c[1] && i32::from(c[2]) >= i32::from(c[0].max(c[1])) + 56;

    let (cruise, text) = under(&mut app, Autopilot::Cruise);
    assert!(text.contains(&lit), "{text:?}");
    assert!(cruise.len() > 60, "the run: {}", cruise.len());
    assert!(
        cruise.iter().copied().any(blue_violet),
        "Cruise's rule reaches blue and violet: {cruise:?}"
    );
    assert!(
        cruise.iter().copied().any(blue),
        "Cruise's visible rule shows true blue: {cruise:?}"
    );
    let ground = app.windows[&wid].input_scratch.implicit_blank;
    let dark = crate::claude_card::dark_ground(ground);
    let paint = |t: f32| {
        let c = aterm_effects::harness_card::CardPalette::Cruise.art(t, dark);
        let stop = [(c >> 16) as u8, (c >> 8) as u8, c as u8];
        super::mix([90, 90, 90], super::mix(stop, ground.fg, 0.25), 0.68)
    };
    assert!(
        text.starts_with("\u{2500}\u{2500}    claude \u{2500}")
            && text.contains(&format!("\u{2500}{lit}"))
            && text.ends_with(" \u{2500}\u{2500}"),
        "the lockup at the rule's left, the switch and its label at its right: {text:?}"
    );
    let paint = |t: f32| {
        let fg = paint(t);
        if fg == [90, 90, 90] {
            [fg[0] ^ 1, fg[1], fg[2]]
        } else {
            fg
        }
    };
    // Every `─` wears its own column's place on the palette, the lockup's and the close's
    // too: violet at the pane's left, red at its right.
    let scratch = &app.windows[&wid].input_scratch;
    let shown: Vec<[u8; 3]> = scratch.cells[strip(&app, wid) + 5]
        .iter()
        .enumerate()
        .filter(|(_, c)| c.ch == '\u{2500}')
        .map(|(col, _)| paint(aterm_effects::glass_inks::field(col, scratch.cols)))
        .collect();
    assert_eq!(cruise, shown, "the palette backward over the pane");
    assert!(
        !cruise.iter().copied().all(warm),
        "CONTROL: Cruise's rule is not a sunset"
    );
    let mut old_half = (0..=255).map(|i| {
        let c = aterm_effects::spectrum::SPECTRUM_LUT[i];
        [(c >> 16) as u8, (c >> 8) as u8, c as u8]
    });
    assert!(
        !old_half.any(blue_violet),
        "CONTROL: the old reading stopped short of blue"
    );

    let (land, text) = under(&mut app, Autopilot::Land);
    assert!(text.contains(&lit), "{text:?}");
    assert!(
        land.iter().copied().all(warm),
        "Land's rule is the sunset: {land:?}"
    );

    let (off, text) = under(&mut app, Autopilot::Off);
    assert!(
        text.contains(&format!("{lit} ")) && !text.contains("alab kitty"),
        "Autopilot Off's switch says the harness is on, no wordmark: {text:?}"
    );
    for c in &off {
        let spread = c.iter().max().unwrap() - c.iter().min().unwrap();
        assert!(spread <= 24, "Off's rule is the moonbow: {c:?} in {off:?}");
    }
    assert_ne!(off, cruise, "Off is not Cruise");
    release();
    assert!(!app.session_autopilot(session).2, "the claim is given back");
    let (released, text) = under(&mut app, Autopilot::Off);
    assert!(text.contains(&lit), "no claim: {text:?}");
    assert_eq!(released, off, "white with no claim");
    let (frame_row, cols) = app.windows[&wid]
        .claude_lights
        .border_toggles
        .iter()
        .find(|t| t.session == session)
        .map(|t| (t.frame_row, t.cols.clone()))
        .expect("the switch is a target");
    let col = cols.start + 2;
    let (cw, ch) = app.win_cell_size(wid);
    let (ox, oy) = app.frame_origin(wid);
    let x = ox as f64 + (app.win_pad(wid) + col * cw + cw / 2) as f64;
    let y = oy as f64 + (app.win_pad_top(wid) + app.win_head(wid) + frame_row * ch + ch / 2) as f64;
    assert_eq!(app.frame_cell_at(wid, x, y), Some((frame_row, col)));
    app.windows.get_mut(&wid).expect("window").last_cursor_px = (x, y);
    assert!(
        app.press_claude_light(wid, std::time::Instant::now()),
        "the switch takes the press"
    );
    assert_eq!(
        app.session_autopilot(session).0,
        Some(Autopilot::Off),
        "the lit switch writes no Autopilot word"
    );
    let wrote = app
        .messages
        .log()
        .records()
        .rfind(|r| r.key.as_deref() == Some("harness.switch-save.harness.enabled"))
        .map(|r| r.detail.join(" "))
        .expect("the master switch's write was made");
    assert!(
        wrote.contains("harness.enabled = false"),
        "the lit switch turns the whole harness off: {wrote:?}"
    );

    claim();
    {
        let term = app.pool.get(session).expect("session").term.clone();
        crate::term_lock(&term).process(b"\x1b]11;rgb:ff/ff/ff\x1b\\\x1b]10;rgb:20/20/20\x1b\\");
    }
    let pair_contrast = |a: [u8; 3], b: [u8; 3]| {
        use aterm_effects::color_math::relative_luminance;
        let ya = relative_luminance(u32::from_be_bytes([0, a[0], a[1], a[2]]));
        let yb = relative_luminance(u32::from_be_bytes([0, b[0], b[1], b[2]]));
        (ya.max(yb) + 0.05) / (ya.min(yb) + 0.05)
    };
    let contrast = |c: [u8; 3]| pair_contrast(c, [0xFF, 0xFF, 0xFF]);
    for word in [Autopilot::Cruise, Autopilot::Off] {
        let (_, text) = under(&mut app, word);
        let cells = &app.windows[&wid].input_scratch.cells[strip(&app, wid) + 5];
        if word == Autopilot::Off {
            assert!(
                !text.contains('\u{2726}'),
                "Off wears no wordmark: {text:?}"
            );
        }
        let face: Vec<char> = lit.chars().collect();
        let at = (0..cells.len().saturating_sub(face.len()))
            .find(|&k| {
                cells[k..k + face.len()]
                    .iter()
                    .map(|c| c.ch)
                    .eq(face.iter().copied())
            })
            .unwrap_or_else(|| panic!("{word:?}: the switch on paper: {text:?}"));
        let switch = super::probe::LIT_SWITCH.chars().count();
        let letters: Vec<[u8; 3]> = cells[at + switch..at + face.len()]
            .iter()
            .chain(&cells[6..12])
            .filter(|c| c.ch != ' ')
            .map(|c| c.fg)
            .collect();
        assert_eq!(
            letters.len(),
            label.replace(' ', "").chars().count() + "claude".len(),
            "{text:?}"
        );
        for c in &letters {
            assert!(contrast(*c) >= 4.5, "{word:?}: {c:?} too pale on paper");
        }
        for cell in &cells[at..at + switch] {
            if cell.ch != ' ' {
                assert!(
                    pair_contrast(cell.fg, cell.bg) >= 4.5,
                    "{word:?}: {:?} {:?} on {:?} too pale",
                    cell.ch,
                    cell.fg,
                    cell.bg
                );
            }
        }
    }
    let ink = [0x20, 0x20, 0x20];
    let head: Vec<char> = " alab harness \u{25CF} on ".chars().collect();
    let old_pale = (0..head.len()).filter(|&k| head[k] != ' ').any(|k| {
        let c = aterm_effects::spectrum::SPECTRUM_LUT[(k * 255) / head.len()];
        contrast(mix([(c >> 16) as u8, (c >> 8) as u8, c as u8], ink, 0.25)) < 4.5
    });
    assert!(old_pale, "CONTROL: the old letters vanish on paper");

    app.harness_policy = crate::harness_policy_cell::HarnessPolicyCell::new(
        crate::app_config::HarnessPolicy::read("[harness]\nenabled = false\n"),
    );
    let (_, text) = under(&mut app, Autopilot::Off);
    assert!(
        !text.contains("alab kitty")
            && text.contains(&format!("{} {label}", super::probe::OFF_SWITCH)),
        "master off: the bare off switch {text:?}"
    );
    host.shutdown_and_join();
}

#[test]
fn agy_glass_presentation_preserves_literal_cells_and_undoes_cleanly() {
    fn collect(scratch: &RenderInput) -> Vec<AgyRowEdit> {
        collect_for(scratch, aterm_phase::Program::Agy)
    }
    fn collect_for(scratch: &RenderInput, program: aterm_phase::Program) -> Vec<AgyRowEdit> {
        let rows: Vec<String> = scratch
            .cells
            .iter()
            .map(|r| r.iter().map(|c| c.ch).collect())
            .collect();
        let mut edits = Vec::new();
        collect_agy_transcript_edits(
            scratch,
            program,
            &rows,
            PaneTextRegion {
                strip: 0,
                row_off: 0,
                col_off: 0,
                rows: scratch.rows,
                cols: scratch.cols,
            },
            Some(scratch.rows),
            RenderCell::default(),
            RenderCell {
                fg: [80, 80, 80],
                ..RenderCell::default()
            },
            [180, 100, 80],
            &mut edits,
        );
        edits
    }
    let rows = [
        "Signing in...",
        "Authenticated.",
        "  └ Tip: literal tool output",
        "^[[?2026$p^[P>|aterm(0.42.0)",
        "○ Thought for 1s (literal suffix)",
        "● # Overview",
        "  Here is **bold**, *italic*, and `code`.",
        "  ```text",
        "  Signing in...",
        "  └ Tip: literal code",
        "  **literal**",
        "  ^[[?2026$p",
        "  ```",
        "  - First item",
        "  ```text",
        "──────────────",
        "> x",
        "hello",
        "  **literal**",
        "      ▄▀▀▄        Antigravity CLI 1.4.0",
        "     ▀▀▀▀▀▀       redacted-user@example.test (Google AI Ultra)",
        "    ▀▀▀▀▀▀▀▀      Gemini 4 Argon (Medium)",
        "   ▄▀▀    ▀▀▄     ~/aterm",
        "  ▄▀▀      ▀▀▄",
        "  **still literal after a quoted card**",
        "  ```",
    ];
    let quoted: Vec<String> = rows.iter().map(|r| (*r).to_owned()).collect();
    assert!(aterm_phase::agy::launch_card_scan(&quoted).1.is_some());
    let mut scratch = RenderInput::empty();
    scratch.rows = rows.len();
    scratch.cols = 100;
    scratch.cursor_visible = false;
    scratch.cells = rows.iter().map(|r| row_of(r, [0, 0, 0])).collect();
    let original = scratch.cells.clone();
    let edits = collect(&scratch);
    assert_eq!(
        edits.iter().map(|e| e.frame_row).collect::<Vec<_>>(),
        vec![5, 6, 13]
    );
    let mut undo_slot = FooterUndo::default();
    write_edits(
        &mut undo_slot,
        &mut scratch,
        edits
            .iter()
            .map(|e| (e.frame_row, e.col, e.cells.as_slice(), e.ground)),
    );
    assert_eq!(text_of(&scratch.cells[5]).trim_end(), "● Overview");
    assert!(
        scratch.cells[5][2].bold
            && scratch.cells[5][2].underline == aterm_core::terminal::UnderlineStyle::Single,
        "H1 heading is bold + underlined"
    );
    assert_eq!(
        text_of(&scratch.cells[6]).trim_end(),
        "  Here is bold, italic, and code."
    );
    assert!(scratch.cells[6][10].bold && scratch.cells[6][16].italic);
    assert_ne!(
        scratch.cells[6][28].bg,
        RenderCell::default().bg,
        "inline code pill background tinted"
    );
    assert_eq!(text_of(&scratch.cells[13]).trim_end(), "  • First item");
    for (r, original_row) in original.iter().enumerate().take(scratch.rows) {
        if ![5, 6, 13].contains(&r) {
            assert_eq!(&scratch.cells[r], original_row);
        }
    }
    assert!(undo(&mut undo_slot, &mut scratch));
    assert_eq!(scratch.cells, original);

    let md_rows = [
        "  ## Section",
        "  ### Detail",
        "  See [docs](https://example.com) now.",
        "  - [ ] Pending",
        "    - Nested",
        "  > Quoted",
    ];
    scratch.rows = md_rows.len();
    scratch.cells = md_rows.iter().map(|r| row_of(r, [0, 0, 0])).collect();
    let md_edits = collect(&scratch);
    assert_eq!(md_edits.len(), 6);
    assert_eq!(text_of(&md_edits[0].cells).trim_end(), "  Section");
    assert!(
        md_edits[0].cells[2].bold
            && !md_edits[0].cells[2].italic
            && md_edits[0].cells[2].underline == aterm_core::terminal::UnderlineStyle::None
    );
    assert_eq!(text_of(&md_edits[1].cells).trim_end(), "  Detail");
    assert!(md_edits[1].cells[2].bold && md_edits[1].cells[2].italic);
    assert_eq!(text_of(&md_edits[2].cells).trim_end(), "  See docs now.");
    assert_eq!(
        md_edits[2].cells[6].underline,
        aterm_core::terminal::UnderlineStyle::Single
    );
    assert_eq!(text_of(&md_edits[3].cells).trim_end(), "  \u{2610} Pending");
    assert_eq!(
        text_of(&md_edits[4].cells).trim_end(),
        "    \u{25E6} Nested"
    );
    assert_eq!(text_of(&md_edits[5].cells).trim_end(), "  \u{258E} Quoted");

    let muse_rows = [
        "  Muse Code 0.9.3",
        "\u{276f} user prompt",
        "  **wrapped user text stays literal**",
        "  \u{256d}\u{2500} Bash",
        "  \u{2502} **tool output stays literal**",
        "  \u{2570}\u{2500}",
        "  \u{25c7} Thinking **literal**...",
        "\u{25c6} # Muse Heading",
        "  Here is ***bold italic*** and **bold `code`**.",
        "  ---",
    ];
    scratch.rows = muse_rows.len();
    scratch.cells = muse_rows.iter().map(|r| row_of(r, [0, 0, 0])).collect();
    let muse_edits = collect_for(&scratch, aterm_phase::Program::Muse);
    assert_eq!(
        muse_edits.iter().map(|e| e.frame_row).collect::<Vec<_>>(),
        vec![7, 8, 9],
        "only Muse assistant reply rows (7, 8, 9) are styled"
    );
    assert_eq!(
        text_of(&muse_edits[0].cells).trim_end(),
        "\u{25c6} Muse Heading"
    );
    assert_eq!(
        text_of(&muse_edits[1].cells).trim_end(),
        "  Here is bold italic and bold code."
    );
    assert!(muse_edits[1].cells[10].bold && muse_edits[1].cells[10].italic);
    assert!(
        muse_edits[2]
            .cells
            .iter()
            .skip(2)
            .take(10)
            .all(|c| c.ch == '\u{2500}'),
        "horizontal rule rendered as box-drawing line"
    );

    scratch.rows = 3;
    scratch.cells = [8, 9, 11]
        .into_iter()
        .map(|r| original[r].clone())
        .collect();
    assert!(collect(&scratch).is_empty());

    scratch.rows = 1;
    scratch.cells = vec![row_of("  **text**", [0, 0, 0])];
    assert_eq!(collect(&scratch).len(), 1);
    scratch.cells = vec![row_of("  **你 **", [0, 0, 0])];
    scratch.cells[0][5].wide = true;
    assert!(collect(&scratch).is_empty());
    scratch.cells = vec![row_of("  **❤ **", [0, 0, 0])];
    scratch.cells[0][4].emoji_presentation = true;
    scratch.cells[0][5].wide = true;
    assert!(collect(&scratch).is_empty());
    scratch.cells = vec![row_of("  **😀**", [0, 0, 0])];
    scratch.cells[0][4].text_presentation = true;
    assert!(collect(&scratch).is_empty());
    scratch.cells = vec![row_of("  **גבא**", [0, 0, 0])];
    let mut map: Vec<u16> = (0..scratch.cols)
        .map(|c| u16::try_from(c).unwrap())
        .collect();
    map.swap(4, 6);
    scratch.visual_to_logical = vec![map];
    assert!(collect(&scratch).is_empty());
    scratch.visual_to_logical.clear();
    assert_eq!(collect(&scratch).len(), 1);
    scratch.cells = vec![row_of("  **text**", [0, 0, 0])];
    scratch.selection.start_selection(
        0,
        2,
        aterm_core::selection::SelectionSide::Left,
        aterm_core::selection::SelectionType::Simple,
    );
    scratch
        .selection
        .update_selection(0, 7, aterm_core::selection::SelectionSide::Right);
    scratch.selection.complete_selection();
    assert!(scratch.any_selection());
    assert!(collect(&scratch).is_empty());

    let scrolled_rows = [
        "  let **literal_in_scrolled_code** = 1;",
        "  ```",
        "  ## Scrolled Heading",
        "  - Scrolled **bold** and `code` item",
    ];
    let mut scrolled = RenderInput::empty();
    scrolled.rows = scrolled_rows.len();
    scrolled.cols = 100;
    scrolled.cursor_visible = false;
    scrolled.cells = scrolled_rows.iter().map(|r| row_of(r, [0, 0, 0])).collect();
    let scrolled_text: Vec<String> = scrolled_rows.iter().map(|r| (*r).to_owned()).collect();
    let mut scrolled_edits = Vec::new();
    collect_agy_transcript_edits(
        &scrolled,
        aterm_phase::Program::Agy,
        &scrolled_text,
        PaneTextRegion {
            strip: 0,
            row_off: 0,
            col_off: 0,
            rows: scrolled.rows,
            cols: scrolled.cols,
        },
        None,
        RenderCell::default(),
        RenderCell {
            fg: [80, 80, 80],
            ..RenderCell::default()
        },
        [180, 100, 80],
        &mut scrolled_edits,
    );
    assert_eq!(
        scrolled_edits
            .iter()
            .map(|e| e.frame_row)
            .collect::<Vec<_>>(),
        vec![2, 3],
        "scrolled-off code block stays literal (row 0), markdown below closing fence renders (rows 2, 3)"
    );
    assert_eq!(
        text_of(&scrolled_edits[0].cells).trim_end(),
        "  Scrolled Heading"
    );
    assert_eq!(
        text_of(&scrolled_edits[1].cells).trim_end(),
        "  \u{2022} Scrolled bold and code item"
    );

    let multi_block_rows = [
        "  let **literal_block_1** = 1;",
        "  ```",
        "  ## Between Blocks",
        "  - **Important** point",
        "  ```",
        "  let **literal_block_2** = 2;",
        "  ```",
        "  - **Final** summary",
    ];
    scrolled.rows = multi_block_rows.len();
    scrolled.cells = multi_block_rows
        .iter()
        .map(|r| row_of(r, [0, 0, 0]))
        .collect();
    let multi_block_text: Vec<String> = multi_block_rows.iter().map(|r| (*r).to_owned()).collect();
    scrolled_edits.clear();
    collect_agy_transcript_edits(
        &scrolled,
        aterm_phase::Program::Agy,
        &multi_block_text,
        PaneTextRegion {
            strip: 0,
            row_off: 0,
            col_off: 0,
            rows: scrolled.rows,
            cols: scrolled.cols,
        },
        None,
        RenderCell::default(),
        RenderCell {
            fg: [80, 80, 80],
            ..RenderCell::default()
        },
        [180, 100, 80],
        &mut scrolled_edits,
    );
    assert_eq!(
        scrolled_edits
            .iter()
            .map(|e| e.frame_row)
            .collect::<Vec<_>>(),
        vec![2, 3, 7],
        "only markdown rows between and after the two code blocks (2, 3, 7) are styled"
    );

    let bottom_open_rows = [
        "  ## Overview Above Fence",
        "  - **First** item",
        "  ```",
        "  # shell comment in unclosed code block",
        "  echo **literal**",
    ];
    scrolled.rows = bottom_open_rows.len();
    scrolled.cells = bottom_open_rows
        .iter()
        .map(|r| row_of(r, [0, 0, 0]))
        .collect();
    let bottom_open_text: Vec<String> = bottom_open_rows.iter().map(|r| (*r).to_owned()).collect();
    scrolled_edits.clear();
    collect_agy_transcript_edits(
        &scrolled,
        aterm_phase::Program::Agy,
        &bottom_open_text,
        PaneTextRegion {
            strip: 0,
            row_off: 0,
            col_off: 0,
            rows: scrolled.rows,
            cols: scrolled.cols,
        },
        None,
        RenderCell::default(),
        RenderCell {
            fg: [80, 80, 80],
            ..RenderCell::default()
        },
        [180, 100, 80],
        &mut scrolled_edits,
    );
    assert_eq!(
        scrolled_edits
            .iter()
            .map(|e| e.frame_row)
            .collect::<Vec<_>>(),
        vec![0, 1],
        "markdown above unclosed bottom fence renders (0, 1); code below fence stays literal"
    );

    let rule_80 = "\u{2500}".repeat(80);
    let rule_60 = "\u{2500}".repeat(60);
    let scrolled_rule_rows = [
        "  ## Above Earlier Rule",
        rule_80.as_str(),
        "  ## Below Earlier Rule",
        "  - **Rich** item after rule",
        rule_60.as_str(),
    ];
    scrolled.cols = 80;
    scrolled.rows = scrolled_rule_rows.len();
    scrolled.cells = scrolled_rule_rows
        .iter()
        .map(|r| row_of(r, [0, 0, 0]))
        .collect();
    let scrolled_rule_text: Vec<String> =
        scrolled_rule_rows.iter().map(|r| (*r).to_owned()).collect();
    scrolled_edits.clear();
    collect_agy_transcript_edits(
        &scrolled,
        aterm_phase::Program::Agy,
        &scrolled_rule_text,
        PaneTextRegion {
            strip: 0,
            row_off: 0,
            col_off: 0,
            rows: scrolled.rows,
            cols: scrolled.cols,
        },
        None,
        RenderCell::default(),
        RenderCell {
            fg: [80, 80, 80],
            ..RenderCell::default()
        },
        [180, 100, 80],
        &mut scrolled_edits,
    );
    assert_eq!(
        scrolled_edits
            .iter()
            .map(|e| e.frame_row)
            .collect::<Vec<_>>(),
        vec![0, 2, 3],
        "markdown rows both above (0) and below (2, 3) a rule in a scrolled viewport render"
    );

    let mut split_scratch = RenderInput::empty();
    split_scratch.cols = 120;
    split_scratch.rows = 1;
    let mut full_row = vec![RenderCell::default(); 40];
    full_row.extend(row_of("  ## Split Heading", [0, 0, 0]));
    full_row.resize(120, RenderCell::default());
    split_scratch.cells = vec![full_row];
    split_scratch.cursor_visible = true;
    split_scratch.cursor_row = 0;
    split_scratch.cursor_col = 5;
    let split_rows = vec!["  ## Split Heading".to_owned()];
    scrolled_edits.clear();
    collect_agy_transcript_edits(
        &split_scratch,
        aterm_phase::Program::Agy,
        &split_rows,
        PaneTextRegion {
            strip: 0,
            row_off: 0,
            col_off: 40,
            rows: 1,
            cols: 80,
        },
        None,
        RenderCell::default(),
        RenderCell {
            fg: [80, 80, 80],
            ..RenderCell::default()
        },
        [180, 100, 80],
        &mut scrolled_edits,
    );
    assert_eq!(
        scrolled_edits.len(),
        1,
        "adjacent pane cursor outside col_off..col_off+cols must not skip agy markdown row"
    );
    split_scratch.cursor_col = 45;
    scrolled_edits.clear();
    collect_agy_transcript_edits(
        &split_scratch,
        aterm_phase::Program::Agy,
        &split_rows,
        PaneTextRegion {
            strip: 0,
            row_off: 0,
            col_off: 40,
            rows: 1,
            cols: 80,
        },
        None,
        RenderCell::default(),
        RenderCell {
            fg: [80, 80, 80],
            ..RenderCell::default()
        },
        [180, 100, 80],
        &mut scrolled_edits,
    );
    assert!(
        scrolled_edits.is_empty(),
        "cursor inside the agy pane's own columns skips the active cursor row"
    );
}

#[test]
fn agy_subagent_display_rainbow_blings_transcript_panel_and_footer_rows() {
    let rule_80 = "\u{2500}".repeat(80);
    let screen_rows = [
        "● Spawning 2 subagent(s)... (ctrl+o to expand) \u{00B7} 3s",
        "  └ Agent(self: Codebase Researcher): Reading file... \u{00B7} 4m36s",
        "● Spawned 1 subagent(s)",
        "● Agent(self: Verifier) needs approval for RunCommand",
        rule_80.as_str(),
        "> ",
        rule_80.as_str(),
        "  ● Agent(self: Codebase (tests) Researcher)     Reading file... \u{00B7} 4m36s",
        "  ● Agent(research: Docs (API) Finder)         Searching web... \u{00B7} 8s",
        "  ● [17:20:01] sleep 30                  running",
        rule_80.as_str(),
        "  ? for shortcuts                        Gemini 4 Argon \u{00B7} high \u{00B7} 1 task(s) \u{00B7} 2 subagent(s)",
    ];
    let rows: Vec<String> = screen_rows.iter().map(|r| format!("{r:<100}")).collect();
    let blank = RenderCell {
        ch: ' ',
        fg: [0xd8, 0xdc, 0xe4],
        bg: [0x14, 0x16, 0x1b],
        ..RenderCell::default()
    };
    let mut scratch = RenderInput::empty();
    scratch.rows = rows.len();
    scratch.cols = 100;
    scratch.cursor_visible = false;
    scratch.cells = rows
        .iter()
        .map(|r| {
            r.chars()
                .map(|ch| RenderCell { ch, ..blank })
                .collect::<Vec<_>>()
        })
        .collect();
    // Simulate agy coloring the panel's `●` and `Agent(...)` in ANSI blue/green.
    scratch.cells[7][2].fg = [0x40, 0xc0, 0x60];
    for c in
        &mut scratch.cells[7][4..4 + "Agent(self: Codebase (tests) Researcher)".chars().count()]
    {
        c.fg = [0x5c, 0x9c, 0xff];
    }
    let original = scratch.cells.clone();

    // 1. `claude_panel::read_panel` reads agy's open subagent panel rows even when ANSI-tinted,
    // and `PanelRow.name` spans columns `4..4 + unwrapped.chars().count()`.
    let pane_geom = crate::claude_card::PaneGeom {
        strip: 0,
        row_off: 0,
        col_off: 0,
        rows: scratch.rows,
        cols: scratch.cols,
    };
    let panel_read = crate::claude_panel::read_panel(1, &rows, 6, &scratch, pane_geom, blank)
        .expect("agy open subagent panel should be read by claude_panel");
    assert_eq!(panel_read.panel.rows().len(), 2);
    assert_eq!(
        panel_read.panel.rows()[0].name.range(),
        4..4 + "Codebase (tests) Researcher".len()
    );
    assert_eq!(
        panel_read.panel.rows()[1].name.range(),
        4..4 + "research: Docs (API) Finder".len()
    );
    assert!(
        panel_read.ink[0].bullet.is_some() && panel_read.ink[0].name.is_some(),
        "ANSI-colored agy subagent bullet and name are accepted by claude_panel::read_panel"
    );

    // 2. `collect_agy_transcript_edits` rewrites transcript subagent rows, panel rows, and the
    // footer row beneath the open panel.
    let mut edits = Vec::new();
    collect_agy_transcript_edits(
        &scratch,
        aterm_phase::Program::Agy,
        &rows,
        PaneTextRegion {
            strip: 0,
            row_off: 0,
            col_off: 0,
            rows: scratch.rows,
            cols: scratch.cols,
        },
        Some(4),
        blank,
        RenderCell {
            ch: '\u{2500}',
            fg: [0x58, 0x5c, 0x68],
            ..blank
        },
        [0xe0, 0x7a, 0x5f],
        &mut edits,
    );
    let mut undo_slot = FooterUndo::default();
    write_edits(
        &mut undo_slot,
        &mut scratch,
        edits
            .iter()
            .map(|e| (e.frame_row, e.col, e.cells.as_slice(), e.ground)),
    );
    let row_str = |r: usize| -> String {
        scratch.cells[r]
            .iter()
            .map(|c| c.ch)
            .collect::<String>()
            .trim_end()
            .to_owned()
    };
    assert_eq!(
        row_str(0),
        "● Spawning 2 subagents (ctrl+o to expand) \u{00B7} 3s"
    );
    assert_eq!(
        row_str(1),
        "  └ Codebase Researcher  Reading file \u{00B7} 4m36s"
    );
    assert_eq!(row_str(2), "● Spawned 1 subagent");
    assert_eq!(row_str(3), "● Verifier needs approval for RunCommand");
    assert_eq!(
        row_str(7),
        "  ● Codebase (tests) Researcher  Reading file \u{00B7} 4m36s"
    );
    assert_eq!(
        row_str(8),
        "  ● research: Docs (API) Finder  Searching web \u{00B7} 8s"
    );
    assert_eq!(row_str(9), "  ● [17:20:01] sleep 30  running");
    let footer_text = row_str(11);
    assert!(
        footer_text.starts_with("  yolo \u{00B7} ? for shortcuts"),
        "open-panel footer keeps yolo prefix: {footer_text:?}"
    );
    assert!(
        !footer_text.contains("Gemini 4 Argon"),
        "redundant model/effort stripped from footer: {footer_text:?}"
    );
    assert!(
        footer_text.ends_with("1 task \u{00B7} ● 2 subagents"),
        "footer right-aligns humanized task and rainbow subagent badge: {footer_text:?}"
    );

    // Verify rainbow spectrum ink (bold, distinct hues, >= 4.5:1 contrast) on subagent names.
    let name_range = panel_read.panel.rows()[0].name.range();
    let name_cells = &scratch.cells[7][name_range.clone()];
    let mut distinct_fgs = std::collections::HashSet::new();
    for c in name_cells.iter().filter(|c| c.ch != ' ') {
        assert!(c.bold, "subagent name cell {:?} should be bold", c.ch);
        assert!(
            reads_on(c.fg, blank.bg) >= 4.5,
            "subagent rainbow ink {:?} must meet 4.5:1 contrast on {:?}",
            c.fg,
            blank.bg
        );
        distinct_fgs.insert(c.fg);
    }
    assert!(
        distinct_fgs.len() >= 4,
        "subagent name should span multiple rainbow spectrum stops, got {distinct_fgs:?}"
    );

    // Verify `claude_panel` derives in-flight tool states (`DotState::Tool`) for agy subagents
    // past `QUIET_MS` and emits dot `InkCell` + `RainHalo` without overwriting the rainbow
    // name cells in `scratch.cells`.
    assert!(panel_read.agy_agent[0] && panel_read.agy_agent[1]);
    let mut track = aterm_agent::harness::agents::PanelTrack::default();
    track.fold_flown(
        &panel_read.panel,
        &[aterm_agent::harness::agents::Tint::Plain; aterm_phase::agents_panel::PANEL_ROWS_MAX],
        &panel_read.agy_flights,
        0,
    );
    let mut rows_25s = rows.clone();
    rows_25s[7] =
        "  ● Agent(self: Codebase (tests) Researcher)  Reading file \u{00B7} 5m01s".to_owned();
    rows_25s[8] = "  ● Agent(research: Docs (API) Finder)  Working \u{00B7} 33s".to_owned();
    let panel_read_25s =
        crate::claude_panel::read_panel(1, &rows_25s, 6, &scratch, pane_geom, blank)
            .expect("agy panel after 25s");
    assert!(matches!(
        panel_read.agy_flights[1],
        aterm_agent::harness::flight::Flight::Tool { .. }
    ));
    assert_eq!(
        panel_read_25s.agy_flights[1],
        aterm_agent::harness::flight::Flight::Model,
        "parentheses inside the accepted name do not become part of the model activity"
    );
    track.fold_flown(
        &panel_read_25s.panel,
        &[aterm_agent::harness::agents::Tint::Plain; aterm_phase::agents_panel::PANEL_ROWS_MAX],
        &panel_read_25s.agy_flights,
        25_000,
    );
    assert_eq!(
        track.dots()[0].state,
        aterm_agent::harness::agents::DotState::Tool,
        "agy tool subagent stays in DotState::Tool (sat=1.0) past QUIET_MS instead of fading to Quiet"
    );
    assert_eq!(
        track.dots()[1].state,
        aterm_agent::harness::agents::DotState::Running
    );
    assert_eq!(track.dots()[0].sat, 1.0);
    assert_eq!(
        track.dots()[0].waits.as_ref().map(|w| w.tool.as_str()),
        Some("Reading file")
    );
    for palette in [
        aterm_effects::harness_card::CardPalette::Cruise,
        aterm_effects::harness_card::CardPalette::Land,
        aterm_effects::harness_card::CardPalette::Off,
    ] {
        let (ink, halo) =
            crate::claude_panel::look_for_test(&panel_read_25s, &track, palette, pane_geom, blank);
        assert!(
            ink.iter().any(|c| c.row == 7 && c.col == 2),
            "bullet dot at col 2 is inked under {palette:?}"
        );
        assert!(
            !ink.iter()
                .any(|c| c.row == 7 && name_range.contains(&usize::from(c.col))),
            "make_look must not emit InkCells over agy rainbow name columns under {palette:?}: {ink:?}"
        );
        assert!(
            halo.iter().any(|h| h.row == 7),
            "subagent bullet dot emits halo light quads under {palette:?}"
        );
    }
    let inks = MarkdownInks::of(
        blank,
        RenderCell {
            ch: '\u{2500}',
            fg: [0x58, 0x5c, 0x68],
            ..blank
        },
        [0xe0, 0x7a, 0x5f],
    );
    let nested_panel = render_agy_panel_item_row(
        "  ● Agent(self: Researcher (GUI))  Reading file (foo.rs) \u{00B7} 25s",
        0,
        scratch.cols,
        scratch.cols,
        blank,
        &inks,
    )
    .expect("parenthesized role in agy panel row should render");
    let nested_panel_text: String = nested_panel.iter().map(|c| c.ch).collect();
    assert!(
        nested_panel_text.contains("Researcher (GUI)  Reading file (foo.rs) \u{00B7} 25s"),
        "parenthesized role unwrapped cleanly in panel row: {nested_panel_text:?}"
    );
    let nested_tx = render_agy_subagent_transcript_row(
        "  └ Agent(self: Researcher (GUI)): Reading file (foo.rs)... \u{00B7} 25s",
        0,
        scratch.cols,
        scratch.cols,
        blank,
        &inks,
    )
    .expect("parenthesized role in agy transcript row should render");
    let nested_tx_text: String = nested_tx.iter().map(|c| c.ch).collect();
    assert!(
        nested_tx_text.contains("Researcher (GUI)  Reading file (foo.rs) \u{00B7} 25s"),
        "parenthesized role unwrapped cleanly in transcript row: {nested_tx_text:?}"
    );

    // Verify `undo` restores all original cells cleanly.
    assert!(undo(&mut undo_slot, &mut scratch));
    assert_eq!(scratch.cells, original);
}

#[test]
fn agy_usage_card_weekly_places_and_expires_iso_reset_clock() {
    let rule = "\u{2500}".repeat(80);
    let rows: Vec<String> = vec![
        "● /usage".into(),
        "  └ Gemini Models".into(),
        "    Weekly Limit Remaining                 ████████░░  78%    2026-10-29 09:31 PDT".into(),
        "  Claude and GPT models".into(),
        "    Weekly Limit Remaining                 ██████████  100%   2026-10-31 00:51 PDT".into(),
        rule.clone(),
        "> ".into(),
        rule,
        "  ? for shortcuts                                        Gemini 3.1 Pro \u{00B7} high"
            .into(),
    ];
    let reading = aterm_phase::agy::usage_card_weekly(&rows).expect("agy /usage weekly");
    assert_eq!(reading.text, "weekly 78% left");
    assert_eq!(reading.reset.as_deref(), Some("2026-10-29 09:31 PDT"));

    let mut scratch = RenderInput::empty();
    scratch.rows = rows.len();
    scratch.cols = rows.iter().map(String::len).max().expect("rows");
    scratch.cells = rows.iter().map(|row| row_of(row, [90, 90, 90])).collect();
    scratch.snapshot_seq = 1;
    let region = PaneTextRegion {
        strip: 0,
        row_off: 0,
        col_off: 0,
        rows: scratch.rows,
        cols: scratch.cols,
    };
    let mut cache = PaneTextCache::default();
    let original = cache.rows(0, 7, &scratch, region);
    for (model, words, reset) in [
        (
            Some("Gemini 3.1 Pro"),
            "weekly 78% left",
            "2026-10-29 09:31 PDT",
        ),
        (
            Some("Claude Opus 4.6"),
            "weekly 100% left",
            "2026-10-31 00:51 PDT",
        ),
        (
            Some("Gemini 3.1 Pro"),
            "weekly 78% left",
            "2026-10-29 09:31 PDT",
        ),
        (
            Some("Claude Opus 4.6"),
            "weekly 100% left",
            "2026-10-31 00:51 PDT",
        ),
        (None, "weekly 78% left", "2026-10-29 09:31 PDT"),
    ] {
        cache.note_facts(
            0,
            7,
            &FooterFacts {
                model: model.map(str::to_owned),
                ..FooterFacts::default()
            },
            None,
            true,
            FactMask::default(),
        );
        assert_eq!(
            cache.usage_weekly(0, Some(aterm_phase::Program::Agy)),
            Some((words, Some(reset))),
            "unchanged quota rows follow the current model: {model:?}"
        );
        assert!(
            Arc::ptr_eq(&original, &cache.rows(0, 7, &scratch, region)),
            "a model refresh does not rebuild the unchanged screen rows"
        );
    }

    fn pdt_offset(zone: Option<&str>, _unix: i64) -> Option<i64> {
        match zone {
            None | Some("America/Los_Angeles") => Some(-7 * 3600),
            Some("UTC") => Some(0),
            _ => None,
        }
    }
    // 2026-10-29 09:31 PDT = 2026-10-29 16:31:00 UTC = 1_793_291_460.
    let reset_unix = 1_793_291_460;
    let before = || footer::ReadClock {
        read_at: std::time::UNIX_EPOCH,
        now: reset_unix - 3600,
        offset_at: &pdt_offset,
    };
    let (weekly, reset) =
        muse_weekly_observation((&reading.text, reading.reset.as_deref()), None, &before);
    assert_eq!(weekly, Some("weekly 78% left"));
    assert_eq!(reset, Some(("2026-10-29 09:31 PDT", Some(reset_unix))));

    let after = || footer::ReadClock {
        read_at: std::time::UNIX_EPOCH,
        now: reset_unix + 60,
        offset_at: &pdt_offset,
    };
    let (expired_weekly, expired_reset) =
        muse_weekly_observation((&reading.text, reading.reset.as_deref()), None, &after);
    assert_eq!(expired_weekly, None);
    assert_eq!(
        expired_reset,
        Some(("2026-10-29 09:31 PDT", Some(reset_unix)))
    );
}

#[test]
fn agy_resolver_uses_process_presence_and_keeps_task_fallback() {
    const PID: i32 = 4299;
    thread_local! {
        static HOME_DIR: std::cell::RefCell<PathBuf> = const { std::cell::RefCell::new(PathBuf::new()) };
        static PROC_ENV: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
        static FILE_READS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
        static PROC_FILES: std::cell::RefCell<Vec<PathBuf>> = const { std::cell::RefCell::new(Vec::new()) };
    }
    fn test_home() -> Option<PathBuf> {
        Some(HOME_DIR.with(|h| h.borrow().clone()))
    }
    fn test_files(pid: u32) -> Option<Vec<PathBuf>> {
        assert_eq!(pid, u32::try_from(PID).unwrap());
        FILE_READS.with(|reads| {
            assert_eq!(
                reads.replace(1),
                0,
                "resume and excerpts share one observation"
            );
        });
        Some(PROC_FILES.with(|files| files.borrow().clone()))
    }
    fn test_args(_: i32) -> Option<atpkg::caller_shell::ProcArgs> {
        Some(atpkg::caller_shell::ProcArgs {
            argv: vec!["agy".to_owned()],
            env: PROC_ENV.with(|e| e.borrow().clone()),
            ..Default::default()
        })
    }

    let dir = aterm_tempfile::Builder::new()
        .prefix("aterm-agy-footer-")
        .tempdir()
        .expect("private agy home");
    let home = dir.path().join("home");
    let cfg = home.join(".gemini/antigravity-cli");
    std::fs::create_dir_all(&cfg).unwrap();
    std::fs::write(
        cfg.join("settings.json"),
        br#"{"model":"Gemini 4 Argon (High)"}"#,
    )
    .unwrap();
    std::fs::write(
        cfg.join("cli.log"),
        b"I1023 17:50:20.000000 1 server_oauth.go:224] applyAuthResult: email=dev@example.com, authMethod=consumer\n",
    )
    .unwrap();
    HOME_DIR.with(|h| *h.borrow_mut() = home.clone());

    let run_with_env = |extra_env: Option<&str>| {
        let mut env = vec![
            format!("HOME={}", home.display()),
            format!("AGY_CLI_CONFIG_DIR={}", cfg.display()),
        ];
        if let Some(extra) = extra_env {
            env.push(extra.to_owned());
        }
        PROC_ENV.with(|e| *e.borrow_mut() = env);
        FILE_READS.with(|reads| reads.set(0));

        let timeline = Arc::new(Mutex::new(SessionTimeline::default()));
        {
            let mut held = timeline.lock().unwrap();
            held.note_foreground_group(PID);
            held.set_program(PID, Some("agy".to_owned()));
        }
        let mut identity = unread(PID, &home.join("no-claude"));
        identity.dir = None;
        resolve_facts_at_with_cwd(
            &timeline,
            &mut identity,
            &mut footer::TailCache::default(),
            &mut footer::FooterCache::default(),
            &mut RosterMemo::default(),
            (
                Asks {
                    files: test_files,
                    tap: NO_TAP,
                    args: test_args,
                    roster: || None,
                    home: test_home,
                    route: |_, _, _| accounts::AuthRoute::Outside,
                },
                |_| None,
            ),
            &footer::ReadClock {
                read_at: std::time::UNIX_EPOCH,
                now: 1_792_000_000,
                offset_at: &|_, _| Some(0),
            },
        );
        let held = timeline.lock().unwrap();
        (held.claude_footer_shown(), held.claude_excerpts().cloned())
    };

    let (shown, excerpts) = run_with_env(None);
    let shown = shown.expect("agy footer facts");
    assert_eq!(shown.model.as_deref(), Some("Gemini 4 Argon"));
    assert_eq!(shown.effort.as_deref(), Some("high"));
    assert_eq!(
        shown.subscription.as_ref().and_then(|s| s.email.as_deref()),
        Some("dev@example.com")
    );
    assert!(
        excerpts.is_none(),
        "interactive agy session without transcript or task leaves excerpts None so survey falls back to screen rows"
    );

    let tasks_dir = crate::task_record::tasks_dir().expect("tasks_dir");
    std::fs::create_dir_all(&tasks_dir).unwrap();
    let task_file = tasks_dir.join(format!("test-agy-footer-{}.task", std::process::id()));
    std::fs::write(&task_file, b"fix the footer\n").unwrap();
    let task_env = format!("{}={}", crate::survey::TASK_FILE_ENV, task_file.display());
    let (_, tasked_excerpts) = run_with_env(Some(&task_env));
    let _ = std::fs::remove_file(&task_file);
    assert_eq!(
        tasked_excerpts.and_then(|e| e.task),
        Some("fix the footer".to_owned())
    );

    let cid = "dfaaf1c5-98ff-4412-9226-76412af952a5";
    let logs_dir = cfg.join("brain").join(cid).join(".system_generated/logs");
    std::fs::create_dir_all(&logs_dir).unwrap();
    let transcript_bytes = br#"{"step_index":0,"source":"USER_EXPLICIT","type":"USER_INPUT","status":"DONE","content":"<USER_REQUEST>\npopulate agy survey\n</USER_REQUEST>"}
{"step_index":1,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","content":"Populated agy survey excerpts.","tool_calls":[{"name":"run_command","args":{}}]}
"#;
    std::fs::write(logs_dir.join("transcript.jsonl"), transcript_bytes).unwrap();
    let win_home = format!(r"C:\Users\dev\{}", home.to_string_lossy());
    std::fs::write(
        cfg.join("history.jsonl"),
        format!(
            "{}\n{}\n",
            aterm_json::json!({
                "display": "earlier windows workspace prompt",
                "timestamp": 0,
                "workspace": win_home,
                "conversationId": cid,
            }),
            aterm_json::json!({
                "display": "populate agy survey",
                "timestamp": 1,
                "workspace": home.to_string_lossy().as_ref(),
                "conversationId": cid,
            }),
        ),
    )
    .unwrap();
    let (unheld_shown, unheld_excerpts) = run_with_env(None);
    assert!(
        unheld_excerpts.is_none() && unheld_shown.unwrap().resume.is_none(),
        "a transcript on disk is not process ownership"
    );
    let lock = aterm_agent::harness::upgrade_agy::presence_lock(&cfg, cid);
    std::fs::create_dir_all(lock.parent().unwrap()).unwrap();
    std::fs::write(&lock, b"").unwrap();
    PROC_FILES.with(|files| {
        *files.borrow_mut() = vec![std::fs::canonicalize(&lock).unwrap()];
    });
    let (resumed_shown, transcript_excerpts) = run_with_env(None);
    let resumed_shown = resumed_shown.expect("agy footer facts with held transcript");
    assert!(
        resumed_shown
            .resume
            .as_deref()
            .is_some_and(|r| r.ends_with(&format!("agy --conversation={cid}")))
    );
    let transcript_excerpts = transcript_excerpts.expect("agy transcript excerpts");
    assert_eq!(
        transcript_excerpts.transcript_len,
        Some(transcript_bytes.len() as u64)
    );
    assert_eq!(
        transcript_excerpts.excerpts.prompts[0].text,
        "populate agy survey"
    );
    assert_eq!(
        transcript_excerpts.excerpts.prompts[1].text,
        "earlier windows workspace prompt"
    );
    assert_eq!(
        transcript_excerpts.excerpts.said.as_deref(),
        Some("Populated agy survey excerpts.")
    );
    assert_eq!(
        transcript_excerpts.excerpts.doing.as_deref(),
        Some("run_command")
    );

    let mut switch_tl = SessionTimeline::default();
    switch_tl.note_foreground_group(PID);
    switch_tl.set_program(PID, Some("agy".to_owned()));
    let mut first_facts = resumed_shown.clone();
    first_facts.subscription = Some(aterm_agent::harness::profile::SubscriptionDisplay {
        email: Some("dev@example.com".to_owned()),
        weekly: Some("weekly 78% left".to_owned()),
        weekly_figure: None,
        ..Default::default()
    });
    assert!(switch_tl.set_claude_footer(PID, None, Some(first_facts.clone())));
    assert!(!switch_tl.muse_cards_need_account());

    let mut switched_anon = first_facts.clone();
    switched_anon.subscription.as_mut().unwrap().email = Some("second@example.com".to_owned());
    assert!(switch_tl.set_claude_footer(PID, None, Some(switched_anon)));
    assert!(switch_tl.muse_cards_need_account());
    assert_eq!(
        switch_tl
            .claude_footer()
            .and_then(|f| f.subscription.as_ref()?.weekly.as_deref()),
        None,
        "anonymous screen weekly from the previous agy account is cleared on switch"
    );

    let fig = aterm_agent::harness::profile::parse_agy_usage_json(
        br#"{"status":"SUCCESS","command":{"name":"usage","data":{"groups":[{"name":"Gemini Models","buckets":[{"id":"gemini-weekly","name":"Weekly Limit Remaining","window":"weekly","remaining_fraction":0.91,"reset_time":"2026-10-29T16:31:22Z"}]}]}}}"#,
        false,
        1_792_000_000,
    )
    .expect("live figure");
    let mut switched_live = first_facts;
    switched_live.subscription = Some(aterm_agent::harness::profile::SubscriptionDisplay {
        email: Some("second@example.com".to_owned()),
        weekly: fig.words(1_792_000_000),
        weekly_figure: Some(fig),
        ..Default::default()
    });
    assert!(switch_tl.set_claude_footer(PID, None, Some(switched_live)));
    assert_eq!(
        switch_tl
            .claude_footer()
            .and_then(|f| f.subscription.as_ref()?.weekly.as_deref()),
        Some("weekly 91% left"),
        "live WeeklyFigure for the new agy account is preserved after switch"
    );
    PROC_FILES.with(|files| files.borrow_mut().clear());
    let (released_shown, released_excerpts) = run_with_env(None);
    assert!(
        released_excerpts.is_none() && released_shown.unwrap().resume.is_none(),
        "released presence is not cached ownership"
    );
}

#[test]
fn muse_resolver_populates_model_effort_resume_and_excerpts_from_session_log() {
    const PID: i32 = 4399;
    thread_local! {
        static HOME_DIR: std::cell::RefCell<PathBuf> = const { std::cell::RefCell::new(PathBuf::new()) };
        static PROC_FILES: std::cell::RefCell<Vec<PathBuf>> = const { std::cell::RefCell::new(Vec::new()) };
        static PROC_ENV: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
    }
    fn test_files(_: u32) -> Option<Vec<PathBuf>> {
        Some(PROC_FILES.with(|f| f.borrow().clone()))
    }
    fn test_home() -> Option<PathBuf> {
        Some(HOME_DIR.with(|h| h.borrow().clone()))
    }
    fn test_args(_: i32) -> Option<atpkg::caller_shell::ProcArgs> {
        Some(atpkg::caller_shell::ProcArgs {
            argv: vec!["muse".to_owned()],
            env: PROC_ENV.with(|e| e.borrow().clone()),
            ..Default::default()
        })
    }

    let scratch = aterm_tempfile::TempDir::new().unwrap();
    let (root, _) = super::rig::claude_home_at(
        scratch.path().join("muse-footer-resolver"),
        PID,
        1_790_607_848,
    );
    let home = root.join("home");
    let cfg = home.join(".config/muse");
    std::fs::create_dir_all(&cfg).unwrap();
    std::fs::write(
        cfg.join("settings.json"),
        br#"{"model":"Muse Spark (High)"}"#,
    )
    .unwrap();
    std::fs::write(
        cfg.join("auth.json"),
        br#"{"providers":{"meta":{"mechanism":"oauth","user_email":"muse@example.com"}}}"#,
    )
    .unwrap();
    let sid = "01a16480-f908-7d10-a78e-5d95372dfb98";
    let sdir = home.join(".local/share/muse/sessions/2026/10/23").join(sid);
    std::fs::create_dir_all(&sdir).unwrap();
    std::fs::write(sdir.join(".session.lock"), format!("pid={PID}\nhost=mac\n")).unwrap();
    let session_log = br#"{"payload_type":"runtime.session","payload":{"kind":"run","run_id":"r1","event":{"kind":"started","prompt":"inspect muse footer"}}}
{"payload_type":"runtime.session","payload":{"kind":"run","run_id":"r1","event":{"kind":"assistant_message_committed","text":"Muse footer and survey excerpts ready."}}}
{"payload_type":"runtime.session","payload":{"kind":"task","run_id":"r1","task_id":"t1","event":{"kind":"proposed","task_kind":"tool.run_command"}}}
"#;
    std::fs::write(sdir.join("session.jsonl"), session_log).unwrap();

    HOME_DIR.with(|h| *h.borrow_mut() = home.clone());
    PROC_FILES.with(|f| {
        *f.borrow_mut() = vec![std::fs::canonicalize(sdir.join(".session.lock")).unwrap()]
    });
    PROC_ENV.with(|e| *e.borrow_mut() = vec![format!("HOME={}", home.display())]);

    let timeline = Arc::new(Mutex::new(SessionTimeline::default()));
    {
        let mut held = timeline.lock().unwrap();
        held.note_foreground_group(PID);
        held.set_program(PID, Some("muse".to_owned()));
    }
    // Even if identity.dir points to a directory with a synthetic `<PID>.json` for Claude,
    // `uses_claude_registry` is false for `Program::Muse` so it reads Muse's own facts.
    let mut identity = unread(PID, &root.join("claude"));
    resolve_facts_at_with_cwd(
        &timeline,
        &mut identity,
        &mut footer::TailCache::default(),
        &mut footer::FooterCache::default(),
        &mut RosterMemo::default(),
        (
            Asks {
                files: test_files,
                tap: NO_TAP,
                args: test_args,
                // Claude Code's roster, which names no Muse account.
                roster: || Some(two_accounts()),
                home: test_home,
                route: |_, _, _| accounts::AuthRoute::Outside,
            },
            |_| Some(home.clone()),
        ),
        &footer::ReadClock {
            read_at: std::time::UNIX_EPOCH,
            now: 1_792_000_000,
            offset_at: &|_, _| Some(0),
        },
    );
    let held = timeline.lock().unwrap();
    let shown = held.claude_footer_shown().expect("muse footer facts");
    assert_eq!(shown.model.as_deref(), Some("Muse Spark"));
    // NEGATIVE CONTROL: with the footer judging every pane's login against Claude Code's
    // roster, this Muse pane is named `main`, the Claude row with no directory.
    assert_eq!(
        shown.account, None,
        "Claude Code's roster names no Muse account"
    );
    assert_eq!(shown.effort.as_deref(), Some("high"));
    assert_eq!(
        shown.subscription.as_ref().and_then(|s| s.email.as_deref()),
        Some("muse@example.com")
    );
    assert_eq!(
        shown.resume.as_deref(),
        Some(format!("muse --contributor-mode off resume {sid}").as_str())
    );
    let excerpts = held.claude_excerpts().expect("muse excerpts");
    assert_eq!(excerpts.transcript_len, Some(session_log.len() as u64));
    assert_eq!(excerpts.excerpts.prompts[0].text, "inspect muse footer");
    assert_eq!(
        excerpts.excerpts.said.as_deref(),
        Some("Muse footer and survey excerpts ready.")
    );
    assert_eq!(excerpts.excerpts.doing.as_deref(), Some("run_command"));
    drop(held);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn glass_columns_maps_and_swallows_side_by_side_panes_on_the_same_row() {
    let rows = [
        "\u{2500}".repeat(60),
        "  \u{23F5}\u{23F5} bypass permissions on \u{B7} 14 shells".to_owned(),
    ];
    let mode = footer::mode_row_rewrite_under(&rows, 0).expect("mode row");
    let mut glass = GlassColumns::default();
    glass.push(GlassShift {
        term_row: 23,
        col_off: 0,
        mode: Some(mode),
        blank_to: 0,
    });
    glass.push(GlassShift {
        term_row: 23,
        col_off: 61,
        mode: Some(mode),
        blank_to: 0,
    });
    glass.push(GlassShift {
        term_row: 22,
        col_off: 0,
        mode: None,
        blank_to: 20,
    });
    glass.push(GlassShift {
        term_row: 22,
        col_off: 61,
        mode: None,
        blank_to: 20,
    });

    let kept_start = mode.kept()[0].start as u16;
    // Left pane (`col_off = 0`) maps its kept item and swallows its vacated tail.
    assert_eq!(glass.grid_col(23, 0), kept_start);
    assert!(!glass.swallows(23, 0));
    assert!(glass.swallows(23, mode.glass_end() as u16));
    // Unrewritten columns between panes (`mode.end..61`) are untouched.
    assert!(!glass.swallows(23, mode.end as u16));
    assert_eq!(glass.grid_col(23, mode.end as u16), mode.end as u16);

    // Right pane (`col_off = 61`) is not shadowed by the left pane's shift.
    assert_eq!(glass.grid_col(23, 61), 61 + kept_start);
    assert!(!glass.swallows(23, 61));
    assert!(glass.swallows(23, 61 + mode.glass_end() as u16));
    assert!(!glass.swallows(23, 61 + mode.end as u16));
    assert_eq!(
        glass.grid_col(23, 61 + mode.end as u16),
        61 + mode.end as u16
    );

    // Blanked effort rows (`mode = None`) swallow `< blank_to` in both panes and leave the rest.
    assert!(glass.swallows(22, 10));
    assert!(!glass.swallows(22, 20));
    assert_eq!(glass.grid_col(22, 20), 20);
    assert!(glass.swallows(22, 61 + 10));
    assert!(!glass.swallows(22, 61 + 20));
    assert_eq!(glass.grid_col(22, 61 + 20), 61 + 20);
}
