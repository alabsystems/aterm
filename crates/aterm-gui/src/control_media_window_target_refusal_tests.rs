// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use crate::app_introspect::AuxTarget;
use crate::native_settings::SettingsRoute;
use crate::{App, WindowId};

#[test]
fn a_bare_word_that_is_no_target_is_refused_not_written() {
    let refusal = super::unknown_window_target("bogus").expect("a bare word is refused");
    assert!(refusal.starts_with("ERR "), "{refusal}");
    let listed: Vec<&str> = refusal
        .split_once("(targets: ")
        .and_then(|(_, tail)| tail.split_once(')'))
        .map(|(list, _)| list.split(" | ").collect())
        .expect("the refusal lists the targets");
    assert_eq!(
        listed,
        AuxTarget::ALL
            .iter()
            .map(|target| target.keyword())
            .collect::<Vec<_>>()
    );
    for operand in ["", "shot.png", "front.png", "sub/x", r"sub\x"] {
        assert_eq!(super::unknown_window_target(operand), None, "{operand:?}");
    }
}

#[test]
fn help_open_names_every_target_open_takes() {
    use crate::app_introspect::AuxTarget;
    use aterm_types::control_verbs::{OPEN_TARGETS, spec};
    let help = spec("open").expect("the open verb").help_line();
    assert!(help.contains(OPEN_TARGETS), "{help}");
    assert!(OPEN_TARGETS.contains("fact-menu"), "{OPEN_TARGETS}");
    assert!(
        super::open_target("autopilot-menu").is_err_and(|refusal| refusal.contains(OPEN_TARGETS)),
        "the Autopilot chip opens no menu: no such target"
    );
    for &target in AuxTarget::ALL {
        let word = target.keyword();
        match super::open_target(word) {
            Ok(taken) => assert_eq!(taken, (target, false), "{word}"),
            Err(refusal) => {
                assert!(!target.opens(), "{word} is opened but refused: {refusal}");
                assert!(refusal.contains(OPEN_TARGETS), "{word}: {refusal}");
            }
        }
    }
    assert_eq!(
        super::open_target("tab-menu close"),
        Ok((AuxTarget::TabMenu, true))
    );
    assert_eq!(
        super::open_target("settings").map(|(t, _)| t),
        Ok(AuxTarget::Prefs),
        "an alias resolves"
    );
}

/// `open fact-menu <name>` takes every name `controls` prints (each fact's `fact=<key>`,
/// [`crate::claude_lights::Fact::key`]), and the usage names them all. NEGATIVE CONTROL: a
/// bogus name, a missing one and a trailing word are refused with that usage.
#[test]
fn open_fact_menu_names_its_fact() {
    use crate::claude_lights::Fact;
    for (words, fact) in [
        ("fact-menu model", Fact::Model),
        ("fact-menu effort", Fact::Effort),
        ("FACT-MENU path", Fact::Path),
        ("fact-menu  branch", Fact::Branch),
        ("fact-menu context", Fact::Context),
        ("fact-menu usage", Fact::Usage),
        ("fact-menu top-effort", Fact::TopEffort),
        ("fact-menu account", Fact::Account),
    ] {
        let said: Vec<String> = words
            .split_whitespace()
            .map(str::to_ascii_lowercase)
            .collect();
        assert_eq!(
            said,
            ["fact-menu", fact.key()],
            "{words}: the name `controls` prints as `fact={}`",
            fact.key()
        );
        assert_eq!(
            super::fact_menu_operand(words),
            Some(Ok(Some(fact))),
            "{words}"
        );
    }
    assert_eq!(super::fact_menu_operand("fact-menu close"), Some(Ok(None)));
    for refused in ["fact-menu", "fact-menu bogus", "fact-menu path x"] {
        let refusal = super::fact_menu_operand(refused)
            .expect("the fact menu's words")
            .expect_err(refused);
        assert_eq!(
            refusal,
            "ERR usage: open fact-menu <model | effort | path | branch | context | usage \
             | top-effort | account> | open fact-menu close\n",
            "{refused}"
        );
    }
    assert_eq!(super::fact_menu_operand("tab-menu"), None);
    assert_eq!(super::fact_menu_operand("fact-menus"), None);
}

/// Ordinary menus and native pages retain their routes; retired program-coordinate
/// popups are rejected as unsupported operands.
#[test]
fn open_routes_surfaces_without_program_coordinate_popups() {
    use super::OpenRoute;
    use crate::app_introspect::AuxTarget;
    for (rest, target, close) in [
        ("tab-menu", AuxTarget::TabMenu, false),
        ("tab-menu close", AuxTarget::TabMenu, true),
    ] {
        assert_eq!(super::open_route(rest), OpenRoute::Aux { target, close });
    }
    assert_eq!(
        super::open_route("fact-menu close"),
        OpenRoute::FactMenu(None)
    );
    assert_eq!(
        super::open_route("app about"),
        OpenRoute::App(crate::app_control::OpenRequest::About)
    );
    for rest in [
        "tab-menu at=20,40",
        "tab-menu at=20,40 by=keys",
        "tab-menu by=pointer at=0,0",
        "tab-menu bogus",
        "autopilot-menu",
    ] {
        assert!(
            matches!(super::open_route(rest), OpenRoute::Refused(_)),
            "{rest:?}"
        );
    }
}

/// One list writes an exhaustive `match` over `Fact` and the facts `Fact::EVERY` is held
/// to, so a fact left out of `EVERY` (which `Fact::parse` walks and the usage prints) fails
/// here, and a new variant does not compile until it is listed. Each wire name parses back
/// to its fact.
#[test]
fn every_fact_is_in_every_so_each_parses_and_the_usage_names_it() {
    use crate::claude_lights::Fact;
    macro_rules! each_fact {
        ($($variant:ident),+ $(,)?) => {{
            let ident = |fact: Fact| match fact {
                $(Fact::$variant => stringify!($variant)),+
            };
            [$((Fact::$variant, ident(Fact::$variant))),+]
        }};
    }
    let variants = each_fact!(
        TopEffort, Model, Effort, Path, Branch, Context, Usage, Account
    );
    let usage = super::fact_menu_operand("fact-menu")
        .expect("the fact menu's words")
        .expect_err("a missing name is refused with the usage");
    let named: Vec<&str> = usage
        .split_once('<')
        .and_then(|(_, rest)| rest.split_once('>'))
        .map(|(names, _)| names.split('|').map(str::trim).collect())
        .unwrap_or_default();
    for (fact, ident) in variants {
        assert!(
            Fact::EVERY.contains(&fact),
            "Fact::{ident} is left out of Fact::EVERY: {:?}",
            Fact::EVERY
        );
        assert_eq!(
            Fact::parse(fact.key()),
            Some(fact),
            "Fact::{ident}'s wire name `{}` parses back",
            fact.key()
        );
        assert_eq!(
            super::fact_menu_operand(&format!("fact-menu {}", fact.key())),
            Some(Ok(Some(fact))),
            "Fact::{ident}: `open fact-menu {}`",
            fact.key()
        );
        assert_eq!(
            named.iter().filter(|&&name| name == fact.key()).count(),
            1,
            "Fact::{ident}'s `{}` is in the usage once: {usage}",
            fact.key()
        );
    }
    assert_eq!(
        (Fact::EVERY.len(), named.len()),
        (variants.len(), variants.len()),
        "EVERY and the usage hold each fact once: {:?}, {usage}",
        Fact::EVERY
    );
}

#[test]
fn an_open_overlay_is_photographed_and_only_that_one() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.tab_strip_rows = 1;
    app.push_stub_tab(wid, crate::stub_session(app.next_session_id));
    app.splice_tab_strip(wid);
    let shown = |app: &App| {
        AuxTarget::ALL
            .iter()
            .copied()
            .filter(|&t| t != AuxTarget::Front && app.window_target_refusal(t).is_none())
            .collect::<Vec<_>>()
    };
    assert_eq!(shown(&app), vec![], "nothing is open yet");

    app.palette_enter();
    assert_eq!(shown(&app), vec![AuxTarget::Menu]);
    app.palette_exit();

    assert!(app.open_active_tab_context_menu(wid));
    assert_eq!(shown(&app), vec![AuxTarget::TabMenu]);
    assert!(app.close_tab_menu(wid));

    assert!(app.open_overview().is_ok());
    assert_eq!(shown(&app), vec![AuxTarget::Overview]);
    app.overview_exit(wid);

    assert_eq!(shown(&app), vec![], "every overlay closed again");
}

#[test]
fn settings_counts_only_as_the_active_tab_on_its_own_route() {
    let mut app = App::headless_for_test();
    assert!(app.open_settings_tab(SettingsRoute::Home));
    assert_eq!(app.window_target_refusal(AuxTarget::Prefs), None);
    assert!(
        app.window_target_refusal(AuxTarget::About).is_some(),
        "Settings on its home route is not the about page"
    );
    assert!(app.window_target_refusal(AuxTarget::Update).is_some());

    assert!(app.open_settings_tab(SettingsRoute::About));
    assert!(app.window_target_refusal(AuxTarget::About).is_some());
    assert_eq!(
        app.window_target_refusal(AuxTarget::Prefs),
        None,
        "any route is the Settings surface"
    );

    assert!(app.open_about_tab());
    assert_eq!(app.window_target_refusal(AuxTarget::About), None);
    assert!(app.window_target_refusal(AuxTarget::Prefs).is_some());

    app.switch_tab_in(WindowId(0), 0);
    assert!(
        app.read_aux_controls(AuxTarget::Prefs)[0].starts_with("state open=true"),
        "`controls prefs` still reads the background Settings tab"
    );
    assert!(
        app.window_target_refusal(AuxTarget::Prefs).is_some(),
        "but the front window shows the terminal"
    );
}

#[cfg(any(target_os = "macos", windows))]
#[test]
fn aux_capture_refuses_before_admission() {
    let photograph = |app: &mut App, target| {
        let bodies = crate::app_introspect::capture_bodies();
        let (reply, rx) = std::sync::mpsc::channel();
        app.capture_aux_window(
            target,
            crate::control::ReplyRetentionPermit::from_own_budget(),
            crate::control_auth::ConfinedImage::in_memory(),
            crate::control::CaptureCancellation::new(),
            reply,
        );
        let answer = rx
            .recv_timeout(std::time::Duration::from_secs(60))
            .expect("the capture answers")
            .expect_err("a headless app photographs nothing");
        (answer, crate::app_introspect::capture_bodies() - bodies)
    };
    let mut app = App::headless_for_test();
    let (refused, admitted) = photograph(&mut app, AuxTarget::Prefs);
    assert_eq!(
        refused,
        "prefs is not open in the front window; use `open prefs` first"
    );
    assert_eq!(admitted, 0, "refused before admission");

    assert!(app.open_settings_tab(SettingsRoute::Home));
    let (answer, admitted) = photograph(&mut app, AuxTarget::Prefs);
    assert_eq!(admitted, 1, "shown, the capture is admitted: {answer}");
    assert_eq!(answer, "no window to capture (headless)");
}
