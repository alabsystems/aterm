// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn the_on_button_writes_exactly_the_switches_that_hold_it_off() {
    use aterm_phase::Program;
    let cfg = |text: &str| crate::app_config::HarnessPolicy::read(text).policy;
    let claude = Some(Program::Claude);
    let plan = |text: &str, headless: bool| switch_on_plan(&cfg(text), headless, claude);
    assert_eq!(
        plan("[harness]\n", false),
        SwitchOnPlan::default(),
        "control: nothing holds it off"
    );
    assert_eq!(
        plan("[harness]\nautopilot = \"off\"\n", false),
        SwitchOnPlan::default(),
        "Autopilot Off holds no switch off: the word is the chip's, never the switch's"
    );
    assert_eq!(
        plan("[harness]\nenabled = false\n", false).keys,
        [crate::prefs::EDIT_HARNESS_ENABLED]
    );
    assert_eq!(
        plan("[harness]\nheadless = false\n", false).keys,
        Vec::<&str>::new(),
        "the headless gate holds a windowed instance nothing"
    );
    assert_eq!(
        plan("[harness]\nheadless = false\n", true).keys,
        [HARNESS_HEADLESS_KEY]
    );
    assert_eq!(
        plan("[harness.claude]\nenabled = false\n", false).keys,
        [crate::prefs::EDIT_HARNESS_CLAUDE_ENABLED]
    );
    assert_eq!(
        plan("[harness.agy]\nenabled = false\n", false).keys,
        Vec::<&str>::new(),
        "another agent's switch is not Claude Code's"
    );
    assert_eq!(
        agent_switch_key(Program::Agy),
        Some(crate::prefs::EDIT_HARNESS_AGY_ENABLED)
    );
    assert_eq!(
        agent_switch_key(Program::Muse),
        Some(crate::prefs::EDIT_HARNESS_MUSE_ENABLED)
    );
}

#[test]
fn every_hosted_view_has_a_switch_and_no_unhosted_one_does() {
    for view in AutopilotView::ALL {
        let switch = HarnessSwitch::of(view);
        match view {
            AutopilotView::Cruise | AutopilotView::Land | AutopilotView::Off => {
                assert_eq!(
                    switch,
                    Some(HarnessSwitch::On),
                    "{view:?}: the harness is on"
                );
            }
            AutopilotView::MasterOff => {
                assert_eq!(switch, Some(HarnessSwitch::Off(SwitchOff::Master)));
            }
            AutopilotView::AgentOff(agent) => {
                assert_eq!(switch, Some(HarnessSwitch::Off(SwitchOff::Agent(agent))));
            }
            AutopilotView::Unhosted => assert_eq!(switch, None),
        }
        assert_eq!(
            switch.map(HarnessSwitch::is_on),
            switch.map(|_| view.mode().is_some()),
            "{view:?}: on wherever the harness runs a mode, Off included"
        );
        assert_eq!(
            switch.map(HarnessSwitch::is_off),
            switch.map(|_| view.mode().is_none()),
            "{view:?}"
        );
    }
    assert_eq!(HarnessSwitch::On.toggled(), Some(true));
    assert_eq!(HarnessSwitch::Off(SwitchOff::Master).toggled(), Some(false));
    assert_eq!(HarnessSwitch::Turning.toggled(), None);
    assert_eq!(HarnessSwitch::TurningOff.toggled(), None);
    assert!(HarnessSwitch::Turning.is_off() && !HarnessSwitch::Turning.is_on());
    assert!(
        !HarnessSwitch::TurningOff.is_off() && !HarnessSwitch::TurningOff.is_on(),
        "a switch turning off is on until the policy reads it back, and lit no more"
    );
    let said = HarnessSwitch::On.described("Claude Code");
    assert!(
        said.starts_with("The aterm harness is on (Supervise coding agents).")
            && said.contains("every session"),
        "{said:?}"
    );
}

#[test]
fn the_view_is_the_master_switch_then_the_word_then_the_default() {
    for word in [
        None,
        Some(Autopilot::Cruise),
        Some(Autopilot::Land),
        Some(Autopilot::Off),
    ] {
        assert_eq!(
            AutopilotView::of(HarnessPosture::Unhosted, word),
            AutopilotView::Unhosted
        );
        assert_eq!(
            AutopilotView::of(HarnessPosture::MasterOff, word),
            AutopilotView::MasterOff,
            "a session ON never lifts a global off"
        );
        for default in Autopilot::ALL {
            let cfg = SupervisorConfig {
                autopilot: default,
                ..SupervisorConfig::default()
            };
            assert_eq!(
                AutopilotView::of(HarnessPosture::On(default), word).mode(),
                Some(cfg.autopilot_for(word)),
                "{default:?} {word:?}"
            );
        }
    }
}

#[test]
fn the_posture_reads_the_host_the_switch_and_the_default() {
    let on = SupervisorConfig::default();
    assert_eq!(
        HarnessPosture::of(false, &on, false),
        HarnessPosture::Unhosted
    );
    assert_eq!(
        HarnessPosture::of(true, &on, false),
        HarnessPosture::On(Autopilot::Cruise)
    );
    let mut off = SupervisorConfig::default();
    off.set("enabled", "false").unwrap();
    assert_eq!(
        HarnessPosture::of(true, &off, false),
        HarnessPosture::MasterOff
    );
    let mut quiet = SupervisorConfig::default();
    quiet.set("headless", "false").unwrap();
    assert_eq!(
        HarnessPosture::of(true, &quiet, true),
        HarnessPosture::MasterOff
    );
    let mut land = SupervisorConfig::default();
    land.set("autopilot", "land").unwrap();
    assert_eq!(
        HarnessPosture::of(true, &land, false),
        HarnessPosture::On(Autopilot::Land)
    );
    assert_eq!(HarnessPosture::default(), HarnessPosture::Unhosted);
}

#[test]
fn the_chips_follow_the_designs_table_and_cycle_in_order() {
    let rows = [
        (
            AutopilotView::Cruise,
            "\u{221e}",
            ChipTone::Letter,
            Some(Autopilot::Land),
        ),
        (
            AutopilotView::Land,
            "\u{2913}",
            ChipTone::Letter,
            Some(Autopilot::Off),
        ),
        (
            AutopilotView::Off,
            "\u{25cb}",
            ChipTone::Quiet,
            Some(Autopilot::Cruise),
        ),
        (
            AutopilotView::MasterOff,
            "\u{2298} off",
            ChipTone::Quiet,
            None,
        ),
        (
            AutopilotView::Unhosted,
            "\u{2298} off",
            ChipTone::Quiet,
            None,
        ),
        (
            AutopilotView::AgentOff(aterm_phase::Program::Claude),
            "\u{2298} off",
            ChipTone::Quiet,
            None,
        ),
    ];
    for (view, label, tone, next) in rows {
        assert_eq!(view.label(), label, "{view:?}");
        assert_eq!(view.tone(), tone, "{view:?}");
        assert_eq!(view.next(), next, "{view:?}");
        assert!(view.title().contains("click: "), "{view:?}");
    }
    assert_eq!(
        AutopilotView::MasterOff.title(),
        "harness off in Settings \u{b7} click: turn the harness on"
    );
    let mut view = AutopilotView::Cruise;
    let mut seen = Vec::new();
    for _ in 0..Autopilot::ALL.len() {
        seen.push(view.mode().unwrap());
        view = AutopilotView::from_mode(view.next().unwrap());
    }
    assert_eq!(seen, Autopilot::ALL);
    assert_eq!(view, AutopilotView::Cruise);
    assert_eq!(PENDING_GLYPH, "\u{25cc}");
    assert_eq!(PENDING_GLYPH.chars().collect::<Vec<_>>(), [IN_FLIGHT]);
    assert!(AutopilotView::Cruise.carries() && AutopilotView::Land.carries());
    assert!(!AutopilotView::Off.carries() && !AutopilotView::MasterOff.carries());
    assert_eq!(AutopilotView::ALL.len(), 6);
    for view in AutopilotView::ALL {
        for supervised in [false, true] {
            let glass = match view {
                AutopilotView::Cruise if supervised => Some(CardPalette::Cruise),
                AutopilotView::Land if supervised => Some(CardPalette::Land),
                AutopilotView::Off => Some(CardPalette::Off),
                _ => None,
            };
            assert_eq!(view.wears_glass(supervised), glass, "{view:?} {supervised}");
            for (word, unhosted_on) in [
                None,
                Some(Autopilot::Cruise),
                Some(Autopilot::Land),
                Some(Autopilot::Off),
            ]
            .into_iter()
            .flat_map(|word| [(word, true), (word, false)])
            {
                let theme = match (view, word) {
                    (AutopilotView::Unhosted, _) if !unhosted_on => None,
                    (AutopilotView::Unhosted, Some(Autopilot::Off)) => None,
                    (AutopilotView::Unhosted, Some(Autopilot::Land)) => Some(CardPalette::Land),
                    (AutopilotView::Unhosted, _) => Some(CardPalette::Cruise),
                    _ => glass,
                };
                assert_eq!(
                    harness_theme(view, supervised, word, unhosted_on),
                    theme,
                    "{view:?} {supervised} {word:?} {unhosted_on}"
                );
            }
        }
    }
    assert_eq!(AutopilotView::Cruise.wears_glass(false), None, "CONTROL");
}

#[test]
fn an_agents_own_switch_off_reads_agent_off_and_the_master_wins() {
    use crate::harness_screen::AbArm;
    use aterm_phase::Program;
    for agent in Program::ALL.into_iter().filter(|p| p.supervisable()) {
        let mut cfg = SupervisorConfig::default();
        cfg.set(&format!("{}.enabled", agent.name()), "false")
            .unwrap();
        let posture = HarnessPosture::of(true, &cfg, false);
        let joined = posture.for_agent(Some(agent));
        assert_eq!(joined, HarnessPosture::AgentOff(agent), "{agent:?}");
        assert_eq!(joined.default_mode(), None, "{agent:?}");
        for word in [None, Some(Autopilot::Cruise), Some(Autopilot::Land)] {
            let view = AutopilotView::of(joined, word);
            assert_eq!(view, AutopilotView::AgentOff(agent), "{agent:?} {word:?}");
            assert_eq!(view.mode(), None);
            assert_eq!(
                view.next(),
                None,
                "a click turns the switch on, writing no word"
            );
            assert!(!view.carries());
            assert_eq!(view.wire(), "off");
            assert_eq!(view.label(), "\u{2298} off");
            assert_eq!(view.tone(), ChipTone::Quiet);
            assert_eq!(
                wire_field(posture, word, Some(agent)),
                "off",
                "{agent:?} {word:?}"
            );
        }
        let switch = format!("Supervise {} sessions is off", agent.product());
        assert_eq!(agent_switch_off(agent), switch);
        assert_eq!(
            switch,
            format!(
                "{} is off",
                crate::prefs::agent_switch_label(agent.product())
            ),
            "{agent:?}: the switch's Settings label"
        );
        assert_eq!(
            AutopilotView::AgentOff(agent).title(),
            format!("{switch} \u{b7} click: turn the harness on")
        );
        assert_eq!(
            AbArm::of(AutopilotView::AgentOff(agent), false, false),
            AbArm::Off,
            "{agent:?}: plain, the baseline"
        );
        assert_eq!(
            AbArm::of(AutopilotView::AgentOff(agent), false, true),
            AbArm::Unsupervised,
            "{agent:?}: a claim still live"
        );
        for other in Program::ALL
            .into_iter()
            .filter(|p| p.supervisable() && *p != agent)
        {
            assert_eq!(
                posture.for_agent(Some(other)),
                HarnessPosture::On(Autopilot::Cruise),
                "{agent:?} off, {other:?} on"
            );
        }
        assert_eq!(posture.for_agent(None), HarnessPosture::MasterOff);
        cfg.set("enabled", "false").unwrap();
        assert_eq!(
            HarnessPosture::of(true, &cfg, false).for_agent(Some(agent)),
            HarnessPosture::MasterOff,
            "{agent:?}: the master wins"
        );
    }
    for agent in Program::ALL.into_iter().filter(|p| p.harness_table()) {
        assert_eq!(
            agent_switch_off(agent),
            format!(
                "{} is off",
                crate::prefs::agent_switch_label(agent.product())
            ),
            "{agent:?}"
        );
    }
    assert_eq!(
        agent_switch_off(Program::Codex),
        "Enable codex-alab Autopilot is off"
    );
    let mut codes: Vec<u64> = AutopilotView::ALL.iter().map(|v| v.code()).collect();
    codes.extend(Program::ALL.map(|p| AutopilotView::AgentOff(p).code()));
    let unique: std::collections::BTreeSet<u64> = codes.iter().copied().collect();
    assert_eq!(unique.len(), 5 + Program::ALL.len(), "{codes:?}");
    assert!(codes.iter().all(|c| *c < 256), "bits 8-15 hold it");
}

fn idle_host() -> crate::harness_host::HostHandle {
    crate::harness_host::HostHandle::for_supervised_test(
        Vec::new(),
        std::sync::Arc::new(|_| crate::harness_host::BodyEnd::Stopped),
    )
}

fn autopilot_records(app: &App) -> Vec<(String, Vec<String>)> {
    let key = format!(
        "harness.autopilot.{}",
        app.pool.get(0).unwrap().ctx.self_id.as_str()
    );
    app.messages
        .log()
        .records()
        .filter(|r| r.key.as_deref() == Some(key.as_str()))
        .map(|r| (r.title.clone(), r.detail.clone()))
        .collect()
}

#[test]
fn the_window_writes_the_word_and_the_chip_cycles_it() {
    let mut app = App::headless_for_test();
    assert_eq!(app.harness_posture(), HarnessPosture::Unhosted);
    assert_eq!(
        app.session_autopilot(0),
        (None, AutopilotView::Unhosted, false)
    );
    assert_eq!(app.cycle_session_autopilot(0), AutopilotView::Unhosted);
    assert_eq!(app.session_autopilot(0).0, None, "no host: nothing written");

    let host = idle_host();
    app.harness = Some(host.clone());
    assert_eq!(app.harness_posture(), HarnessPosture::On(Autopilot::Cruise));
    let ctx = app.pool.get(0).unwrap().ctx.clone();
    let rings = crate::harness_host::rings_here();
    assert!(app.set_session_autopilot(0, Some(Autopilot::Land), crate::autopilot::Surface::Chip));
    assert_eq!(
        crate::harness_host::rings_here(),
        rings + 1,
        "the host is rung"
    );
    assert_eq!(ctx.meta.lock().unwrap().autopilot.as_deref(), Some("land"));
    assert_eq!(
        app.session_autopilot(0),
        (Some(Autopilot::Land), AutopilotView::Land, false)
    );
    assert!(
        !app.set_session_autopilot(0, Some(Autopilot::Land), crate::autopilot::Surface::Chip),
        "a re-set moves nothing"
    );
    assert_eq!(crate::harness_host::rings_here(), rings + 1);
    assert_eq!(
        ctx.human_input.wire(crate::metrics::now_us()),
        "-",
        "never a person's keystroke"
    );
    let events: Vec<String> = ctx
        .timeline
        .lock()
        .unwrap()
        .since(None)
        .filter(|e| e.kind == "meta-change")
        .map(|e| e.payload.to_string())
        .collect();
    assert_eq!(events, ["field=autopilot value=land"]);
    assert_eq!(
        autopilot_records(&app),
        [(
            "Autopilot set to land".to_string(),
            vec![
                format!("Tab 1: {}", mode_words(Autopilot::Land).record),
                "this session's own setting".to_string(),
                "set from the chip".to_string()
            ]
        )]
    );

    assert_eq!(app.cycle_session_autopilot(0), AutopilotView::Off);
    assert_eq!(app.session_autopilot(0).0, Some(Autopilot::Off));
    assert_eq!(app.cycle_session_autopilot(0), AutopilotView::Cruise);
    assert_eq!(app.session_autopilot(0).0, Some(Autopilot::Cruise));
    assert_eq!(app.cycle_session_autopilot(0), AutopilotView::Land);

    assert!(app.set_session_autopilot(0, None, crate::autopilot::Surface::Chip));
    assert_eq!(
        app.session_autopilot(0),
        (None, AutopilotView::Cruise, false)
    );
    let records = autopilot_records(&app);
    assert_eq!(
        records.last().map(|(_, detail)| detail[1].as_str()),
        Some("back to the default, [harness] autopilot")
    );

    let now = crate::metrics::now_us();
    crate::session_timeline::claim_supervisor(&ctx, "someone-else", None, None, now).unwrap();
    assert!(!app.session_autopilot(0).2, "another holder's claim");
    crate::session_timeline::release_supervisor(&ctx, None, now, aterm_slog::Actor::Ctl);
    crate::session_timeline::claim_supervisor(
        &ctx,
        &crate::harness_host::holder(),
        None,
        None,
        now,
    )
    .unwrap();
    assert!(app.session_autopilot(0).2, "this host's claim");
    assert_eq!(own_holder(), crate::harness_host::holder(), "built once");
    crate::session_timeline::claim_supervisor(
        &ctx,
        &crate::harness_host::holder(),
        None,
        Some(1),
        now,
    )
    .unwrap();
    assert!(!app.session_autopilot(0).2, "this host's lapsed lease");

    app.harness_policy = crate::harness_policy_cell::HarnessPolicyCell::new(
        crate::app_config::HarnessPolicy::read("[harness]\nenabled = false\n"),
    );
    assert_eq!(app.harness_posture(), HarnessPosture::MasterOff);
    assert_eq!(app.cycle_session_autopilot(0), AutopilotView::MasterOff);
    assert_eq!(app.session_autopilot(0).0, None, "nothing written");
    host.shutdown_and_join();
}

struct StopHost(crate::harness_host::HostHandle);

impl Drop for StopHost {
    fn drop(&mut self) {
        self.0.shutdown_and_join();
    }
}

fn agent_policy_app() -> (App, StopHost) {
    let mut app = App::headless_for_test();
    let host = idle_host();
    let policy = crate::app_config::HarnessPolicy::read(
        "[harness]\nheadless = true\n[harness.claude]\nenabled = false\n",
    );
    host.set_config(policy.policy.clone());
    app.harness = Some(host.clone());
    app.harness_policy = crate::harness_policy_cell::HarnessPolicyCell::new(policy);
    app.set_session_autopilot(0, Some(Autopilot::Cruise), crate::autopilot::Surface::Chip);
    (app, StopHost(host))
}

fn publish_program(app: &App, name: &str) {
    let ctx = &app.pool.get(0).unwrap().ctx;
    let mut timeline = ctx.timeline.lock().unwrap();
    timeline.note_foreground_group(7);
    timeline.set_program(7, Some(name.to_string()));
}

#[test]
fn a_disabled_agent_shows_off_without_erasing_its_session_intent() {
    let (app, host) = agent_policy_app();
    publish_program(&app, "claude");
    assert!(!host.0.admits(aterm_phase::Program::Claude));
    assert!(host.0.admits(aterm_phase::Program::Agy));
    let ctx = &app.pool.get(0).unwrap().ctx;
    crate::session_timeline::claim_supervisor(
        ctx,
        own_holder(),
        None,
        None,
        crate::metrics::now_us(),
    )
    .unwrap();
    let (word, view, supervised) = app.session_autopilot(0);
    assert_eq!(word, Some(Autopilot::Cruise), "the stored intent survives");
    assert!(supervised, "the actual claim remains visible");
    assert_eq!(view.wire(), "off", "Claude is denied by the host's policy");
    assert!(!view.carries(), "a stale claim does not admit a card");
    assert!(
        view.wears_glass(supervised).is_none(),
        "a stale claim does not admit a card"
    );
    assert_eq!(
        harness_theme(
            view,
            supervised,
            word,
            app.unhosted_glass(Some(aterm_phase::Program::Claude))
        ),
        None,
        "nor the border"
    );
    assert_eq!(app.harness_state().sessions[0].view.wire(), "off");

    publish_program(&app, "agy");
    assert_eq!(app.session_autopilot(0).1, AutopilotView::Cruise);
    assert_eq!(app.harness_state().sessions[0].view, AutopilotView::Cruise);
}

#[test]
fn an_unhosted_window_keeps_the_master_and_agent_switches() {
    use aterm_phase::Program;
    let claude = Some(Program::Claude);
    for (toml, lit_claude, lit_agy) in [
        ("[harness]\nheadless = true\n", true, true),
        ("[harness]\nenabled = false\n", false, false),
        (
            "[harness]\nheadless = true\n[harness.claude]\nenabled = false\n",
            false,
            true,
        ),
    ] {
        let mut app = App::headless_for_test();
        assert!(app.harness.is_none(), "CONTROL: no host");
        app.harness_policy = crate::harness_policy_cell::HarnessPolicyCell::new(
            crate::app_config::HarnessPolicy::read(toml),
        );
        assert_eq!(app.unhosted_glass(claude), lit_claude, "{toml:?}: Claude");
        assert_eq!(
            app.unhosted_glass(Some(Program::Agy)),
            lit_agy,
            "{toml:?}: agy"
        );
        let (word, view, supervised) = app.session_autopilot(0);
        assert_eq!(view, AutopilotView::Unhosted, "{toml:?}");
        assert_eq!(
            harness_theme(view, supervised, word, app.unhosted_glass(claude)).is_some(),
            lit_claude,
            "{toml:?}: the border"
        );
    }
}

#[test]
fn the_published_posture_joins_the_rosters_actual_agent() {
    let (app, _host) = agent_policy_app();
    app.publish_harness_posture();
    let read = |name| {
        publish_program(&app, name);
        let roster = crate::control::cmd_sessions_store(&app.store, None);
        roster
            .lines()
            .nth(1)
            .expect("one session")
            .split_whitespace()
            .find_map(|field| field.strip_prefix("autopilot="))
            .expect("effective mode")
            .to_string()
    };
    assert_eq!(read("claude"), "off");
    assert_eq!(read("agy"), "cruise");
    assert_eq!(read("codex"), "-", "a vendor codex claims no mode");
    assert_eq!(read("sh"), "-", "non-agent rows do not claim a mode");
}

#[test]
fn the_app_publishes_its_posture_to_the_registry() {
    let mut app = App::headless_for_test();
    let read = |app: &App| {
        app.store
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .harness_posture()
    };
    assert_eq!(read(&app), HarnessPosture::Unhosted, "unpublished");
    let host = idle_host();
    app.harness = Some(host.clone());
    app.publish_harness_posture();
    assert_eq!(read(&app), HarnessPosture::On(Autopilot::Cruise));
    host.shutdown_and_join();
}

#[test]
fn the_harness_page_opens_in_the_window_asked() {
    use crate::native_settings::SettingsRoute;
    let mut app = App::headless_for_test();
    assert!(
        app.native_settings_route_view_target(SettingsRoute::Harness)
            .is_none()
    );
    assert!(app.open_harness_settings(WindowId(0), crate::harness_screen::HarnessFocus::Top));
    assert!(
        app.native_settings_route_view_target(SettingsRoute::Harness)
            .is_some()
    );
    let title = AutopilotView::Unhosted.title();
    assert!(
        title.ends_with(&format!(
            "Settings \u{25b8} {}",
            SettingsRoute::Harness.label()
        )),
        "{title}"
    );
    assert_eq!(
        SettingsRoute::from_path("/harness"),
        Some(SettingsRoute::Harness)
    );
}

#[test]
fn the_wire_word_is_the_effective_mode_of_an_agent_session() {
    let on = HarnessPosture::On(Autopilot::Cruise);
    assert_eq!(
        wire_field(on, None, Some(aterm_phase::Program::Claude)),
        "cruise"
    );
    assert_eq!(
        wire_field(
            on,
            Some(Autopilot::Land),
            Some(aterm_phase::Program::Claude)
        ),
        "land"
    );
    assert_eq!(
        wire_field(on, Some(Autopilot::Off), Some(aterm_phase::Program::Claude)),
        "off"
    );
    assert_eq!(
        wire_field(
            HarnessPosture::On(Autopilot::Off),
            None,
            Some(aterm_phase::Program::Claude)
        ),
        "off"
    );
    assert_eq!(
        wire_field(
            HarnessPosture::On(Autopilot::Off),
            Some(Autopilot::Cruise),
            Some(aterm_phase::Program::Claude)
        ),
        "cruise",
        "a session's word opts it in under a default of off"
    );
    assert_eq!(
        wire_field(
            HarnessPosture::MasterOff,
            Some(Autopilot::Cruise),
            Some(aterm_phase::Program::Claude)
        ),
        "off"
    );
    assert_eq!(
        wire_field(
            HarnessPosture::Unhosted,
            None,
            Some(aterm_phase::Program::Claude)
        ),
        "off"
    );
    assert_eq!(
        wire_field(on, Some(Autopilot::Land), None),
        "-",
        "no agent: -"
    );
}
