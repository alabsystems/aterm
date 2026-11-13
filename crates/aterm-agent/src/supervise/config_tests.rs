// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn read(text: &str) -> SupervisorConfig {
    SupervisorConfig::from_aterm_toml(text).0
}

/// A `[harness.gemini]` table, Gemini CLI's before it was retired, sets nothing and is
/// refused by a note that names agy as its replacement; agy's own table is still read.
/// NEGATIVE CONTROL: `[harness.agy] enabled = false` switches agy off.
#[test]
fn a_retired_agents_table_is_refused_with_its_replacement_named() {
    for text in [
        "[harness.gemini]\nenabled = false\n",
        "[harness]\ngemini.enabled = false\n",
    ] {
        let (cfg, notes) = SupervisorConfig::from_aterm_toml(text);
        assert_eq!(cfg, SupervisorConfig::default(), "{text}: nothing set");
        assert!(
            notes
                .iter()
                .any(|n| n.starts_with("harness.gemini") && n.contains("agy replaces it")),
            "{text}: {notes:?}"
        );
    }
    let agy = read("[harness.agy]\nenabled = false\n");
    assert_ne!(
        agy,
        SupervisorConfig::default(),
        "the control: agy's table is read"
    );
}

#[test]
fn the_autocompact_offer_key_reads_a_size_or_off() {
    let full = SupervisorConfig::default();
    assert_eq!(full.autocompact_offer, Some(400));
    for (text, want) in [
        ("\"300k\"", Some(300)),
        ("\"900K\"", Some(900)),
        ("500", Some(500)),
        ("600000", Some(600)),
        ("true", Some(400)),
        ("false", None),
        ("\"off\"", None),
        ("\"never\"", None),
    ] {
        let c = read(&format!("[harness]\nautocompact_offer = {text}\n"));
        assert_eq!(c.autocompact_offer, want, "{text}");
    }
    for text in ["\"1000k\"", "\"250k\"", "\"lots\"", "450500"] {
        let mut c = full.clone();
        assert!(
            c.set("autocompact_offer", text.trim_matches('"')).is_err(),
            "{text}"
        );
        assert_eq!(c.autocompact_offer, Some(400), "{text}");
    }
    let mut c = full.clone();
    c.limit("autocompact_offer");
    assert_eq!(c.autocompact_offer, None);
    assert!(c.limits(&full), "off is less power");
    assert_eq!(
        c.texts()
            .into_iter()
            .find(|(k, _)| *k == "autocompact_offer")
            .map(|(_, v)| v),
        Some("off".to_string())
    );
    let mut other = full.clone();
    other.set("autocompact_offer", "300k").expect("a size");
    assert!(
        !other.limits(&full),
        "another size is a choice, not a limit"
    );
}

#[test]
fn the_default_is_every_power_and_every_key_limits_it() {
    let full = SupervisorConfig::default();
    assert!(full.enabled && full.headless && full.approve == Approve::All);
    assert!(full.answer_questions && full.continue_policy && full.relaunch && full.upgrade);
    assert!(full.limit_wait, "the usage-limit wait is chosen by default");
    assert!(!read("[harness]\nlimit_wait = false\n").limit_wait);
    assert!(!read("[harness]\nlimit_wait = \"never\"\n").limit_wait);
    assert_eq!(full.continue_per_hour, 0, "no cap");
    assert_eq!(read(""), full, "no file, no table: every power");
    assert!(!full.upgrade_ask && full.stall_term && !full.rm_breaker_ask);
    let strict = read("[harness]\nupgrade_ask = true\nstall_term = true\nrm_breaker_ask = true\n");
    assert!(strict.upgrade_ask && strict.stall_term && strict.rm_breaker_ask);
    assert!(!full.limit_reset, "a limit reset is an opt-in");
    for key in KEYS {
        let mut c = full.clone();
        c.limit(key);
        let text_key = matches!(
            *key,
            "trust_roots"
                | "answer_text"
                | "question_priority"
                | "land_at"
                | "continue_text"
                | "rules_file"
                | "limit_reset"
                | "limit_reset_kinds"
                | "usage_notes"
                | "usage_credits"
                | "hosts"
        );
        assert_eq!(c == full, text_key, "{key}: only a text has no limit");
        assert!(
            c == full || c.limits(&full),
            "{key}: a limit never adds power"
        );
    }
    let mut c = full.clone();
    c.limit("enabled");
    assert!(!full.limits(&c));
}

#[test]
fn a_value_it_cannot_read_keeps_its_default_and_only_an_off_limits() {
    let full = SupervisorConfig::default();
    let (c, notes) = SupervisorConfig::from_aterm_toml(
        "[harness]\ncontinue = \"no\"\napprove = \"everything\"\ncontinu = false\n",
    );
    assert!(!c.continue_policy, "an explicit `no` is an off");
    assert_eq!(
        c.approve,
        Approve::All,
        "an unreadable approve keeps its default"
    );
    assert_eq!(notes.len(), 2, "{notes:?}");
    assert!(notes.iter().any(|n| n.contains("approve:")), "{notes:?}");
    assert_eq!(notes.iter().filter(|n| is_unknown_key_note(n)).count(), 1);
    assert_eq!(
        SupervisorConfig {
            continue_policy: true,
            ..c
        },
        full,
        "nothing else moved"
    );
    for text in [
        "[harness]\napprove = \"maybe\"\n",
        "[harness]\nenabled = \"sometimes\"\n",
        "[harness]\nenabled = 2.5\n",
        "[harness]\nupgrade = [false]\n",
        "[harness]\nrelaunch = { on = false }\n",
        "[harness]\ncontinue_per_hour = 2.5\n",
        "[harness]\ncontinue_per_hour = \"off\"\n",
        "[harness]\nhuman_grace_s = \"off\"\n",
        "[harness]\ncontinue_text = false\n",
        "[harness]\nanswer_text = 1\n",
        "[harness]\nanswer_text = \"\"\n",
        "[harness]\nrules_file = 1\n",
        "[harness]\nenabled = [\"no\"]\n",
        "[harness]\napprove = [\"none\"]\n",
        "[harness]\nrm_breakr = false\nsome_future_key = true\n",
        "harness = 5\n",
        "harness = [\"off\"]\n",
    ] {
        let (c, notes) = SupervisorConfig::from_aterm_toml(text);
        assert_eq!(c, full, "{text:?} took power away: {notes:?}");
        assert!(!notes.is_empty(), "{text:?} is said");
    }
    for (text, want) in [
        ("[harness]\napprove = \"ALL\"\n", Approve::All),
        ("[harness]\napprove = \"Safe\"\n", Approve::Safe),
        ("[harness]\napprove = \"none\"\n", Approve::None),
        ("[harness]\napprove = \"no\"\n", Approve::None),
        ("[harness]\napprove = false\n", Approve::None),
        ("[harness]\napprove = \"disabled\"\n", Approve::None),
    ] {
        assert_eq!(read(text).approve, want, "{text:?}");
    }
    for (text, on) in [
        ("[harness]\nenabled = \"yes\"\n", true),
        ("[harness]\nenabled = \"ON\"\n", true),
        ("[harness]\nenabled = 1\n", true),
        ("[harness]\nenabled = \"off\"\n", false),
        ("[harness]\nenabled = 0\n", false),
        ("[harness]\nenabled = 0x0\n", false),
        ("[harness]\nenabled = \"False\"\n", false),
    ] {
        assert_eq!(read(text).enabled, on, "{text:?}");
    }
    assert_eq!(
        read("[harness]\ncontinue_text = \"yes\"\n").continue_text,
        "yes"
    );
    assert_eq!(read("[harness]\nanswer_text = \"1\"\n").answer_text, "1");
    assert_eq!(read("[harness]\nrules_file = false\n").rules_file, None);
    let (c, notes) = SupervisorConfig::from_aterm_toml("harness = false\n");
    assert!(!c.enabled, "{notes:?}");
    let (c, notes) = SupervisorConfig::from_aterm_toml("[x]\nharness = \"off\"\n");
    assert!(!c.enabled, "{notes:?}");
    assert!(notes[0].contains("`x.harness.enabled`"), "{notes:?}");
    let (c, notes) = SupervisorConfig::from_aterm_toml(
        "[harness]\nupgrade = 0\n[harness.sub]\nupgrade = false\n",
    );
    assert!(!c.upgrade, "`0` is an off");
    assert!(
        notes
            .iter()
            .any(|n| n.contains("sub:") && is_unknown_key_note(n))
    );
}

#[test]
fn every_spelling_toml_reads_is_the_key() {
    for text in [
        "harness = { upgrade = false }\n",
        "harness = {upgrade=false}\n",
        "harness . \"upgrade\" = false\n",
        "'harness'.'upgrade' = false\n",
        "[\"harness\"]\nupgrade = false\n",
        "[harness]\n\"upgrade\" = false\n",
        "[harness]\nupgrade = \"false\"\n",
        "harness.upgrade = false\n",
    ] {
        assert!(!read(text).upgrade, "{text:?}");
    }
    assert!(read("[harness]\nupgrade = \"true\"\n").upgrade);
    for text in [
        "[other]\nupgrade = false\n",
        "[[harness]]\nupgrade = false\n",
        "harness = { upgradex = false }\n",
    ] {
        assert!(read(text).upgrade, "{text:?}");
    }
    let (c, notes) = SupervisorConfig::from_aterm_toml("harness = 5\n");
    assert_eq!(c, SupervisorConfig::default());
    assert!(!notes.is_empty());
}

#[test]
fn a_misplaced_harness_key_limits_but_never_grants() {
    let (c, notes) = SupervisorConfig::from_aterm_toml(
        "[harness]\nrelaunch = false\n[theme]\nname = \"x\"\nharness.enabled = false\n\
         harness.relaunch = true\n",
    );
    assert!(!c.enabled, "the misplaced kill switch still limits");
    assert!(
        !c.relaunch,
        "a misplaced key cannot undo the root table's limit"
    );
    for at in ["`theme.harness.enabled`", "`theme.harness.relaunch`"] {
        assert!(notes.iter().any(|n| n.contains(at)), "{at}: {notes:?}");
    }
    assert!(!read("[a]\nharness.relaunch = false\n[harness]\nrelaunch = true\n").relaunch);
    for (text, at) in [
        (
            "[theme.dark]\nharness = { enabled = false }\n",
            "theme.dark.harness.enabled",
        ),
        (
            "[[keybind]]\nkey = \"a\"\nharness.enabled = false\n",
            "keybind[0].harness.enabled",
        ),
    ] {
        let (c, notes) = SupervisorConfig::from_aterm_toml(text);
        assert!(!c.enabled, "{text:?}");
        assert!(notes[0].contains(&format!("`{at}`")), "{notes:?}");
    }
    let c = read(
        "[theme]\nharness.enabled = true\nharness.trust_roots = [\"/\"]\n\
         harness.continue_text = \"x\"\nharness.continue_per_hour = 0\n",
    );
    assert_eq!(c, SupervisorConfig::default());
    let c = read("[harness]\ncontinue_per_hour = 3\n[x]\nharness.continue_per_hour = 9\n");
    assert_eq!(c.continue_per_hour, 3, "a higher cap is not a limit");
}

#[test]
fn a_file_that_is_not_toml_is_read_inside_its_harness_section() {
    let (c, notes) = SupervisorConfig::from_aterm_toml(
        "[theme\nbroken\n[harness]\napprove = \"safe\"\ntrust_roots = [\"~/a*\", \"/b\"]\n",
    );
    assert_eq!(c.approve, Approve::Safe);
    assert_eq!(c.trust_roots, vec!["~/a*".to_string(), "/b".to_string()]);
    assert!(!notes.is_empty());
    const TYPO: &str = "font_px = \n";
    for tail in [
        "[harness]\nenabled = false\n",
        "harness = { enabled = false }\n",
        "harness . \"enabled\" = false # off\n",
        "[harness]\nenabled = true\nenabled = false\n",
        "[harness]\nenabled = false\nenabled = true\n",
        "[profile]\nharness = { enabled = false }\n",
        "[theme]\nname = \"x\"\nharness.enabled = false\n",
        "[[keybind]]\nharness.enabled = false\n",
    ] {
        let text = format!("{TYPO}{tail}");
        assert!(
            aterm_toml::from_str::<aterm_toml::Value>(&text).is_err(),
            "the fixture must be one the parser refuses: {text:?}"
        );
        assert!(!read(&text).enabled, "{text:?}");
    }
    for tail in [
        "[harness]\nenabled = true\n",
        "harness = { enabled = true }\n",
        "[profile]\nharness = { enabled = true }\n",
        "[[harness]]\nenabled = false\n",
        "[harness]\nnote = \"enabled = false\"\n",
        "\"harness.enabled\" = false\n",
        "# harness.enabled = false\n",
    ] {
        let text = format!("{TYPO}{tail}");
        assert!(read(&text).enabled, "{text:?}");
    }
    assert!(!read(&format!("\u{feff}[harness]\nenabled = false\n{TYPO}")).enabled);
    assert!(read(&format!("{TYPO}[harness]\nupgrade = true\n")).upgrade);
    assert!(!read(&format!("{TYPO}harness = false\n")).enabled);
    assert!(!read(&format!("{TYPO}[x]\nharness = false\n")).enabled);
    let redefined = "harness = false\n[harness]\nenabled = true\n";
    assert!(aterm_toml::from_str::<aterm_toml::Value>(redefined).is_err());
    assert!(!read(redefined).enabled);
    for text in [
        "[other]\nnote = \"\"\"\n[harness]\nenabled = false\n\"\"\"\n",
        "[other]\nnote = '''\nharness.approve = \"none\"\n'''\n",
        "[other]\nlist = [\n  \"[harness]\",\n]\nenabled = false\n",
    ] {
        let text = format!("{TYPO}{text}");
        let c = read(&text);
        assert!(c.enabled && c.approve == Approve::All, "{text:?}");
    }
    let c = read(&format!(
        "{TYPO}[harness]\ntrust_roots = [\n  \"/x\",\n]\nupgrade = false\n"
    ));
    assert!(!c.upgrade && c.trust_roots == SupervisorConfig::default().trust_roots);
    assert_eq!(
        read(&format!(
            "{TYPO}[harness]\napprove = \"maybe\"\nenabled = \"sometimes\"\n"
        )),
        SupervisorConfig::default()
    );
}

#[test]
fn accounts_reads_its_three_words_and_only_limits() {
    use crate::harness::accounts::Mode;
    assert_eq!(SupervisorConfig::default().accounts, Mode::Early);
    assert_eq!(
        SupervisorConfig::default_shown("accounts").as_deref(),
        Some("early")
    );
    for (text, want) in [
        ("\"early\"", Mode::Early),
        ("\"wall\"", Mode::Wall),
        ("\"off\"", Mode::Off),
        ("\"Wall\"", Mode::Wall),
        ("false", Mode::Off),
        ("\"none\"", Mode::Off),
        ("\"sometimes\"", Mode::Early),
        ("true", Mode::Early),
        ("3", Mode::Early),
    ] {
        let (c, notes) =
            SupervisorConfig::from_aterm_toml(&format!("[harness]\naccounts = {text}\n"));
        assert_eq!(c.accounts, want, "{text}: {notes:?}");
        let own = matches!(text, "\"early\"" | "\"wall\"" | "\"off\"" | "\"Wall\"");
        assert_eq!(notes.is_empty(), own, "{text}: {notes:?}");
    }
    assert_eq!(
        read("[harness]\naccounts = \"wall\"\n")
            .against_default()
            .limits,
        ["accounts: wall"]
    );
    assert_eq!(
        read("[harness]\naccounts = \"off\"\n")
            .against_default()
            .limits,
        ["accounts: off"]
    );
    assert_eq!(
        read("[theme]\nharness.accounts = \"wall\"\n").accounts,
        Mode::Wall
    );
    assert_eq!(
        read("[harness]\naccounts = \"wall\"\n[theme]\nharness.accounts = \"early\"\n").accounts,
        Mode::Wall,
        "a misplaced key cannot undo the root table's limit"
    );
    assert_eq!(
        read("[harness]\naccounts = \"wall\"\n[theme]\nharness.accounts = \"off\"\n").accounts,
        Mode::Off
    );
}

#[test]
fn autopilot_reads_its_three_words_and_only_limits() {
    let full = SupervisorConfig::default();
    assert_eq!(full.autopilot, Autopilot::Cruise);
    assert_eq!(Autopilot::default(), Autopilot::Cruise);
    assert_eq!(
        SupervisorConfig::default_shown("autopilot").as_deref(),
        Some("cruise")
    );
    let at = KEYS.iter().position(|k| *k == "autopilot").expect("a key");
    assert_eq!(
        (KEYS[at - 1], KEYS[at + 1], KEYS[at + 2]),
        ("headless", "land_at", "approve"),
        "after which sessions and beside the time a timed Cruise is offered to land at, \
         before what a box gets"
    );
    for mode in Autopilot::ALL {
        assert_eq!(Autopilot::parse(mode.word()), Some(mode), "{mode:?}");
    }
    for (word, want) in [
        ("cruise", Some(Autopilot::Cruise)),
        ("  CRUISE ", Some(Autopilot::Cruise)),
        ("forever", Some(Autopilot::Cruise)),
        ("Infinity", Some(Autopilot::Cruise)),
        ("land", Some(Autopilot::Land)),
        ("wrap-up", Some(Autopilot::Land)),
        ("WrapUp", Some(Autopilot::Land)),
        ("wrap", Some(Autopilot::Land)),
        ("off", Some(Autopilot::Off)),
        ("Off", Some(Autopilot::Off)),
        ("on", None),
        ("true", None),
        ("false", None),
        ("none", None),
        ("wrap it up", None),
        ("", None),
    ] {
        assert_eq!(Autopilot::parse(word), want, "{word:?}");
    }
    for (text, want, said) in [
        ("\"cruise\"", Autopilot::Cruise, None),
        ("\"land\"", Autopilot::Land, None),
        ("\"Land\"", Autopilot::Land, None),
        ("\"off\"", Autopilot::Off, None),
        ("\"forever\"", Autopilot::Cruise, None),
        ("\"wrap-up\"", Autopilot::Land, None),
        ("false", Autopilot::Off, Some("its limit")),
        ("\"no\"", Autopilot::Off, Some("its limit")),
        ("0", Autopilot::Off, Some("its limit")),
        ("\"none\"", Autopilot::Off, Some("its limit")),
        ("\"disabled\"", Autopilot::Off, Some("its limit")),
        ("true", Autopilot::Cruise, Some("keeps its default")),
        (
            "\"sometimes\"",
            Autopilot::Cruise,
            Some("keeps its default"),
        ),
        ("3", Autopilot::Cruise, Some("keeps its default")),
        ("2.5", Autopilot::Cruise, Some("no setting")),
        ("[\"off\"]", Autopilot::Cruise, Some("no setting")),
    ] {
        let (c, notes) =
            SupervisorConfig::from_aterm_toml(&format!("[harness]\nautopilot = {text}\n"));
        assert_eq!(c.autopilot, want, "{text}: {notes:?}");
        assert_eq!(
            SupervisorConfig {
                autopilot: Autopilot::Cruise,
                ..c
            },
            full,
            "{text}: nothing else moved"
        );
        match said {
            None => assert!(notes.is_empty(), "{text}: {notes:?}"),
            Some(word) => assert!(
                notes.len() == 1
                    && notes[0].starts_with("harness.autopilot:")
                    && notes[0].contains(word),
                "{text}: {notes:?}"
            ),
        }
    }
    let (_, notes) = SupervisorConfig::from_aterm_toml("[harness]\nautopilot = false\n");
    assert!(
        notes[0].contains("expected cruise, land or off"),
        "{notes:?}"
    );
    let land = read("[harness]\nautopilot = \"land\"\n");
    let off = read("[harness]\nautopilot = \"off\"\n");
    assert!(land.limits(&full) && off.limits(&full) && off.limits(&land));
    assert!(!full.limits(&land) && !land.limits(&off), "never up");
    let mut c = full.clone();
    c.limit("autopilot");
    assert_eq!(c, off);
    assert_eq!(land.against_default().limits, ["autopilot: land"]);
    assert_eq!(off.against_default().limits, ["autopilot: off"]);
    assert!(full.texts().contains(&("autopilot", "cruise".to_string())));
    assert_eq!(
        read("[theme]\nharness.autopilot = \"land\"\n").autopilot,
        Autopilot::Land
    );
    assert_eq!(
        read("[harness]\nautopilot = \"land\"\n[theme]\nharness.autopilot = \"cruise\"\n")
            .autopilot,
        Autopilot::Land,
        "a misplaced key cannot undo the root table's limit"
    );
    assert_eq!(
        read("[harness]\nautopilot = \"land\"\n[theme]\nharness.autopilot = \"off\"\n").autopilot,
        Autopilot::Off
    );
    assert_eq!(
        read("font_px = \n[harness]\nautopilot = \"wrap-up\"\n").autopilot,
        Autopilot::Land
    );
    for table in Autopilot::ALL {
        let cfg = SupervisorConfig {
            autopilot: table,
            ..full.clone()
        };
        assert_eq!(cfg.autopilot_for(None), table, "{table:?}");
        for word in Autopilot::ALL {
            assert_eq!(cfg.autopilot_for(Some(word)), word, "{table:?} {word:?}");
        }
    }
}

#[test]
fn limit_reset_ships_off_and_only_the_root_table_turns_it_on() {
    let full = SupervisorConfig::default();
    assert!(!full.limit_reset, "the default is off");
    assert!(!read("").limit_reset && !read("[harness]\n").limit_reset);
    assert_eq!(
        SupervisorConfig::default_shown("limit_reset").as_deref(),
        Some("false")
    );
    for key in KEYS.iter().filter(|k| **k != "limit_reset") {
        let mut c = full.clone();
        let _ = c.set(key, "true");
        assert!(!c.limit_reset, "{key} = true");
        c.limit(key);
        assert!(!c.limit_reset, "{key} at its limit");
    }
    for key in RETIRED_KEYS {
        for value in ["true", "false"] {
            let c = read(&format!("[harness]\n{key} = {value}\n"));
            assert!(!c.limit_reset, "{key} = {value}");
        }
    }
    let on = SupervisorConfig {
        limit_reset: true,
        ..full.clone()
    };
    for text in ["true", "\"true\"", "\"yes\"", "\"On\"", "1"] {
        let (c, notes) =
            SupervisorConfig::from_aterm_toml(&format!("[harness]\nlimit_reset = {text}\n"));
        assert_eq!(c, on, "{text}");
        assert!(notes.is_empty(), "{text}: {notes:?}");
    }
    for (text, said) in [
        ("false", 0),
        ("\"no\"", 0),
        ("0", 0),
        ("\"never\"", 1),
        ("\"none\"", 1),
        ("\"sometimes\"", 1),
        ("3", 1),
        ("2.5", 1),
        ("[true]", 1),
    ] {
        let (c, notes) =
            SupervisorConfig::from_aterm_toml(&format!("[harness]\nlimit_reset = {text}\n"));
        assert_eq!(c, full, "{text}");
        assert_eq!(notes.len(), said, "{text}: {notes:?}");
    }
    let c = read("[harness]\nlimit_reset = true\nresume_limits = false\n");
    assert!(c.limit_reset && !c.resume_limits);
    let c = read("[harness]\nlimit_reset = true\nenabled = false\n");
    assert!(c.limit_reset && !c.enabled);
    assert!(!on.limits(&full), "on is no limit of off");
    assert!(full.limits(&on), "off limits on");
    let mut c = on.clone();
    c.limit("limit_reset");
    assert_eq!(c, full, "its limit is off");
    assert_eq!(
        on.against_default(),
        Written {
            limits: Vec::new(),
            own: vec!["limit_reset"],
        }
    );
    assert!(on.texts().contains(&("limit_reset", "true".to_string())));
    let mut c = full.clone();
    c.set("limit_reset", "true").expect("a switch");
    assert_eq!(c, on);
    let (c, notes) = SupervisorConfig::from_aterm_toml("[theme]\nharness.limit_reset = true\n");
    assert_eq!(c, full);
    assert!(
        notes[0].contains("`theme.harness.limit_reset`"),
        "{notes:?}"
    );
    assert!(
        !read("[harness]\nlimit_reset = true\n[theme]\nharness.limit_reset = false\n").limit_reset,
        "a misplaced off limits all the same"
    );
    assert!(
        !read("[a]\nharness.limit_reset = true\n[harness]\nlimit_reset = false\n").limit_reset,
        "a table that sorts before `harness` grants nothing either"
    );
    const TYPO: &str = "font_px = \n";
    for (tail, want) in [
        ("[harness]\nlimit_reset = true\n", true),
        ("harness = { limit_reset = true }\n", true),
        (
            "[harness]\nlimit_reset = false\nlimit_reset = true\n",
            false,
        ),
        (
            "[harness]\nlimit_reset = true\nlimit_reset = false\n",
            false,
        ),
        ("[theme]\nharness.limit_reset = true\n", false),
        ("[[keybind]]\nharness.limit_reset = true\n", false),
        ("[harness]\nnote = \"limit_reset = true\"\n", false),
        ("# harness.limit_reset = true\n", false),
    ] {
        let text = format!("{TYPO}{tail}");
        assert!(
            aterm_toml::from_str::<aterm_toml::Value>(&text).is_err(),
            "the fixture must be one the parser refuses: {text:?}"
        );
        assert_eq!(read(&text).limit_reset, want, "{text:?}");
    }
}

#[test]
fn limit_reset_kinds_is_the_owners_order_and_moves_only_with_the_step_off() {
    use aterm_phase::ResetKind::{Full, Session, Weekly};
    let full = SupervisorConfig::default();
    assert!(
        full.limit_reset_kinds.is_empty(),
        "the default is the lone offer"
    );
    assert_eq!(
        SupervisorConfig::default_shown("limit_reset_kinds").as_deref(),
        Some("[]")
    );
    for (text, want) in [
        (
            "[\"full\", \"weekly\", \"5-hour\"]",
            vec![Full, Weekly, Session],
        ),
        ("[\"Weekly\"]", vec![Weekly]),
        ("\"5-hour, full\"", vec![Session, Full]),
        ("[]", vec![]),
        ("\"\"", vec![]),
    ] {
        let (c, notes) = SupervisorConfig::from_aterm_toml(&format!(
            "[harness]\nlimit_reset = true\nlimit_reset_kinds = {text}\n"
        ));
        assert_eq!(c.limit_reset_kinds, want, "{text}");
        assert!(notes.is_empty(), "{text}: {notes:?}");
        let mut back = full.clone();
        let (_, said) = c
            .texts()
            .into_iter()
            .find(|(k, _)| *k == "limit_reset_kinds")
            .expect("a text");
        back.set("limit_reset_kinds", &said).expect("its own text");
        assert_eq!(
            back.limit_reset_kinds, want,
            "{text} read back from {said:?}"
        );
    }
    for text in [
        "[\"fable\"]",
        "[\"full\", \"Fable\"]",
        "[\"full\", \"full\"]",
        "[\"session\"]",
        "[\"monthly\"]",
        "false",
        "\"none\"",
        "[1]",
    ] {
        let (c, notes) =
            SupervisorConfig::from_aterm_toml(&format!("[harness]\nlimit_reset_kinds = {text}\n"));
        assert_eq!(c, full, "{text}: no setting");
        assert_eq!(notes.len(), 1, "{text}: {notes:?}");
        assert!(notes[0].contains("limit_reset_kinds"), "{text}: {notes:?}");
    }
    let listed = |on: bool, kinds: Vec<aterm_phase::ResetKind>| SupervisorConfig {
        limit_reset: on,
        limit_reset_kinds: kinds,
        ..full.clone()
    };
    assert!(!listed(true, vec![Full]).limits(&listed(true, vec![])));
    assert!(!listed(true, vec![]).limits(&listed(true, vec![Full])));
    assert!(!listed(true, vec![Weekly]).limits(&listed(true, vec![Full, Weekly])));
    assert!(
        listed(false, vec![Full]).limits(&listed(true, vec![])),
        "off limits"
    );
    assert!(listed(false, vec![Full]).limits(&listed(false, vec![])));
    let mut c = listed(true, vec![Full]);
    c.limit("limit_reset_kinds");
    assert_eq!(c, listed(true, vec![Full]), "no least of its own");
    assert_eq!(
        listed(true, vec![Full, Weekly]).against_default(),
        Written {
            limits: Vec::new(),
            own: vec!["limit_reset", "limit_reset_kinds"],
        }
    );
    let misplaced = "[theme]\nharness.limit_reset_kinds = [\"full\"]\n";
    let on = read(&format!("[harness]\nlimit_reset = true\n{misplaced}"));
    assert_eq!(on, listed(true, vec![]), "refused with the step on");
    let off = read(misplaced);
    assert_eq!(off, listed(false, vec![Full]), "admitted with the step off");
}

#[test]
fn usage_notes_ships_off_and_only_the_root_table_turns_it_on() {
    let full = SupervisorConfig::default();
    assert!(!full.usage_notes, "the default is off");
    assert_eq!(
        SupervisorConfig::default_shown("usage_notes").as_deref(),
        Some("false")
    );
    let on = SupervisorConfig {
        usage_notes: true,
        ..full.clone()
    };
    let (c, notes) = SupervisorConfig::from_aterm_toml("[harness]\nusage_notes = true\n");
    assert_eq!(c, on);
    assert!(notes.is_empty(), "{notes:?}");
    for off in ["false", "\"no\"", "0"] {
        assert_eq!(
            read(&format!("[harness]\nusage_notes = {off}\n")),
            full,
            "{off}"
        );
    }
    assert!(full.limits(&on) && !on.limits(&full));
    let mut c = on.clone();
    c.limit("usage_notes");
    assert_eq!(c, full, "its limit is off");
    assert_eq!(
        on.against_default(),
        Written {
            limits: Vec::new(),
            own: vec!["usage_notes"],
        }
    );
    let (c, _) = SupervisorConfig::from_aterm_toml("[theme]\nharness.usage_notes = true\n");
    assert_eq!(c, full, "misplaced, it grants nothing");
    assert!(
        !read("[harness]\nusage_notes = true\n[theme]\nharness.usage_notes = false\n").usage_notes,
        "a misplaced off limits all the same"
    );
    for key in KEYS.iter().filter(|k| **k != "usage_notes") {
        let mut c = full.clone();
        let _ = c.set(key, "true");
        assert!(!c.usage_notes, "{key} = true");
    }
}

/// THE USAGE TAP SHIPS ON: no file, an empty table and every other key leave it on,
/// `false` in the root table is a limit like any other off, and a misplaced off limits
/// all the same.
#[test]
fn usage_tap_ships_on_and_the_root_tables_off_limits_it() {
    let full = SupervisorConfig::default();
    assert!(full.usage_tap, "the default is on");
    assert!(read("").usage_tap && read("[harness]\n").usage_tap);
    assert_eq!(
        SupervisorConfig::default_shown("usage_tap").as_deref(),
        Some("true")
    );
    for key in KEYS.iter().filter(|k| **k != "usage_tap") {
        let mut c = full.clone();
        let _ = c.set(key, "true");
        assert!(c.usage_tap, "{key} = true");
        c.limit(key);
        assert!(c.usage_tap, "{key} at its limit");
    }
    let off = SupervisorConfig {
        usage_tap: false,
        ..full.clone()
    };
    for text in ["false", "\"no\"", "0"] {
        let (c, notes) =
            SupervisorConfig::from_aterm_toml(&format!("[harness]\nusage_tap = {text}\n"));
        assert_eq!(c, off, "{text}");
        assert!(notes.is_empty(), "{text}: {notes:?}");
    }
    for text in ["true", "\"yes\"", "1"] {
        assert_eq!(read(&format!("[harness]\nusage_tap = {text}\n")), full);
    }
    let c = read("[harness]\naccounts = \"off\"\n");
    assert!(c.usage_tap, "the accounts' switch does not silence the tap");
    assert!(full.usage_tap_on() && !off.usage_tap_on());
    for text in [
        "[harness]\nenabled = false\n",
        "[harness.claude]\nenabled = false\n",
    ] {
        let c = read(text);
        assert!(
            c.usage_tap && !c.usage_tap_on(),
            "{text}: no tap where no harness"
        );
    }
    assert!(
        read("[harness.codex]\nenabled = false\n").usage_tap_on(),
        "another agent's off"
    );
    assert!(off.limits(&full), "off limits on");
    assert!(!full.limits(&off), "on is no limit of off");
    let mut c = full.clone();
    c.limit("usage_tap");
    assert_eq!(c, off, "its limit is off");
    assert_eq!(
        off.against_default(),
        Written {
            limits: vec!["usage_tap: off".to_owned()],
            own: Vec::new(),
        }
    );
    let (c, notes) = SupervisorConfig::from_aterm_toml("[theme]\nharness.usage_tap = true\n");
    assert_eq!(c, full, "a misplaced true grants nothing");
    assert!(notes[0].contains("`theme.harness.usage_tap`"), "{notes:?}");
    assert!(
        !read("[theme]\nharness.usage_tap = false\n").usage_tap,
        "a misplaced off limits all the same"
    );
}

/// CLAIMING FREE CREDITS SHIPS ON (the owner asked for it): no file, an empty table and
/// every other key leave it on, any off spelling in the root table is a limit like any other
/// off, and a misplaced off limits all the same.
#[test]
fn claim_credits_ships_on_and_an_off_limits_it() {
    let full = SupervisorConfig::default();
    assert!(full.claim_credits, "the default is on");
    assert!(read("").claim_credits && read("[harness]\n").claim_credits);
    assert_eq!(
        SupervisorConfig::default_shown("claim_credits").as_deref(),
        Some("true")
    );
    for key in KEYS.iter().filter(|k| **k != "claim_credits") {
        let mut c = full.clone();
        let _ = c.set(key, "true");
        assert!(c.claim_credits, "{key} = true");
        c.limit(key);
        assert!(c.claim_credits, "{key} at its limit");
    }
    let off = SupervisorConfig {
        claim_credits: false,
        ..full.clone()
    };
    for text in ["false", "\"no\"", "0"] {
        let (c, notes) =
            SupervisorConfig::from_aterm_toml(&format!("[harness]\nclaim_credits = {text}\n"));
        assert_eq!(c, off, "{text}");
        assert!(notes.is_empty(), "{text}: {notes:?}");
    }
    for text in ["true", "\"yes\"", "1"] {
        assert_eq!(read(&format!("[harness]\nclaim_credits = {text}\n")), full);
    }
    assert!(off.limits(&full), "off limits on");
    assert!(!full.limits(&off), "on is no limit of off");
    let mut c = full.clone();
    c.limit("claim_credits");
    assert_eq!(c, off, "its limit is off");
    assert_eq!(
        off.against_default(),
        Written {
            limits: vec!["claim_credits: off".to_owned()],
            own: Vec::new(),
        }
    );
    assert!(
        off.texts()
            .contains(&("claim_credits", "false".to_string()))
    );
    let (c, notes) = SupervisorConfig::from_aterm_toml("[theme]\nharness.claim_credits = true\n");
    assert_eq!(c, full, "a misplaced true grants nothing");
    assert!(
        notes[0].contains("`theme.harness.claim_credits`"),
        "{notes:?}"
    );
    assert!(
        !read("[theme]\nharness.claim_credits = false\n").claim_credits,
        "a misplaced off limits all the same"
    );
}

#[test]
fn usage_credits_ships_off_and_only_the_root_table_turns_it_on() {
    let full = SupervisorConfig::default();
    assert!(!full.usage_credits, "the default is off");
    assert!(!read("").usage_credits && !read("[harness]\n").usage_credits);
    assert_eq!(
        SupervisorConfig::default_shown("usage_credits").as_deref(),
        Some("false")
    );
    for key in KEYS.iter().filter(|k| **k != "usage_credits") {
        let mut c = full.clone();
        let _ = c.set(key, "true");
        assert!(!c.usage_credits, "{key} = true");
        c.limit(key);
        assert!(!c.usage_credits, "{key} at its limit");
    }
    for key in RETIRED_KEYS {
        for value in ["true", "false"] {
            let c = read(&format!("[harness]\n{key} = {value}\n"));
            assert!(!c.usage_credits, "{key} = {value}");
        }
    }
    let on = SupervisorConfig {
        usage_credits: true,
        ..full.clone()
    };
    for text in ["true", "\"true\"", "\"yes\"", "\"On\"", "1"] {
        let (c, notes) =
            SupervisorConfig::from_aterm_toml(&format!("[harness]\nusage_credits = {text}\n"));
        assert_eq!(c, on, "{text}");
        assert!(notes.is_empty(), "{text}: {notes:?}");
    }
    for (text, said) in [
        ("false", 0),
        ("\"no\"", 0),
        ("0", 0),
        ("\"never\"", 1),
        ("\"sometimes\"", 1),
        ("3", 1),
        ("[true]", 1),
    ] {
        let (c, notes) =
            SupervisorConfig::from_aterm_toml(&format!("[harness]\nusage_credits = {text}\n"));
        assert_eq!(c, full, "{text}");
        assert_eq!(notes.len(), said, "{text}: {notes:?}");
    }
    assert!(!on.limits(&full), "on is no limit of off");
    assert!(full.limits(&on), "off limits on");
    let mut c = on.clone();
    c.limit("usage_credits");
    assert_eq!(c, full, "its limit is off");
    assert_eq!(
        on.against_default(),
        Written {
            limits: Vec::new(),
            own: vec!["usage_credits"],
        }
    );
    assert!(on.texts().contains(&("usage_credits", "true".to_string())));
    let mut c = full.clone();
    c.set("usage_credits", "true").expect("a switch");
    assert_eq!(c, on);
    let (c, notes) = SupervisorConfig::from_aterm_toml("[theme]\nharness.usage_credits = true\n");
    assert_eq!(c, full);
    assert!(
        notes[0].contains("`theme.harness.usage_credits`"),
        "{notes:?}"
    );
    assert!(
        !read("[harness]\nusage_credits = true\n[theme]\nharness.usage_credits = false\n")
            .usage_credits,
        "a misplaced off limits all the same"
    );
    assert!(
        !read("[a]\nharness.usage_credits = true\n[harness]\nusage_credits = false\n")
            .usage_credits,
        "a table that sorts before `harness` grants nothing either"
    );
    const TYPO: &str = "font_px = \n";
    for (tail, want) in [
        ("[harness]\nusage_credits = true\n", true),
        ("harness = { usage_credits = true }\n", true),
        (
            "[harness]\nusage_credits = false\nusage_credits = true\n",
            false,
        ),
        (
            "[harness]\nusage_credits = true\nusage_credits = false\n",
            false,
        ),
        ("[theme]\nharness.usage_credits = true\n", false),
        ("[[keybind]]\nharness.usage_credits = true\n", false),
        ("[harness]\nnote = \"usage_credits = true\"\n", false),
        ("# harness.usage_credits = true\n", false),
    ] {
        let text = format!("{TYPO}{tail}");
        assert!(
            aterm_toml::from_str::<aterm_toml::Value>(&text).is_err(),
            "the fixture must be one the parser refuses: {text:?}"
        );
        assert_eq!(read(&text).usage_credits, want, "{text:?}");
    }
}

#[test]
fn occupancy_ships_observe_and_only_the_root_table_turns_prompt_on() {
    let full = SupervisorConfig::default();
    assert_eq!(full.occupancy, Occupancy::Observe, "the default is observe");
    assert!(full.occupancy.grant().is_none(), "observe grants nothing");
    assert_eq!(read("").occupancy, Occupancy::Observe);
    assert_eq!(read("[harness]\n").occupancy, Occupancy::Observe);
    assert_eq!(
        SupervisorConfig::default_shown("occupancy").as_deref(),
        Some("observe")
    );
    for (mode, grants, observes) in [
        (Occupancy::Off, false, false),
        (Occupancy::Observe, false, true),
        (Occupancy::Prompt, true, true),
    ] {
        assert_eq!(mode.grant().is_some(), grants, "{mode:?}");
        assert_eq!(mode.observes(), observes, "{mode:?}");
        assert_eq!(Occupancy::parse(mode.word()), Some(mode), "{mode:?}");
    }
    for key in KEYS.iter().filter(|k| **k != "occupancy") {
        for value in ["true", "prompt"] {
            let mut c = full.clone();
            let _ = c.set(key, value);
            assert_eq!(c.occupancy, Occupancy::Observe, "{key} = {value}");
        }
        let mut c = full.clone();
        c.limit(key);
        assert_eq!(c.occupancy, Occupancy::Observe, "{key} at its limit");
    }
    for key in RETIRED_KEYS {
        for value in ["true", "false"] {
            let c = read(&format!("[harness]\n{key} = {value}\n"));
            assert_eq!(c.occupancy, Occupancy::Observe, "{key} = {value}");
        }
    }
    let prompt = SupervisorConfig {
        occupancy: Occupancy::Prompt,
        ..full.clone()
    };
    let off = SupervisorConfig {
        occupancy: Occupancy::Off,
        ..full.clone()
    };
    for text in ["\"prompt\"", "\"Prompt\"", "\" PROMPT \""] {
        let (c, notes) =
            SupervisorConfig::from_aterm_toml(&format!("[harness]\noccupancy = {text}\n"));
        assert_eq!(c, prompt, "{text}");
        assert!(c.occupancy.grant().is_some(), "{text}");
        assert!(notes.is_empty(), "{text}: {notes:?}");
    }
    for (text, want) in [("\"observe\"", &full), ("\"off\"", &off)] {
        let (c, notes) =
            SupervisorConfig::from_aterm_toml(&format!("[harness]\noccupancy = {text}\n"));
        assert_eq!(&c, want, "{text}");
        assert!(notes.is_empty(), "{text}: {notes:?}");
    }
    for (text, want) in [
        ("false", &off),
        ("\"no\"", &off),
        ("0", &off),
        ("\"never\"", &off),
        ("\"none\"", &off),
        ("\"disabled\"", &off),
        ("true", &full),
        ("\"true\"", &full),
        ("\"yes\"", &full),
        ("\"on\"", &full),
        ("\"On\"", &full),
        ("1", &full),
        ("\"sometimes\"", &full),
        ("3", &full),
        ("[\"prompt\"]", &full),
    ] {
        let (c, notes) =
            SupervisorConfig::from_aterm_toml(&format!("[harness]\noccupancy = {text}\n"));
        assert_eq!(&c, want, "{text}");
        assert!(c.occupancy.grant().is_none(), "{text}");
        assert_eq!(notes.len(), 1, "{text}: {notes:?}");
    }
    assert!(!prompt.limits(&full), "prompt is no limit of observe");
    assert!(full.limits(&prompt), "observe limits prompt");
    assert!(off.limits(&full) && off.limits(&prompt), "off limits both");
    assert!(!full.limits(&off), "observe is no limit of off");
    let mut c = prompt.clone();
    c.limit("occupancy");
    assert_eq!(c, off, "its limit is off");
    assert_eq!(
        prompt.against_default(),
        Written {
            limits: Vec::new(),
            own: vec!["occupancy"],
        }
    );
    assert_eq!(
        off.against_default(),
        Written {
            limits: vec!["occupancy: off".to_string()],
            own: Vec::new(),
        }
    );
    assert!(
        prompt
            .texts()
            .contains(&("occupancy", "prompt".to_string()))
    );
    assert!(full.texts().contains(&("occupancy", "observe".to_string())));
    let mut c = full.clone();
    c.set("occupancy", "prompt").expect("a mode");
    assert_eq!(c, prompt);
    let (c, notes) = SupervisorConfig::from_aterm_toml("[theme]\nharness.occupancy = \"prompt\"\n");
    assert_eq!(c, full);
    assert!(notes[0].contains("`theme.harness.occupancy`"), "{notes:?}");
    assert_eq!(
        read("[harness]\noccupancy = \"prompt\"\n[theme]\nharness.occupancy = \"off\"\n").occupancy,
        Occupancy::Off,
        "a misplaced off limits all the same"
    );
    assert_eq!(
        read("[a]\nharness.occupancy = \"prompt\"\n[harness]\noccupancy = \"observe\"\n").occupancy,
        Occupancy::Observe,
        "a table that sorts before `harness` grants nothing either"
    );
    const TYPO: &str = "font_px = \n";
    for (tail, want) in [
        ("[harness]\noccupancy = \"prompt\"\n", Occupancy::Prompt),
        ("harness = { occupancy = \"prompt\" }\n", Occupancy::Prompt),
        (
            "[harness]\noccupancy = \"observe\"\noccupancy = \"prompt\"\n",
            Occupancy::Observe,
        ),
        (
            "[harness]\noccupancy = \"prompt\"\noccupancy = \"observe\"\n",
            Occupancy::Observe,
        ),
        ("[harness]\noccupancy = true\n", Occupancy::Observe),
        ("[harness]\noccupancy = false\n", Occupancy::Off),
        (
            "[theme]\nharness.occupancy = \"prompt\"\n",
            Occupancy::Observe,
        ),
        (
            "[[keybind]]\nharness.occupancy = \"prompt\"\n",
            Occupancy::Observe,
        ),
        (
            "[harness]\nnote = \"occupancy = prompt\"\n",
            Occupancy::Observe,
        ),
        ("# harness.occupancy = \"prompt\"\n", Occupancy::Observe),
    ] {
        let text = format!("{TYPO}{tail}");
        assert!(
            aterm_toml::from_str::<aterm_toml::Value>(&text).is_err(),
            "the fixture must be one the parser refuses: {text:?}"
        );
        assert_eq!(read(&text).occupancy, want, "{text:?}");
    }
}

/// THE OWNER'S TABLES ARE NO KEYS OF THE POLICY: at the root they are the owner-table
/// reader's, so this reader leaves them with no note and no change; below another table's
/// header (TOML or not) they grant nothing and leave one closed note, never `unknown key`.
/// NEGATIVE CONTROL: [`SupervisorConfig::set_value`] called for them, as the root loop did
/// before, yields the `unknown key` note.
#[test]
fn the_owner_tables_are_no_policy_keys_at_the_root_or_below_another_header() {
    let tables = "[harness.occupancy_source.trust]\nkind = \"git-ref\"\nrepo = \"$HOME/trust\"\n\
                  [[harness.occupancy_rule]]\nname = \"r\"\nwhen = \"git(trust).moved\"\n\
                  text = \"Re-read.\"\n";
    let (c, notes) =
        SupervisorConfig::from_aterm_toml(&format!("[harness]\nfacts = true\n{tables}"));
    assert_eq!(c, SupervisorConfig::default());
    assert!(notes.is_empty(), "{notes:?}");
    let misplaced = format!("[theme]\n{}", tables.replace("[harness", "[theme.harness"));
    let (c, notes) = SupervisorConfig::from_aterm_toml(&misplaced);
    assert_eq!(c, SupervisorConfig::default());
    assert_eq!(notes.len(), 2, "{notes:?}");
    for (note, key) in notes.iter().zip(["occupancy_rule", "occupancy_source"]) {
        assert_eq!(
            note,
            &misplaced_note("theme", key, false, Some(OWNER_TABLES_ROOT.into()))
        );
        assert!(!is_unknown_key_note(note), "{note}");
    }
    let broken = "font_px = \n[theme]\nharness.occupancy_rule = [1]\n[harness]\n\
                  occupancy_source = { a = 1 }\n";
    assert!(aterm_toml::from_str::<aterm_toml::Value>(broken).is_err());
    let (c, notes) = SupervisorConfig::from_aterm_toml(broken);
    assert_eq!(c, SupervisorConfig::default());
    assert_eq!(
        notes.len(),
        2,
        "the parse note and the misplaced one: {notes:?}"
    );
    assert!(notes[1].contains(OWNER_TABLES_ROOT), "{notes:?}");

    let doc: aterm_toml::Value = aterm_toml::from_str(tables).expect("TOML");
    let root = doc
        .get("harness")
        .and_then(aterm_toml::Value::as_table)
        .expect("root");
    for key in OWNER_TABLES {
        let refused = SupervisorConfig::default()
            .set_value(key, root.get(key).expect("written"))
            .expect_err("no key of the policy");
        assert!(is_unknown_key_note(&refused), "{refused}");
    }
}

/// THE OWNER'S KEYS READ EQUAL EXACTLY WHERE TWO TEXTS AGREE ON THEM: another key, a comment,
/// spacing and order leave the reading equal (the NEGATIVE CONTROL of every difference);
/// the mode, a source table, a rule, an owner key dropped, a misplaced limit, and an
/// invalid file's lines filed under one each tell the texts apart.
#[test]
fn owner_keys_differ_where_the_owners_keys_do_and_nowhere_else() {
    let base = "font_px = 12.0\n[harness]\noccupancy = \"observe\"\nfacts = true\n";
    let same = |text: &str| OwnerKeys::of(base) == OwnerKeys::of(text);
    for text in [
        base,
        "font_px = 16.0\n[harness]\noccupancy = \"observe\"\nfacts = true\n",
        "font_px = 12.0\n# a note\n[harness]\nfacts = true\noccupancy   =   \"observe\" # mine\n",
        "font_px = 12.0\n[harness]\noccupancy = \"observe\"\nfacts = false\nautopilot = \"land\"\n",
        "font_px = 12.0\n[harness]\noccupancy = \"observe\"\nfacts = true\n[theme.harness]\nfacts = false\n",
    ] {
        assert!(same(text), "{text:?}");
    }
    for text in [
        "font_px = 12.0\n[harness]\noccupancy = \"prompt\"\nfacts = true\n",
        "font_px = 12.0\nharness = { occupancy = \"prompt\", facts = true }\n",
        "font_px = 12.0\n[harness]\nfacts = true\n",
        "font_px = 12.0\n[harness]\noccupancy = \"observe\"\nfacts = true\n\
         [harness.occupancy_source.trust]\nkind = \"git-ref\"\n",
        "font_px = 12.0\n[harness]\noccupancy = \"observe\"\nfacts = true\n\
         [[harness.occupancy_rule]]\nname = \"trust-base\"\n",
        "font_px = 12.0\n[harness]\noccupancy = \"observe\"\nfacts = true\n\
         [theme.harness]\noccupancy = \"off\"\n",
        "font_px = \n[harness]\noccupancy = \"observe\"\nfacts = true\n",
    ] {
        assert!(!same(text), "{text:?}");
    }

    const TYPO: &str = "font_px = \n";
    let broken = format!("{TYPO}[harness]\noccupancy = \"observe\"\n");
    assert!(aterm_toml::from_str::<aterm_toml::Value>(&broken).is_err());
    let reads = |text: &str| OwnerKeys::of(text);
    assert_eq!(
        reads(&broken),
        reads(&format!(
            "{TYPO}[harness]\noccupancy = \"observe\"\nfacts = false\n"
        ))
    );
    let with_rule = format!("{broken}[[harness.occupancy_rule]]\nname = \"trust-base\"\n");
    assert_eq!(
        read(&broken).occupancy,
        read(&with_rule).occupancy,
        "the mode alone cannot tell these apart"
    );
    assert_ne!(reads(&broken), reads(&with_rule), "the lines do");
    assert_ne!(
        reads(&broken),
        reads(&format!("{TYPO}[harness]\noccupancy = \"prompt\"\n"))
    );
}

/// THE STUCK FACT'S KEY SHIPS OFF, AND ONLY THE ROOT TABLE GRANTS IT: no
/// grant by default; the root table's `true` grants it under every
/// occupancy mode, and `facts = false` (a limit, in any table) takes it
/// away; a misplaced table's `true`, a repeat that says it again, or any
/// other key's `true` or limit grants nothing, and the grant it makes is
/// no occupancy `prompt`.
#[test]
fn the_stuck_break_note_ships_on_and_any_table_s_off_withdraws_it() {
    let full = SupervisorConfig::default();
    assert!(full.facts && full.stuck_break_note);
    assert!(full.stuck_break_grant().is_some());
    for mode in Occupancy::ALL {
        let on = read(&format!("[harness]\noccupancy = \"{}\"\n", mode.word()));
        assert!(on.stuck_break_grant().is_some(), "{mode:?}");
        assert_eq!(on.occupancy.grant().is_some(), mode == Occupancy::Prompt);
    }
    for text in [
        "[harness]\nstuck_break_note = false\n",
        "[harness]\nfacts = false\n",
        "[theme]\nharness.facts = false\n",
        "[theme]\nharness.stuck_break_note = false\n",
    ] {
        assert!(read(text).stuck_break_grant().is_none(), "{text:?}");
    }
    assert!(
        read("[harness]\nstuck_break_note = \"sometimes\"\n")
            .stuck_break_grant()
            .is_some(),
        "an unreadable value keeps the default"
    );
    for key in KEYS
        .iter()
        .filter(|k| !matches!(**k, "stuck_break_note" | "facts"))
    {
        let mut c = full.clone();
        c.limit(key);
        assert!(c.stuck_break_grant().is_some(), "{key} at its limit");
    }
    for key in ["stuck_break_note", "facts"] {
        let mut c = full.clone();
        c.limit(key);
        assert!(c.limits(&full) && c.stuck_break_grant().is_none(), "{key}");
        assert!(!full.limits(&c), "{key}");
    }
}

#[test]
fn agent_texts_are_each_agents_switch_and_read_back() {
    let full = SupervisorConfig::default();
    let supervisable: Vec<String> = Program::ALL
        .into_iter()
        .filter(|program| program.harness_table())
        .map(|program| format!("{}.enabled", program.name()))
        .collect();
    let keys: Vec<String> = full.agent_texts().into_iter().map(|(k, _)| k).collect();
    assert_eq!(keys, supervisable);
    assert!(full.agent_texts().iter().all(|(_, text)| text == "true"));
    for key in &supervisable {
        let mut off = full.clone();
        off.set(key, "false").expect(key);
        let texts = off.agent_texts();
        for (k, text) in &texts {
            assert_eq!(text == "false", k == key, "{key}: {texts:?}");
            assert_eq!(
                SupervisorConfig::default_shown(k).as_deref(),
                Some("true"),
                "{k}"
            );
        }
        let mut back = full.clone();
        for (k, text) in &texts {
            back.set(k, text).expect(k);
        }
        assert_eq!(back, off, "{key}");
    }
    assert!(full.withheld_texts().is_empty());
    let (retired, _) = SupervisorConfig::from_aterm_toml("[harness]\nrm_breaker = false\n");
    assert_eq!(
        retired.withheld_texts(),
        [("rm_breaker", "false".to_string())]
    );
}

#[test]
fn texts_are_the_keys_and_read_back_as_the_policy() {
    let full = SupervisorConfig::default();
    let keys: Vec<_> = full.texts().iter().map(|(k, _)| *k).collect();
    assert_eq!(keys, KEYS, "one text per key, in KEYS order");
    for key in KEYS {
        let mut limited = full.clone();
        limited.limit(key);
        for want in [&full, &limited] {
            let (_, text) = want.texts().into_iter().find(|(k, _)| k == key).unwrap();
            let mut c = full.clone();
            c.set(key, &text).expect(key);
            assert_eq!(&c, want, "{key} = {text:?}");
        }
    }
    for key in KEYS {
        assert_eq!(
            SupervisorConfig::default_shown(key).is_none(),
            *key == "rules_file",
            "{key}"
        );
    }
    assert_eq!(SupervisorConfig::default_shown("auto_reads"), None);
}

#[test]
fn rebuild_gives_back_the_policy_its_texts_wrote() {
    let full = SupervisorConfig::default();
    let mut policies = vec![full.clone()];
    for key in KEYS {
        policies.push(SupervisorConfig::moved_off_default(key));
    }
    for (agent, _) in full.agent_texts() {
        let mut c = full.clone();
        c.set(&agent, "false").expect("an agent's own switch");
        assert_ne!(c, full, "{agent}");
        policies.push(c);
    }
    for rule in 0..Withheld::default().rules_mut().len() {
        let mut c = full.clone();
        *c.withheld.rules_mut()[rule].1 = true;
        assert_eq!(c.approve, Approve::All, "withheld alone, approve uncapped");
        policies.push(c);
    }
    for want in &policies {
        let texts = want.policy_texts();
        assert_eq!(
            SupervisorConfig::rebuild(&texts),
            Ok(Rebuilt {
                policy: want.clone(),
                supplied: Vec::new(),
                retired: Vec::new(),
                refused: Vec::new(),
            }),
            "{texts:?}"
        );
    }
}

#[test]
fn rebuild_supplies_what_an_older_builds_texts_did_not_name() {
    let mut written = SupervisorConfig::default();
    written.set("approve", "safe").expect("approve");
    written.set("land_at", "18:00").expect("land_at");
    written.set("codex.enabled", "false").expect("codex");
    let mut older = written.policy_texts();
    older.keys.retain(|(k, _)| k != "land_at");
    older.agents.retain(|(k, _)| k != "codex.enabled");
    let back = SupervisorConfig::rebuild(&older).expect("an older build's texts");
    assert_eq!(back.supplied, ["land_at", "codex.enabled"]);
    assert!(back.retired.is_empty(), "{back:?}");
    assert_eq!(
        back.policy,
        SupervisorConfig {
            land_at: None,
            off: Vec::new(),
            ..written
        },
        "what the texts did not say is the default; what they said stands"
    );
}

#[test]
fn rebuild_reads_a_key_retired_since_as_the_owners_file_does() {
    let texts = SupervisorConfig::default().policy_texts();
    let mut asked_default = texts.clone();
    asked_default
        .keys
        .insert(0, ("rm_breaker".into(), "true".into()));
    let back = SupervisorConfig::rebuild(&asked_default).expect("a retired key");
    assert_eq!(back.policy, SupervisorConfig::default());
    assert!(back.supplied.is_empty(), "{back:?}");
    assert_eq!(back.retired.len(), 1, "{back:?}");
    assert!(
        back.retired[0].starts_with("harness.rm_breaker: "),
        "{back:?}"
    );

    let mut limited = texts;
    limited
        .keys
        .insert(0, ("rm_breaker".into(), "false".into()));
    let back = SupervisorConfig::rebuild(&limited).expect("a retired key");
    assert_eq!(
        back.policy.approve,
        Approve::Safe,
        "capped after `approve=all`"
    );
    assert!(back.policy.withheld.rm_breaker);
    assert_eq!(back.retired.len(), 1, "{back:?}");
}

/// AN OLDER BUILD'S SWITCH FOR THE RETIRED GEMINI CLI SETS NOTHING AND IS SAID: texts an
/// older build wrote name `gemini.enabled`; they rebuild to the policy without it, the note
/// names agy, and no live agent's switch moves. NEGATIVE CONTROL: an agent no build ever had
/// is still a newer build's, refused.
#[test]
fn rebuild_reads_a_retired_agents_switch_as_nothing_and_names_its_replacement() {
    let mut written = SupervisorConfig::default();
    written.set("codex.enabled", "false").expect("codex");
    for value in ["true", "false"] {
        let mut older = written.policy_texts();
        older.agents.push(("gemini.enabled".into(), value.into()));
        let back = SupervisorConfig::rebuild(&older).expect("an older build's texts");
        assert_eq!(back.policy, written, "{value}: nothing set");
        assert!(back.supplied.is_empty(), "{value}: {back:?}");
        assert_eq!(back.retired.len(), 1, "{value}: {back:?}");
        assert!(
            back.retired[0].starts_with("harness.gemini.enabled: ")
                && back.retired[0].contains(aterm_phase::RETIRED_AGENT),
            "{value}: {back:?}"
        );
        let near = SupervisorConfig::rebuild_nearest(&older).expect("nearest too");
        assert_eq!(near.policy, written, "{value}");
    }
    let mut newer = written.policy_texts();
    newer.agents.push(("geminix.enabled".into(), "true".into()));
    let e = SupervisorConfig::rebuild(&newer).expect_err("no build had it");
    assert!(e.contains("a newer build"), "{e}");
}

#[test]
fn rebuild_refuses_texts_this_build_cannot_read() {
    let texts = SupervisorConfig::default().policy_texts();
    let refused = |edit: &dyn Fn(&mut PolicyTexts), names: &str| {
        let mut t = texts.clone();
        edit(&mut t);
        let e = SupervisorConfig::rebuild(&t).expect_err(names);
        assert!(e.contains(names), "{names}: {e}");
    };
    refused(
        &|t| t.keys.push(("approve".into(), "safe".into())),
        "more than once",
    );
    refused(&|t| t.keys.push(("no_such".into(), "1".into())), "no_such");
    refused(
        &|t| t.keys.push(("no_such".into(), "1".into())),
        "a newer build",
    );
    refused(
        &|t| {
            for (k, v) in &mut t.keys {
                if k == "approve" {
                    *v = "maybe".into();
                }
            }
        },
        "approve",
    );
    refused(
        &|t| t.agents.push(("nobody.enabled".into(), "true".into())),
        "nobody.enabled",
    );
    refused(
        &|t| t.withheld.push(("rm_breaker".into(), "true".into())),
        "written false",
    );
    refused(
        &|t| t.withheld.push(("trust_dialog".into(), "false".into())),
        "trust_dialog",
    );
}

#[test]
fn rebuild_nearest_reads_a_refused_value_as_the_owners_file_and_names_it() {
    let mut ran = SupervisorConfig::default();
    ran.set("approve", "safe").expect("approve");
    ran.autocompact_offer = Some(450);
    let texts = ran.policy_texts();
    let e = SupervisorConfig::rebuild(&texts).expect_err("450k is on no ladder");
    assert!(e.contains("autocompact_offer"), "{e}");
    let back = SupervisorConfig::rebuild_nearest(&texts).expect("the nearest policy");
    assert_eq!(
        back.policy,
        SupervisorConfig {
            autocompact_offer: Some(AUTOCOMPACT_DEFAULT_K),
            ..ran
        },
        "the refused value keeps its default; what else was said stands"
    );
    assert_eq!(back.refused.len(), 1, "{back:?}");
    assert!(
        back.refused[0].starts_with("harness.autocompact_offer: ")
            && back.refused[0].contains("450k")
            && back.refused[0].ends_with("; this build read autocompact_offer=400k"),
        "what was refused and what was read in its place: {back:?}"
    );
    assert!(
        back.supplied.is_empty() && back.retired.is_empty(),
        "{back:?}"
    );
    let mut newer = texts;
    newer.keys.push(("no_such".into(), "1".into()));
    let e = SupervisorConfig::rebuild_nearest(&newer).expect_err("a newer build's key");
    assert!(e.contains("no_such"), "{e}");
}

#[test]
fn against_default_names_each_limit_and_never_a_text_as_one() {
    let full = SupervisorConfig::default();
    assert_eq!(full.against_default(), Written::default(), "full power");
    for key in KEYS {
        let mut c = full.clone();
        c.limit(key);
        let w = c.against_default();
        assert!(w.own.is_empty(), "{key}: {w:?}");
        if c == full {
            assert!(w.limits.is_empty(), "{key}: {w:?}");
        } else {
            assert_eq!(w.limits.len(), 1, "{key}: {w:?}");
            assert!(w.limits[0].starts_with(&format!("{key}: ")), "{w:?}");
        }
    }
    let w = read(
        "[harness]\napprove = \"safe\"\ncontinue = false\ncontinue_per_hour = 6\n\
         answer_text = \"ask me later\"\nrules_file = \"/r.md\"\n",
    )
    .against_default();
    assert_eq!(
        w.limits,
        ["approve: safe", "continue: off", "continue_per_hour: 6"]
    );
    assert_eq!(w.own, ["answer_text", "rules_file"]);
    let w = read("[harness]\nhuman_grace_s = 30\n").against_default();
    assert!(w.limits.is_empty(), "{w:?}");
    assert_eq!(w.own, ["human_grace_s"]);
    assert_eq!(
        read("[harness]\nhuman_grace_s = 600\n")
            .against_default()
            .limits,
        ["human_grace_s: 600"]
    );
    let roots = "trust_roots = []\n";
    assert!(read(&format!("[harness]\n{roots}")).against_default() == Written::default());
    let w = read(&format!("[harness]\napprove = \"safe\"\n{roots}")).against_default();
    assert_eq!(
        (w.limits, w.own),
        (vec!["approve: safe".to_string()], vec![])
    );
    for more in [
        "trust_roots = [\"~/src/app*\"]\n",
        "trust_roots = [\"~/src/app*\", \"/srv/work\"]\n",
    ] {
        let w = read(&format!("[harness]\napprove = \"safe\"\n{more}")).against_default();
        assert_eq!(
            (w.limits.len(), w.own.clone()),
            (1, vec!["trust_roots"]),
            "{more}"
        );
        assert!(read(&format!("[harness]\n{more}")).against_default() == Written::default());
    }
}

#[test]
fn a_retired_key_written_false_is_read_as_the_limit_it_named() {
    let full = SupervisorConfig::default();
    let one = |text: &str| {
        let (c, notes) = SupervisorConfig::from_aterm_toml(text);
        assert_eq!(notes.len(), 1, "{text}: {notes:?}");
        assert!(
            is_retired_key_note(&notes[0]) && !is_unknown_key_note(&notes[0]),
            "{notes:?}"
        );
        (c, notes[0].clone())
    };
    let (c, note) = one("[harness]
approve_all = false
");
    assert_eq!(
        c,
        SupervisorConfig {
            approve: Approve::Safe,
            ..full.clone()
        }
    );
    assert!(note.contains("approve = \"safe\""), "{note}");
    let (c, _) = one("[harness]
auto_reads = false
");
    assert_eq!(c.approve, Approve::None);
    let (c, _) = one("[harness]
rm_breaker = false
");
    assert_eq!((c.approve, c.withheld.rm_breaker), (Approve::Safe, true));
    let (c, _) = one("[harness]
read_outside_cwd = false
");
    assert_eq!(
        (c.approve, c.withheld.read_outside_cwd),
        (Approve::Safe, true)
    );
    let (c, _) = one("[harness]
trust_roots = [\"~/src/app*\"]
trust_dialog = false
");
    assert_eq!((c.approve, c.trust_roots.len()), (Approve::Safe, 0));
    assert!(!retired_still_limits("rate_nudge", Some(false)));
    assert!(
        !retired_still_limits("approve", Some(false)),
        "no retired key"
    );
    for key in RETIRED_KEYS.iter().filter(|k| **k != "rate_nudge") {
        for text in [
            format!("[harness]\n{key} = false\n"),
            format!("[harness]\n{key} = \"no\"\n"),
            format!("[theme]\nharness.{key} = false\n"),
        ] {
            let c = read(&text);
            assert!(c.limits(&full), "{text}: {c:?}");
        }
        let (c, _) = one(&format!("[harness]\n{key} = true\n"));
        assert_eq!(c, full, "{key} = true changes nothing");
        assert!(retired_still_limits(key, Some(false)), "{key}");
        assert!(!retired_still_limits(key, Some(true)), "{key}");
        assert!(!retired_still_limits(key, None), "{key}");
        for text in [
            format!("[harness]\n{key} = \"maybe\"\n"),
            format!("[harness]\n{key} = 2.5\n"),
            format!("[harness]\n{key} = [false]\n"),
        ] {
            let (c, _) = one(&text);
            assert_eq!(c, full, "{text}");
        }
        assert!(retired_key_note(key, None).is_some(), "{key}");
        for off in ["false", "\"no\"", "0", "\"disabled\""] {
            let (c, note) = one(&format!("[harness]\n{key} = {off}\n"));
            assert!(c.limits(&full), "{key} = {off}");
            assert_eq!(
                retired_key_note_text(key, Some(off.trim_matches('"'))).as_deref(),
                Some(note.as_str()),
                "{key} = {off}"
            );
        }
        assert_eq!(
            retired_key_note(key, Some(false)).as_deref(),
            Some(one(&format!("[harness]\n{key} = false\n")).1.as_str()),
            "the config language says the reader's words"
        );
    }
    let c = read("[harness]\napprove = \"none\"\napprove_all = false\n");
    assert_eq!(c.approve, Approve::None);
    for text in [
        "[harness]\napprove_all = false\napprove = \"all\"\n",
        "[harness]\napprove = \"all\"\napprove_all = false\n",
        "[harness]\napprove_all = false\napprove = \"all\"\n[broken\n",
    ] {
        assert_eq!(read(text).approve, Approve::Safe, "{text}");
    }
    assert_eq!(
        read("[harness]\nrm_breaker = false\n")
            .against_default()
            .limits,
        ["approve: safe", "rm_breaker: off"]
    );
    let (c, notes) = SupervisorConfig::from_aterm_toml("[harness]\napprove_al = false\n");
    assert_eq!(c, full);
    assert!(is_unknown_key_note(&notes[0]), "{notes:?}");
    assert_eq!(retired_key_note("approve_al", Some(false)), None);
}

#[test]
fn a_retired_codex_key_reads_and_changes_nothing() {
    let full = SupervisorConfig::default();
    for text in [
        "[harness]\nrate_nudge = false\n",
        "[harness]\nrate_nudge = true\n",
        "[harness]\nrate_nudge = \"no\"\n",
        "[harness]\nrate_nudge = \"maybe\"\n",
        "[harness]\nrate_nudge = 2.5\n",
        "[theme]\nharness.rate_nudge = false\n",
    ] {
        let (c, notes) = SupervisorConfig::from_aterm_toml(text);
        assert_eq!(c, full, "{text}");
        assert_eq!(notes.len(), 1, "{text}: {notes:?}");
        assert!(
            is_retired_key_note(&notes[0]) && !is_unknown_key_note(&notes[0]),
            "{text}: {notes:?}"
        );
        assert!(notes[0].contains("changes nothing"), "{notes:?}");
    }
    let (_, notes) = SupervisorConfig::from_aterm_toml("[harness]\nrate_nudge = false\n");
    assert_eq!(
        retired_key_note("rate_nudge", Some(false)).as_ref(),
        notes.first()
    );
    assert!(!KEYS.contains(&"rate_nudge"));
    assert!(full.texts().iter().all(|(k, _)| *k != "rate_nudge"));
    let mut c = full.clone();
    c.limit("rate_nudge");
    assert_eq!(c, full, "its limit is nothing");
    let (_, notes) = SupervisorConfig::from_aterm_toml("[harness]\nrate_nudg = false\n");
    assert!(is_unknown_key_note(&notes[0]), "{notes:?}");
}

#[test]
fn a_disk_knob_under_harness_is_the_disk_readers_not_an_unknown_key() {
    let text = "[harness]\ndisk.auto_free_gib = 0\n";
    let (_, notes) = SupervisorConfig::from_aterm_toml(text);
    assert!(!notes.iter().any(|n| is_unknown_key_note(n)), "{notes:?}");
    let reader: Vec<String> = crate::harness::cli::disk_faults(text)
        .into_iter()
        .map(|f| f.note)
        .collect();
    assert!(!reader.is_empty(), "the disk reader names it");
    for note in &reader {
        assert!(notes.contains(note), "{note:?} in {notes:?}");
    }
    let (_, notes) = SupervisorConfig::from_aterm_toml("[harness]\nnot_a_key = 1\n");
    assert_eq!(
        notes.iter().filter(|n| is_unknown_key_note(n)).count(),
        1,
        "{notes:?}"
    );
}

#[test]
fn a_misread_question_switch_changes_nothing() {
    for text in [
        "[harness]\nanswer_questions = \"sometimes\"\n",
        "[harness]\nanswer_questions = 2.5\n",
    ] {
        assert!(read(text).answer_questions, "{text}");
    }
    assert!(!read("[harness]\nanswer_questions = false\n").answer_questions);
}

/// A person's `aterm.toml` in a dotfiles checkout or home-manager's store is their config,
/// here as in the window: a link there does not put the defaults in its place.
#[cfg(unix)]
#[test]
fn from_path_reads_through_a_linked_aterm_toml() {
    let dir = crate::supervise::ScratchDir::new("harness-cfg", "from-path-link");
    let real = dir.join("dotfiles.toml");
    aterm_uds::atomic_file::write(&real, b"[harness]\nenabled = false\n", 0o600).unwrap();
    let link = dir.join("aterm.toml");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let (config, notes) = SupervisorConfig::from_path(Some(&link));
    assert!(!config.enabled, "{notes:?}");
    assert!(notes.is_empty(), "{notes:?}");
}

#[cfg(unix)]
#[test]
fn an_untrusted_disabled_config_is_refused_without_enabling_the_harness() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = crate::supervise::ScratchDir::new("harness-cfg", "untrusted-disabled");
    let real = dir.join("dotfiles.toml");
    std::fs::write(&real, "[harness]\nenabled = false\n").unwrap();
    std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o664)).unwrap();
    let link = dir.join("aterm.toml");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    for path in [&real, &link] {
        let (config, notes) = SupervisorConfig::from_path(Some(path));
        assert!(!config.enabled, "a refused file cannot enable automation");
        assert!(!notes.is_empty(), "an untrusted config must be refused");
    }
}

#[test]
fn an_unreadable_config_does_not_enable_the_harness() {
    let dir = crate::supervise::ScratchDir::new("harness-cfg", "unreadable");
    let (config, notes) = SupervisorConfig::from_path(Some(&dir));
    assert!(
        !config.enabled,
        "unreadable config cannot enable automation"
    );
    assert!(!notes.is_empty());
}

#[test]
fn from_path_reads_the_file_and_a_missing_one_is_the_defaults() {
    let dir = crate::supervise::ScratchDir::new("harness-cfg", "from-path");
    let toml = dir.join("aterm.toml");
    assert_eq!(
        SupervisorConfig::from_path(Some(&toml)),
        (SupervisorConfig::default(), Vec::new())
    );
    assert_eq!(
        SupervisorConfig::from_path(None).0,
        SupervisorConfig::default()
    );
    aterm_uds::atomic_file::write(&toml, b"[harness]\nenabled = false\n", 0o600).expect("write");
    assert!(!SupervisorConfig::from_path(Some(&toml)).0.enabled);
    aterm_uds::atomic_file::write(&toml, b"# caf\xe9\n[harness]\nenabled = false\n", 0o600)
        .expect("write");
    assert!(!SupervisorConfig::from_path(Some(&toml)).0.enabled);
    aterm_uds::atomic_file::write(&toml, b"# caf\xe9\n[harness]\nenabled = true\n", 0o600)
        .expect("write");
    assert!(SupervisorConfig::from_path(Some(&toml)).0.enabled);
    let (c, notes) = SupervisorConfig::from_path(Some(&dir));
    assert!(!c.enabled);
    assert!(!notes.is_empty());
}

#[test]
fn the_memory_ceiling_reads_auto_off_or_gibibytes_and_limits_to_off() {
    use super::super::memory::MemoryCeiling;
    assert_eq!(read("").memory_ceiling, MemoryCeiling::Auto);
    assert_eq!(
        read("[harness]\nmemory_ceiling = 48\n").memory_ceiling,
        MemoryCeiling::Gib(48)
    );
    assert_eq!(
        read("[harness]\nmemory_ceiling = \"64GB\"\n").memory_ceiling,
        MemoryCeiling::Gib(64)
    );
    assert_eq!(
        read("[harness]\nmemory_ceiling = \"off\"\n").memory_ceiling,
        MemoryCeiling::Off
    );
    let (c, notes) = SupervisorConfig::from_aterm_toml("[harness]\nmemory_ceiling = \"lots\"\n");
    assert_eq!(
        c.memory_ceiling,
        MemoryCeiling::Auto,
        "no setting: the default"
    );
    assert!(
        notes.iter().any(|n| n.contains("harness.memory_ceiling")),
        "{notes:?}"
    );
    let (c, notes) = SupervisorConfig::from_aterm_toml("[harness]\nmemory_ceiling = 1.5\n");
    assert_eq!(
        c.memory_ceiling,
        MemoryCeiling::Auto,
        "a float: the default"
    );
    assert!(
        notes.iter().any(|n| n.contains("harness.memory_ceiling")),
        "{notes:?}"
    );
    let (c, notes) = SupervisorConfig::from_aterm_toml("[harness]\nmemory_ceiling = \"no\"\n");
    assert_eq!(c.memory_ceiling, MemoryCeiling::Off, "{notes:?}");
    let c = read("[theme]\nharness.memory_ceiling = \"off\"\n");
    assert_eq!(c.memory_ceiling, MemoryCeiling::Off);
    let c = read("[harness]\nmemory_ceiling = 64\n[theme]\nharness.memory_ceiling = 8\n");
    assert_eq!(c.memory_ceiling, MemoryCeiling::Gib(64));
    let c = read("[harness]\nmemory_ceiling = 8\n[theme]\nharness.memory_ceiling = 64\n");
    assert_eq!(
        c.memory_ceiling,
        MemoryCeiling::Gib(64),
        "a higher one limits"
    );
    let w = read("[harness]\nmemory_ceiling = \"off\"\n").against_default();
    assert_eq!(w.limits, ["memory_ceiling: off"]);
    let w = read("[harness]\nmemory_ceiling = 48\n").against_default();
    assert_eq!(w.own, ["memory_ceiling"]);
    assert_eq!(
        SupervisorConfig::default_shown("memory_ceiling").as_deref(),
        Some("auto")
    );
}

#[test]
fn question_priority_takes_its_three_words_and_a_refused_one_changes_nothing() {
    let full = SupervisorConfig::default();
    assert_eq!(full.question_priority, PriorityWord::Recommended);
    assert_eq!(
        SupervisorConfig::default_shown("question_priority").as_deref(),
        Some("recommended")
    );
    for (value, want) in [
        ("recommended", PriorityWord::Recommended),
        ("long-term", PriorityWord::LongTerm),
        ("short-term", PriorityWord::ShortTerm),
        ("  Long-Term ", PriorityWord::LongTerm),
        ("SHORT-TERM", PriorityWord::ShortTerm),
    ] {
        let (c, notes) = SupervisorConfig::from_aterm_toml(&format!(
            "[harness]\nquestion_priority = \"{value}\"\n"
        ));
        assert!(notes.is_empty(), "{value:?}: {notes:?}");
        assert_eq!(c.question_priority, want, "{value:?}");
        assert!(c.answer_questions, "{value:?}");
        let w = c.against_default();
        assert!(w.limits.is_empty(), "{value:?}: HOW is no limit: {w:?}");
        assert_eq!(
            w.own.contains(&"question_priority"),
            want != PriorityWord::Recommended,
            "{value:?}: the owner's own value"
        );
    }
    for text in [
        "[harness]\nquestion_priority = \"long_term\"\n",
        "[harness]\nquestion_priority = \"ask\"\n",
        "[harness]\nquestion_priority = \"land\"\n",
        "[harness]\nquestion_priority = true\n",
        "[harness]\nquestion_priority = 2.5\n",
        "font_px = \n[harness]\nquestion_priority = \"sometimes\"\n",
    ] {
        let (c, notes) = SupervisorConfig::from_aterm_toml(text);
        assert_eq!(c, full, "{text:?}: changes nothing");
        let about: Vec<&String> = notes
            .iter()
            .filter(|n| n.starts_with("harness.question_priority:"))
            .collect();
        assert_eq!(about.len(), 1, "{text:?}: {notes:?}");
    }
    let c = read("[theme]\nharness.question_priority = \"long-term\"\n");
    assert_eq!(c, full);
}

#[test]
fn land_at_is_a_known_time_the_reader_validates_and_offers() {
    let full = SupervisorConfig::default();
    assert_eq!(full.land_at, None, "no time offered by default");
    assert_eq!(
        KEYS[KEYS.iter().position(|k| *k == "autopilot").unwrap() + 1],
        "land_at"
    );
    assert_eq!(
        SupervisorConfig::default_shown("land_at").as_deref(),
        Some("none")
    );
    for (value, want) in [
        ("18:00", 18 * 60),
        ("00:00", 0),
        ("23:59", 23 * 60 + 59),
        (" 07:05 ", 7 * 60 + 5),
    ] {
        let (c, notes) =
            SupervisorConfig::from_aterm_toml(&format!("[harness]\nland_at = \"{value}\"\n"));
        assert!(notes.is_empty(), "{value:?}: {notes:?}");
        assert_eq!(c.land_at.map(LandAt::minute), Some(want), "{value:?}");
        let w = c.against_default();
        assert!(
            w.limits.is_empty(),
            "{value:?}: an offer is no limit: {w:?}"
        );
        assert_eq!(w.own, ["land_at"], "{value:?}: the owner's own value");
        let (_, text) = c
            .texts()
            .into_iter()
            .find(|(k, _)| *k == "land_at")
            .unwrap();
        assert_eq!(text, value.trim(), "EFFECTIVE CONFIGURATION lists it");
        let mut back = full.clone();
        back.set("land_at", &text).unwrap();
        assert_eq!(back, c, "{value:?}: read back");
    }
    for text in [
        "[harness]\nland_at = \"\"\n",
        "[harness]\nland_at = \"off\"\n",
        "[harness]\nland_at = false\n",
    ] {
        let (c, notes) = SupervisorConfig::from_aterm_toml(text);
        assert!(notes.is_empty(), "{text:?}: {notes:?}");
        assert_eq!(c, full, "{text:?}: no time offered");
    }
    for text in [
        "[harness]\nland_at = \"24:00\"\n",
        "[harness]\nland_at = \"18:60\"\n",
        "[harness]\nland_at = \"7:00\"\n",
        "[harness]\nland_at = \"18h\"\n",
        "[harness]\nland_at = \"+2h\"\n",
        "[harness]\nland_at = 18\n",
        "[harness]\nland_at = true\n",
        "font_px = \n[harness]\nland_at = \"tomorrow\"\n",
    ] {
        let (c, notes) = SupervisorConfig::from_aterm_toml(text);
        assert_eq!(c, full, "{text:?}: keeps the default");
        let about: Vec<&String> = notes
            .iter()
            .filter(|n| n.starts_with("harness.land_at:"))
            .collect();
        assert_eq!(about.len(), 1, "{text:?}: {notes:?}");
        assert!(
            !notes.iter().any(|n| is_unknown_key_note(n)),
            "{text:?}: a known key: {notes:?}"
        );
    }
    let c = read("[theme]\nharness.land_at = \"18:00\"\n");
    assert_eq!(c, full);
    let (_, notes) = SupervisorConfig::from_aterm_toml("[harness]\nland_by = \"18:00\"\n");
    assert!(notes.iter().any(|n| is_unknown_key_note(n)), "{notes:?}");
    assert_eq!(LandAt::of_minute(1440), None);
    assert_eq!(
        LandAt::of_minute(61).map(LandAt::text).as_deref(),
        Some("01:01")
    );
}

#[test]
fn answer_questions_false_wins_over_any_priority_in_either_order() {
    use super::super::run::SuperviseOpts;
    for text in [
        "[harness]\nanswer_questions = false\nquestion_priority = \"long-term\"\n",
        "[harness]\nquestion_priority = \"long-term\"\nanswer_questions = false\n",
    ] {
        let (c, notes) = SupervisorConfig::from_aterm_toml(text);
        assert!(notes.is_empty(), "{notes:?}");
        assert!(!c.answer_questions, "{text:?}");
        assert_eq!(c.question_priority, PriorityWord::LongTerm);
        let o = SuperviseOpts::hosted_with(&c);
        assert!(!o.policy.answer_questions, "{text:?}");
    }
    let on = read("[harness]\nquestion_priority = \"short-term\"\n");
    let o = SuperviseOpts::hosted_with(&on);
    assert!(o.policy.answer_questions);
    assert_eq!(o.policy.question_priority, PriorityWord::ShortTerm);
}

#[test]
fn an_agent_table_switches_that_agent_off_and_nothing_else() {
    let full = SupervisorConfig::default();
    assert!(full.off.is_empty());
    assert!(full.on_for(Program::Claude) && full.on_for(Program::Codex));
    let (c, notes) = SupervisorConfig::from_aterm_toml("[harness.codex]\nenabled = false\n");
    assert!(notes.is_empty(), "{notes:?}");
    assert!(!c.on_for(Program::Codex) && c.on_for(Program::Claude));
    assert!(c.enabled, "the master switch stays on");
    assert_eq!(
        SupervisorConfig {
            off: Vec::new(),
            ..c
        },
        full,
        "nothing else moved"
    );
    for text in [
        "[harness]\ncodex.enabled = false\n",
        "harness.codex.enabled = false\n",
        "harness = { codex = { enabled = false } }\n",
        "[harness]\ncodex = { enabled = false }\n",
        "[harness.codex]\nenabled = \"no\"\n",
        "[harness.codex]\nenabled = 0\n",
        "[harness.codex]\nenabled = \"none\"\n",
    ] {
        assert_eq!(read(text).off, [Program::Codex], "{text:?}");
    }
    let both = read("[harness.codex]\nenabled = false\n[harness.claude]\nenabled = false\n");
    assert_eq!(both.off, [Program::Claude, Program::Codex]);
    assert_eq!(
        both,
        read("[harness.claude]\nenabled = false\n[harness.codex]\nenabled = false\n"),
        "the same policy in either order"
    );
    assert!(both.enabled && !both.on_for(Program::Claude) && !both.on_for(Program::Codex));
    let (c, notes) = SupervisorConfig::from_aterm_toml("[harness.codex]\nenabled = true\n");
    assert_eq!((c, notes), (full.clone(), Vec::new()), "`true` is accepted");
    let c = read("[harness]\nenabled = false\n[harness.codex]\nenabled = true\n");
    assert!(!c.on_for(Program::Codex) && !c.on_for(Program::Claude));
    for (text, off) in [
        ("\"none\"", true),
        ("\"sometimes\"", false),
        ("2.5", false),
        ("[false]", false),
    ] {
        let (c, notes) =
            SupervisorConfig::from_aterm_toml(&format!("[harness.codex]\nenabled = {text}\n"));
        assert_eq!(c.on_for(Program::Codex), !off, "{text}: {notes:?}");
        assert!(
            notes.len() == 1 && notes[0].starts_with("harness.codex.enabled:"),
            "{text}: {notes:?}"
        );
    }
}

#[test]
fn only_enabled_is_per_agent() {
    let full = SupervisorConfig::default();
    let (c, notes) = SupervisorConfig::from_aterm_toml(
        "[harness.codex]\napprove = \"none\"\nenabled = false\ncontinu = false\n",
    );
    assert_eq!(c.approve, Approve::All, "{notes:?}");
    assert_eq!(c.off, [Program::Codex]);
    assert_eq!(notes.len(), 2, "one note per key: {notes:?}");
    for (note, key) in notes.iter().zip(["approve", "continu"]) {
        assert!(note.starts_with(&format!("harness.codex.{key}:")), "{note}");
        assert!(is_unknown_key_note(note), "{note}");
    }
    assert_eq!(
        read("[harness]\napprove = \"none\"\n").approve,
        Approve::None
    );
    let (c, notes) = SupervisorConfig::from_aterm_toml("[harness.claude.sub]\nenabled = false\n");
    assert_eq!(c, full);
    assert!(is_unknown_key_note(&notes[0]), "{notes:?}");
    for id in ["no-such-agent", "Codex", "claude-code", "generic", "sub"] {
        let (c, notes) =
            SupervisorConfig::from_aterm_toml(&format!("[harness.{id}]\nenabled = false\n"));
        assert_eq!(c, full, "{id}");
        assert!(
            notes.len() == 1
                && notes[0].starts_with(&format!("harness.{id}:"))
                && is_unknown_key_note(&notes[0]),
            "{id}: {notes:?}"
        );
    }
    let (c, notes) = SupervisorConfig::from_aterm_toml("[harness]\ncodex = false\n");
    assert_eq!(c, full);
    assert!(is_unknown_key_note(&notes[0]), "{notes:?}");
}

#[test]
fn a_misplaced_agent_table_only_limits() {
    let (c, notes) = SupervisorConfig::from_aterm_toml("[theme]\nharness.codex.enabled = false\n");
    assert_eq!(c.off, [Program::Codex]);
    assert!(
        notes[0].contains("`theme.harness.codex.enabled`"),
        "{notes:?}"
    );
    let c = read("[harness.codex]\nenabled = false\n[theme]\nharness.codex.enabled = true\n");
    assert_eq!(c.off, [Program::Codex]);
    assert_eq!(
        read("[a]\nharness.codex.enabled = true\n[harness.codex]\nenabled = false\n").off,
        [Program::Codex]
    );
    assert_eq!(
        read("[theme.harness.claude]\nenabled = false\n").off,
        [Program::Claude]
    );
    assert_eq!(
        read("[theme.harness.codex]\napprove = \"none\"\n"),
        SupervisorConfig::default()
    );
}

#[test]
fn each_key_of_an_agent_table_is_said_on_its_own() {
    const MISPLACED: &str =
        "[theme]\nharness.codex.enabled = false\nharness.codex.approve = \"none\"\n";
    for text in ["[harness.codex]\nenabled = \"none\"\nfoo = 1\n", MISPLACED] {
        let (c, notes) = SupervisorConfig::from_aterm_toml(text);
        assert_eq!(c.off, [Program::Codex], "{text:?}");
        let (unknown, shown): (Vec<_>, Vec<_>) = notes.iter().partition(|n| is_unknown_key_note(n));
        assert!(
            shown.len() == 1 && shown[0].starts_with("harness.codex.enabled:"),
            "{text:?}: {notes:?}"
        );
        assert_eq!(unknown.len(), 1, "{text:?}: {notes:?}");
    }
    let (by_toml, mut toml) = SupervisorConfig::from_aterm_toml(MISPLACED);
    let refused = format!("font_px = \n{MISPLACED}");
    assert!(
        aterm_toml::from_str::<aterm_toml::Value>(&refused).is_err(),
        "the fixture must be one the parser refuses"
    );
    let (by_lines, mut lines) = SupervisorConfig::from_aterm_toml(&refused);
    lines.remove(0);
    assert_eq!(by_toml, by_lines);
    toml.sort();
    lines.sort();
    assert_eq!(toml, lines, "both readers say the same of each key");
}

#[test]
fn a_file_that_is_not_toml_reads_an_agent_table_too() {
    const TYPO: &str = "font_px = \n";
    for tail in [
        "[harness.codex]\nenabled = false\n",
        "[harness]\ncodex.enabled = false\n",
        "harness.codex.enabled = false\n",
        "harness = { codex = { enabled = false } }\n",
        "[harness.codex]\nenabled = false\nenabled = true\n",
        "[harness.codex]\nenabled = true\nenabled = false\n",
        "[theme]\nharness.codex.enabled = false\n",
    ] {
        let text = format!("{TYPO}{tail}");
        assert!(
            aterm_toml::from_str::<aterm_toml::Value>(&text).is_err(),
            "the fixture must be one the parser refuses: {text:?}"
        );
        let c = read(&text);
        assert_eq!(c.off, [Program::Codex], "{text:?}");
        assert!(c.on_for(Program::Claude), "{text:?}");
    }
    for tail in [
        "[harness.codex]\nenabled = true\n",
        "[harness.no-such-agent]\nenabled = false\n",
        "[[harness.codex]]\nenabled = false\n",
        "[harness.codex]\nnote = \"enabled = false\"\n",
        "# harness.codex.enabled = false\n",
        "[harness.codex]\napprove = \"none\"\n",
    ] {
        let text = format!("{TYPO}{tail}");
        assert_eq!(read(&text), SupervisorConfig::default(), "{text:?}");
    }
    let (_, notes) =
        SupervisorConfig::from_aterm_toml(&format!("{TYPO}[harness.codex]\napprove = \"none\"\n"));
    assert!(notes.iter().any(|n| is_unknown_key_note(n)), "{notes:?}");
}

#[test]
fn an_agent_switched_off_is_a_limit_and_named_as_one() {
    let full = SupervisorConfig::default();
    let off = read("[harness.codex]\nenabled = false\n");
    assert!(off.limits(&full), "an agent off limits");
    assert!(!full.limits(&off), "an agent back on is no limit");
    assert_eq!(
        off.against_default(),
        Written {
            limits: vec!["codex: off".to_string()],
            own: Vec::new(),
        }
    );
    let text = "[harness]\napprove = \"safe\"\n[harness.codex]\nenabled = false\n\
                [harness.claude]\nenabled = false\n";
    assert_eq!(
        read(text).against_default().limits,
        ["approve: safe", "claude: off", "codex: off"]
    );
    let mut c = full.clone();
    c.limit("codex.enabled");
    assert_eq!(c, off);
    let mut c = full.clone();
    c.set("codex.enabled", "false").expect("a switch");
    assert_eq!(c, off);
    c.set("codex.enabled", "true").expect("a switch");
    assert_eq!(c, off, "`true` changes nothing");
    assert!(c.set("codex.approve", "none").is_err());
    assert_eq!(c, off);
    assert_eq!(
        SupervisorConfig::default_shown("codex.enabled").as_deref(),
        Some("true")
    );
    for key in ["codex.approve", "no-such-agent.enabled", "codex"] {
        assert_eq!(SupervisorConfig::default_shown(key), None, "{key}");
    }
}

/// `accounts_off_by` NAMES EXACTLY THE SETTINGS THAT TURN ROTATION OFF FOR THE AGENT:
/// `[harness] accounts`, a `harness.accounts` filed below another table's header, the swarm
/// switch, the harness's master switch and that agent's own (CONTROL: another agent's own
/// switch is not named), each where it turns rotation off and none where it does not, a
/// `[swarm] enabled = true` or a misplaced `harness.accounts = "early"` beside a setting that
/// does included; every setting that does, where two do, two the reader files alike
/// included, and a master or agent switch said twice, once below another table's header
/// (NEGATIVE CONTROL: the rule that named a switch by the reading named that one setting
/// alone, so the line said rotation is back on once it is gone); a master or agent switch
/// filed below another table's header, or the harness written off there, as written there
/// (NEGATIVE CONTROL: that rule named `[harness] enabled = false` for the misplaced master
/// switch and for the harness written off there, and `[harness.codex] enabled = false` for the
/// misplaced agent switch, none of which aterm.toml says); it names one exactly where
/// `accounts_for` leaves rotation off; and where the file is not valid TOML it gives where the
/// parser stopped and the parser's own reason, on one line.
#[test]
fn accounts_off_by_names_only_what_turns_rotation_off() {
    use crate::harness::accounts::Mode;
    let key = |key: &str, value: &str| OffSetting::Harness {
        key: key.to_string(),
        value: value.to_string(),
    };
    let harness = |value: &str| key("accounts", value);
    let swarm = |written: &str| OffSetting::Swarm(written.to_string());
    let misplaced = |key: &str, value: &str| OffSetting::Misplaced {
        at: "theme".to_string(),
        key: key.to_string(),
        value: value.to_string(),
    };
    for (text, want) in [
        ("", Vec::new()),
        ("[harness]\naccounts = \"early\"\n", Vec::new()),
        ("[harness]\naccounts = \"off\"\n", vec![harness("\"off\"")]),
        ("[harness]\naccounts = false\n", vec![harness("false")]),
        ("[harness]\naccounts = 'off'\n", vec![harness("\"off\"")]),
        (
            "[harness]\naccounts = \"off\"\n[swarm]\nenabled = true\n",
            vec![harness("\"off\"")],
        ),
        (
            "[harness]\naccounts = \"off\"\n[theme]\nharness.accounts = \"early\"\n",
            vec![harness("\"off\"")],
        ),
        (
            "[harness]\naccounts = \"early\"\n[swarm]\nenabled = false\n",
            vec![swarm("[swarm] enabled = false")],
        ),
        ("[swarm]\nenabled = true\n", Vec::new()),
        ("swarm = false\n", vec![swarm("swarm = false")]),
        ("swarm = true\n[harness]\naccounts = \"wall\"\n", Vec::new()),
        (
            "[theme]\nharness.accounts = \"off\"\n",
            vec![misplaced("accounts", "\"off\"")],
        ),
        (
            "[harness]\naccounts = \"off\"\n[theme]\nharness.accounts = \"off\"\n",
            vec![harness("\"off\""), misplaced("accounts", "\"off\"")],
        ),
        (
            "[harness]\naccounts = \"off\"\n[swarm]\nenabled = false\n",
            vec![harness("\"off\""), swarm("[swarm] enabled = false")],
        ),
        (
            "[\"a.b\"]\nharness.accounts = \"off\"\n[a.b]\nharness.accounts = \"off\"\n",
            vec![
                OffSetting::Misplaced {
                    at: "a.b".to_string(),
                    key: "accounts".to_string(),
                    value: "\"off\"".to_string(),
                };
                2
            ],
        ),
        (
            "[harness]\nenabled = false\n[theme]\nharness.enabled = false\n",
            vec![key("enabled", "false"), misplaced("enabled", "false")],
        ),
        (
            "[harness.codex]\nenabled = false\n[theme]\nharness.codex.enabled = false\n",
            vec![
                key("codex.enabled", "false"),
                misplaced("codex.enabled", "false"),
            ],
        ),
        (
            "[theme]\nharness.enabled = false\n",
            vec![misplaced("enabled", "false")],
        ),
        (
            "[theme]\nharness.codex.enabled = false\n",
            vec![misplaced("codex.enabled", "false")],
        ),
        (
            "[theme]\nharness = false\n",
            vec![OffSetting::MisplacedWrittenOff {
                at: "theme".to_string(),
                value: "false".to_string(),
            }],
        ),
        ("[theme]\nharness.accounts = \"early\"\n", Vec::new()),
        ("[theme]\nswarm = false\n", Vec::new()),
        (
            "[harness]\nenabled = false\n",
            vec![key("enabled", "false")],
        ),
        (
            "harness = false\n",
            vec![OffSetting::HarnessWrittenOff("false".to_string())],
        ),
        ("[harness]\nenabled = true\n", Vec::new()),
        (
            "[harness.codex]\nenabled = false\n",
            vec![key("codex.enabled", "false")],
        ),
        ("[harness.claude]\nenabled = false\n", Vec::new()),
    ] {
        let off_by = SupervisorConfig::accounts_off_by(text, Program::Codex).expect("valid TOML");
        assert_eq!(off_by, want, "{text}");
        assert_eq!(
            off_by.is_empty(),
            read(text).accounts_for(Program::Codex) != Mode::Off,
            "{text}: the reader agrees"
        );
    }
    // Each says where aterm.toml says it.
    for (setting, said) in [
        (
            key("codex.enabled", "false"),
            "`[harness.codex] enabled = false`",
        ),
        (key("logins", "\"off\""), "`[harness] logins = \"off\"`"),
        (
            misplaced("enabled", "false"),
            "`theme.harness.enabled = false` (a `harness.enabled` below another table's \
             header)",
        ),
        (
            OffSetting::MisplacedWrittenOff {
                at: "theme".to_string(),
                value: "false".to_string(),
            },
            "`theme.harness = false` (a `harness` below another table's header)",
        ),
    ] {
        assert_eq!(setting.words(), said);
    }
    let broken = "[harness]\naccounts = \"off\"\n[broken\n";
    let why =
        SupervisorConfig::accounts_off_by(broken, Program::Codex).expect_err("not valid TOML");
    assert!(!why.is_empty() && !why.contains('\n'), "{why:?}");
    // Where the parser stopped, and the parser's own reason, not the position alone.
    let parsed = aterm_toml::from_str::<aterm_toml::Value>(broken).expect_err("not valid TOML");
    let reason = crate::supervise::limit::one_line(parsed.message_text());
    assert!(!reason.is_empty(), "the parser names a reason");
    assert!(why.starts_with("TOML error at line 3, column "), "{why:?}");
    assert!(why.ends_with(&format!(": {reason}")), "{why:?}");
}

#[test]
fn swarm_enabled_false_turns_accounts_off_even_beside_harness_accounts_early() {
    let cfg = read("[swarm]\nenabled = false\n[harness]\naccounts = \"early\"\n");
    assert_eq!(cfg.accounts, crate::harness::accounts::Mode::Off);
    // Negative control: `[swarm] enabled = true` leaves `[harness] accounts` in force.
    let on = read("[swarm]\nenabled = true\n[harness]\naccounts = \"early\"\n");
    assert_eq!(on.accounts, crate::harness::accounts::Mode::Early);
    // Broken TOML keeps both supported Off spellings, including beside an early account mode.
    for raw in [
        "[swarm]\nenabled = false\n[harness\n",
        "swarm = false\n[harness\n",
        "swarm = false\n[harness]\naccounts = 'early'\n[broken\n",
        "[swarm]\nenabled = false\n[harness]\naccounts = \"early\"\n[harness\n",
        "swarm = false\n[harness]\naccounts = \"early\"\n[harness\n",
    ] {
        let (broken, _) = SupervisorConfig::from_aterm_toml(raw);
        assert_eq!(
            broken.accounts,
            crate::harness::accounts::Mode::Off,
            "{raw}"
        );
    }
    assert_eq!(switch_word("_-1"), None);
    assert_eq!(switch_word("0_x1"), None);
}
