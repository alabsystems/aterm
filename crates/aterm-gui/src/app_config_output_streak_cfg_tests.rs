// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{Config, DEFAULT_DECORATIVE_EFFECTS};
use aterm_effects::output_streak::{MAX_COMETS, TAIL_MAX, TAIL_MIN};

fn cfg(toml: &str) -> Config {
    aterm_toml::from_str(toml).expect("valid toml")
}

#[test]
fn output_streak_keys_resolve_clamp_and_fail_toward_less_effect() {
    assert_eq!(
        cfg("[output_streak]\ntail = 12").output_streak_enabled_or_default(),
        DEFAULT_DECORATIVE_EFFECTS
    );
    assert!(!cfg("[output_streak]\nenabled = false").output_streak_enabled_or_default());

    type Read = fn(&Config) -> f64;
    let intensity: Read = |c| f64::from(c.output_streak_intensity_or_default());
    let tail: Read = |c| f64::from(c.output_streak_tail_or_default());
    let streaks: Read = |c| f64::from(c.output_streak_max_streaks_or_default());
    let idle: Read = |c| f64::from(c.output_streak_idle_secs_or_default());
    for (key, read, value, expected) in [
        ("intensity", intensity, "4.0", 1.0),
        ("intensity", intensity, "-2.0", 0.0),
        ("intensity", intensity, "nan", 0.0),
        ("tail", tail, "4000", f64::from(TAIL_MAX)),
        ("tail", tail, "0", f64::from(TAIL_MIN)),
        ("max_streaks", streaks, "99", MAX_COMETS as f64),
        ("max_streaks", streaks, "0", 1.0),
        ("idle_secs", idle, "9000.0", 120.0),
        ("idle_secs", idle, "0.0", 2.0),
        ("idle_secs", idle, "nan", 2.0),
    ] {
        assert_eq!(
            read(&cfg(&format!("[output_streak]\n{key} = {value}"))),
            expected,
            "{key} = {value}"
        );
    }
}

/// `idle_rest_secs`: unset never rests ([`DEFAULT_IDLE_REST_SECS`]); a value is clamped into
/// 2..=3600 s; zero, a negative, NaN or an infinity never rests (today's behaviour), never at
/// once.
#[test]
fn idle_rest_resolves_clamps_and_fails_toward_never() {
    use std::time::{Duration, Instant};
    assert_eq!(Config::default().idle_rest_or_default(), None);
    for (value, expected) in [
        ("45", Some(Duration::from_secs(45))),
        ("2.5", Some(Duration::from_millis(2500))),
        ("1", Some(Duration::from_secs(2))),
        ("0.01", Some(Duration::from_secs(2))),
        ("1e9", Some(Duration::from_secs(3600))),
        ("0", None),
        ("-1", None),
        ("nan", None),
        ("inf", None),
        ("-inf", None),
    ] {
        assert_eq!(
            cfg(&format!("idle_rest_secs = {value}")).idle_rest_or_default(),
            expected,
            "idle_rest_secs = {value}"
        );
    }
    // A reload to `0` wakes a window that rested under the old setting.
    let t0 = Instant::now();
    let mut rest = crate::window_rest::Rest::new(t0);
    let later = t0 + Duration::from_secs(60);
    assert!(rest.settle(later, cfg("idle_rest_secs = 30").idle_rest_or_default()));
    assert!(rest.resting());
    assert!(rest.settle(later, cfg("idle_rest_secs = 0").idle_rest_or_default()));
    assert!(!rest.resting());
}

#[test]
fn the_streak_warning_names_only_the_silently_unreachable_pip() {
    assert!(Config::default().output_streak_warning().is_none());
    assert!(
        cfg("[output_streak]\ntail = 4000\nmax_streaks = 99\nidle_secs = 9000.0")
            .output_streak_warning()
            .is_none(),
        "a documented clamp is not a warning in this repo"
    );
    assert!(
        cfg("[output_streak]\nenabled = true\nsound = true")
            .output_streak_warning()
            .is_none()
    );
    let warning = cfg("[output_streak]\nenabled = false\nsound = true")
        .output_streak_warning()
        .expect("an unreachable pip is worth saying out loud");
    assert!(
        warning.contains("output_streak.sound"),
        "the message names the key: {warning}"
    );
}

#[test]
fn the_harness_words_name_every_limit_and_only_limits() {
    let read = super::HarnessPolicy::read;
    let words = |text: &str| read(text).words();
    let automatic = words("");
    for neutral in [
        "[harness]\naccounts = \"early\"\n",
        "[harness]\nanswer_text = \"ask me\"\n",
        "[harness]\nquestion_priority = \"long-term\"\n",
        "[harness]\nlimit_reset = true\n",
        "[harness]\nlimit_reset = false\n",
        "[harness]\nusage_tap = true\n",
        "[harness]\nusage_notes = true\n",
        "[harness]\nusage_notes = false\n",
        "[harness]\nusage_credits = true\n",
        "[harness]\nusage_credits = false\n",
    ] {
        assert_eq!(words(neutral), automatic, "{neutral}");
    }
    assert_eq!(
        words("[harness]\nusage_tap = false\n"),
        "Automatic, limited by 1 setting in aterm.toml",
        "the tap ships on, so turning it off is a limit"
    );
    let safe = words("[harness]\napprove = \"safe\"\n");
    assert_ne!(safe, automatic);
    for own_roots in [
        "[harness]\napprove = \"safe\"\ntrust_roots = []\n",
        "[harness]\napprove = \"safe\"\ntrust_roots = [\"~/src/app*\"]\n",
    ] {
        assert_eq!(words(own_roots), safe, "{own_roots}");
    }
    let all = "[harness]\nheadless = false\napprove = \"none\"\ntrust_roots = []\n\
               answer_questions = false\nanswer_text = \"ask me\"\n\
               dismiss_surveys = false\ncontinue = false\ncontinue_text = \"go\"\n\
               continue_per_hour = 2\nrules_file = \"/r\"\nretry_api_errors = false\n\
               probe_api = false\nresume_limits = false\nlimit_wait = false\n\
               accounts = \"off\"\n\
               compact_on_context_wall = false\nrelaunch = false\nupgrade = false\n\
               human_grace_s = 600\n";
    for named in [
        "[harness]\napprove = \"none\"\n",
        "[harness]\nanswer_questions = false\n",
        "[harness]\naccounts = \"wall\"\n",
        all,
    ] {
        let said = words(named);
        assert_ne!(said, automatic, "{named}");
        for key in aterm_agent::supervise::config::KEYS {
            assert!(
                !key.contains('_') || !said.contains(key),
                "{key} is a file's word, not the row's: {said}"
            );
        }
        assert!(!said.contains(": "), "no `key: value`: {said}");
    }
    let off = words(&format!("{all}enabled = false\n"));
    assert_eq!(
        off,
        words("[harness]\nenabled = false\n"),
        "Off outranks every limit"
    );
    assert_ne!(off, automatic);
    assert!(!super::HarnessPolicy::read("").policy.limit_reset);
    assert!(!super::HarnessPolicy::read("").policy.usage_notes);
    assert!(!super::HarnessPolicy::read("").policy.usage_credits);
    assert_eq!(
        read("[harness.claude]\nenabled = false\n").words(),
        "Automatic, limited by Supervise Claude Code sessions set to Off"
    );
    assert_eq!(
        read("[harness]\nautopilot = \"land\"\n[harness.codex]\nenabled = false\n").words(),
        "Automatic, limited by Default autopilot set to Land; Enable codex-alab Autopilot set \
         to Off"
    );
    assert_eq!(
        read("[harness]\nautopilot = \"off\"\nrelaunch = false\n").words(),
        "Automatic, limited by Default autopilot set to Off; 1 setting in aterm.toml"
    );
    assert_eq!(
        read("[harness]\nrelaunch = false\n").words(),
        "Automatic, limited by 1 setting in aterm.toml"
    );
    assert_eq!(read("[harness]\nrelaunch = false\n").file_limits(), 1);
    assert_eq!(read("[harness]\napprove = \"none\"\n").file_limits(), 0);
}

#[test]
fn every_harness_rows_limit_is_named_by_its_row() {
    let read = super::HarnessPolicy::read;
    let mut limited = Vec::new();
    for leaf in crate::prefs::NESTED_LEAVES
        .iter()
        .filter(|leaf| leaf.key.starts_with("harness."))
        .filter(|leaf| leaf.key != crate::prefs::EDIT_HARNESS_ENABLED)
    {
        let (table, key) = leaf.key.rsplit_once('.').expect("a dotted key");
        let values: Vec<String> = match leaf.kind {
            crate::prefs::EditKind::Bool => vec!["false".to_string()],
            crate::prefs::EditKind::Enum { options } => options
                .iter()
                .map(|option| format!("\"{option}\""))
                .collect(),
            crate::prefs::EditKind::Text if leaf.key == crate::prefs::EDIT_HARNESS_LAND_AT => {
                let policy = read("[harness]\nland_at = \"18:00\"\n");
                assert!(
                    policy.policy.against_default().limits.is_empty(),
                    "land_at limits nothing: {}",
                    policy.words()
                );
                assert_eq!(policy.file_limits(), 0, "land_at is no file limit");
                Vec::new()
            }
            kind => panic!("{}: a {kind:?} row", leaf.key),
        };
        for value in values {
            let source = format!("[{table}]\n{key} = {value}\n");
            let policy = read(&source);
            if policy.policy.against_default().limits.is_empty() {
                continue;
            }
            let words = policy.words();
            assert!(words.contains(leaf.label), "{source}: {words}");
            assert!(!words.contains("aterm.toml"), "{source}: {words}");
            assert_eq!(policy.file_limits(), 0, "{source}");
            limited.push(leaf.key);
        }
    }
    for key in [
        crate::prefs::EDIT_HARNESS_CLAUDE_ENABLED,
        crate::prefs::EDIT_HARNESS_CODEX_ENABLED,
        crate::prefs::EDIT_HARNESS_AUTOPILOT,
        crate::prefs::EDIT_HARNESS_APPROVE,
        crate::prefs::EDIT_HARNESS_ANSWER_QUESTIONS,
    ] {
        assert!(limited.contains(&key), "{key} limits: {limited:?}");
    }
    assert_eq!(read("[harness]\nrelaunch = false\n").file_limits(), 1);
}

#[test]
fn a_launch_that_could_not_load_the_file_still_reads_its_harness_lines() {
    let text = "font_px = \n[harness]\ncontinue = false\napprove = \"safe\"\n";
    assert!(Config::parse(text).is_err(), "the fixture is not a config");
    let stand_in = Config::stand_in(text);
    let p = &stand_in.harness.policy;
    assert!(p.enabled && !p.continue_policy);
    assert_eq!(p.approve, aterm_agent::supervise::config::Approve::Safe);
    assert!(!stand_in.harness.notes.is_empty());
    assert!(
        stand_in.font_px.is_none(),
        "every other setting at its default"
    );
    assert_eq!(
        Config::stand_in("font_px = \n").harness.policy,
        aterm_agent::supervise::SupervisorConfig::default()
    );
}

#[test]
fn the_harness_table_is_the_supervisors_policy() {
    let cfg = |text: &str| Config::parse(text).expect("valid config");
    let text = "[harness]\ncontinue = false\n";
    assert!(cfg(text) != cfg("[harness]\ncontinue = true\n"));

    let bad = "[harness]\ncontinue = \"maybe\"\nheadless = false\ncontine = true\n";
    let notices = cfg(bad).harness_notices();
    assert_eq!(notices.len(), 1, "the unknown key is the config language's");
    assert!(
        notices[0].starts_with("config harness.continue:") && notices[0].contains("maybe"),
        "{notices:?}"
    );
    let ignored = crate::native_config_language::key_warnings(bad).ignored;
    assert!(ignored.iter().any(|l| l.contains("contine")), "{ignored:?}");

    let text = "[harness]\napprove = \"everything\"\n";
    let told = cfg(text).harness_notices();
    assert_eq!(told.len(), 1, "{told:?}");
    assert_eq!(
        super::unaccepted_value_notices(text, &told),
        Vec::<String>::new()
    );
    assert_eq!(super::unaccepted_value_notices(text, &[]).len(), 1);

    let misplaced = cfg("[packages]\nharness.enabled = false\n");
    assert!(!misplaced.harness.policy.enabled);
    assert!(
        misplaced.harness_notices()[0].contains("`packages.harness.enabled`"),
        "{:?}",
        misplaced.harness_notices()
    );
}

/// THE OWNER'S TABLES RIDE THE POLICY: the mode and the tables a window adopts come from one
/// parse of one text, and a copy written below another table's header is no table at all
/// (NEGATIVE CONTROL: the same rule at the root reads).
#[test]
fn the_owner_tables_ride_the_harness_policy_from_the_root_alone() {
    let cfg = |text: &str| Config::parse(text).expect("valid config");
    let rule = "occupancy_rule]]\nname = \"r\"\nwhen = \"workflow.returned\"\n\
                text = \"Re-read the base.\"\n";
    let root = format!("[harness]\noccupancy = \"prompt\"\n\n[[harness.{rule}");
    let read = cfg(&root);
    assert_eq!(read.harness, super::HarnessPolicy::read(&root));
    assert_eq!(read.harness.owner.rules().len(), 1);
    assert!(
        read.harness_notices().is_empty(),
        "{:?}",
        read.harness_notices()
    );

    let misplaced = cfg(&format!("[packages]\n\n[[packages.harness.{rule}"));
    assert!(
        misplaced.harness.owner.is_empty(),
        "{:?}",
        misplaced.harness.owner
    );
    assert_eq!(
        misplaced.harness.policy.occupancy,
        aterm_agent::supervise::SupervisorConfig::default().occupancy
    );
    let notices = misplaced.harness_notices();
    assert!(
        notices.len() == 1
            && notices[0].contains("`packages.harness.occupancy_rule`")
            && notices[0].contains("root [harness] table alone")
            && !notices[0].contains("Re-read"),
        "{notices:?}"
    );
}
