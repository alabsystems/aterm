// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{
    Config, EDIT_BACKGROUND, EDIT_BACKGROUND_OPACITY, EDIT_COLUMNS, EDIT_COPY_ON_SELECT,
    EDIT_CURSOR_COLOR, EDIT_CURSOR_STYLE, EDIT_CURSOR_TRAIL_STYLE, EDIT_FONT_FAMILY, EDIT_FONT_PX,
    EDIT_FONT_WEIGHT, EDIT_FONT_WEIGHT_DARK_NUDGE, EDIT_FOREGROUND, EDIT_LIGATURES,
    EDIT_LINE_HEIGHT, EDIT_LINES, EDIT_SCROLLBACK, EDIT_SEARCH_HISTORY_LINES, EDIT_SELECTION_COLOR,
    EDIT_THEME, EDIT_WINDOW_PADDING, EDIT_WINDOW_PADDING_TOP, EditKind, PrefsEditError, Range,
    apply_prefs_edits, editable_fields, keywords_of, range_of,
};

fn set(v: &str) -> Option<String> {
    Some(v.to_string())
}

#[test]
fn dotted_remove_reverts_and_prunes_only_a_bare_table() {
    let keep_sibling = "[matrix_rain]\nenabled = true\nfps = 24\n";
    let out = apply_prefs_edits(keep_sibling, &[(super::EDIT_MATRIX_RAIN_ENABLED, None)])
        .expect("dotted remove");
    assert!(!out.contains("enabled"), "child removed");
    assert!(
        out.contains("[matrix_rain]") && out.contains("fps = 24"),
        "table + sibling survive: {out:?}"
    );

    let lone = "font_px = 13.0\n[matrix_rain]\n# mine\nenabled = true\n";
    let out = apply_prefs_edits(lone, &[(super::EDIT_MATRIX_RAIN_ENABLED, None)])
        .expect("dotted remove of the last child");
    assert_eq!(
        out, "font_px = 13.0\n[matrix_rain]\n# mine\n",
        "an emptied table is retained, with the comment above its last key"
    );
    let bare = "font_px = 13.0\n\n[matrix_rain]\nenabled = true\n";
    let out = apply_prefs_edits(bare, &[(super::EDIT_MATRIX_RAIN_ENABLED, None)])
        .expect("dotted remove of the last uncommented child");
    assert_eq!(
        out, "font_px = 13.0\n",
        "a bare table goes with its last key"
    );
    let cfg: crate::app_config::Config = aterm_toml::from_str(&out).expect("round-trip");
    assert!(
        !cfg.matrix_rain_enabled(),
        "an emptied [matrix_rain] resolves to the same default as an absent one"
    );

    let out = apply_prefs_edits(
        "font_px = 13.0\n",
        &[(super::EDIT_MATRIX_RAIN_ENABLED, None)],
    )
    .expect("remove-from-absent is a no-op");
    assert_eq!(out, "font_px = 13.0\n");
}

#[test]
fn matrix_rain_row_surfaces_in_the_settings_model() {
    let key = super::EDIT_MATRIX_RAIN_ENABLED;
    let (group, order) = super::group_of(key);
    assert_ne!(group, "General", "routes out of the catch-all group");
    assert!(order < u8::MAX, "group order assigned");
    assert!(super::footnote_of(key).is_some());
    assert!(!keywords_of(key).is_empty());
    assert!(
        super::nested_leaf(key).is_some(),
        "the row is registered ONCE, via NESTED_LEAVES (no duplicate EditField)"
    );

    let unset = editable_fields(&Config::default());
    assert_eq!(
        unset.iter().filter(|f| f.key == key).count(),
        1,
        "registered once in editable_fields"
    );
    let row = unset.iter().find(|f| f.key == key).expect("row exists");
    assert_eq!(row.kind, EditKind::Bool);
    assert_eq!(row.seed.as_deref(), Some("false"), "resolved default OFF");

    let on: Config = aterm_toml::from_str("[matrix_rain]\nenabled = true").unwrap();
    let row_on = editable_fields(&on)
        .into_iter()
        .find(|f| f.key == key)
        .unwrap();
    assert_eq!(row_on.seed.as_deref(), Some("true"), "resolved ON");
}

#[test]
fn the_output_streak_switch_is_a_row_seeded_where_the_engine_starts() {
    let key = super::EDIT_OUTPUT_STREAK_ENABLED;
    let row = editable_fields(&Config::default())
        .into_iter()
        .find(|f| f.key == key)
        .expect("the row exists on an unconfigured install");
    assert_eq!(row.label, "Output streak");
    assert_eq!(row.kind, EditKind::Bool);
    assert_eq!(super::section_of(key), super::Section::Appearance);
    assert_eq!(super::group_of(key), ("Effects", 5));
    assert_eq!(
        row.seed.as_deref(),
        Some(if crate::app_config::DEFAULT_DECORATIVE_EFFECTS {
            "true"
        } else {
            "false"
        }),
        "the resolved platform default"
    );
    let off: Config = aterm_toml::from_str("[output_streak]\nenabled = false").unwrap();
    let row_off = editable_fields(&off)
        .into_iter()
        .find(|f| f.key == key)
        .unwrap();
    assert_eq!(row_off.seed.as_deref(), Some("false"), "resolved OFF");
    let written = apply_prefs_edits("", &[(key, set("false"))])
        .expect("the nested writer creates [output_streak]");
    assert!(written.contains("[output_streak]"), "{written}");
    let reparsed: Config = aterm_toml::from_str(&written).expect("re-parses as Config");
    assert!(!reparsed.output_streak_enabled_or_default());
}

#[test]
fn the_harness_rows_round_trip_through_set_and_unset() {
    use aterm_agent::supervise::config::Approve;
    use aterm_agent::supervise::{Autopilot, PriorityWord, SupervisorConfig};
    let seed = |cfg: &Config, key: &str| {
        editable_fields(cfg)
            .into_iter()
            .find(|f| f.key == key)
            .expect("the row exists")
            .seed
    };
    let full = SupervisorConfig::default();
    type Row = (
        &'static str,
        Option<&'static str>,
        &'static str,
        &'static str,
        fn(&SupervisorConfig) -> bool,
    );
    let rows: [Row; 5] = [
        (
            super::EDIT_HARNESS_ENABLED,
            Some("true"),
            "false",
            "false",
            |p| !p.enabled,
        ),
        (super::EDIT_HARNESS_AUTOPILOT, None, "Land", "land", |p| {
            p.autopilot == Autopilot::Land && p.enabled
        }),
        (super::EDIT_HARNESS_APPROVE, None, "safe", "safe", |p| {
            p.approve == Approve::Safe && p.enabled
        }),
        (
            super::EDIT_HARNESS_ANSWER_QUESTIONS,
            Some("true"),
            "false",
            "false",
            |p| !p.answer_questions && p.approve == Approve::All,
        ),
        (
            super::EDIT_HARNESS_QUESTION_PRIORITY,
            None,
            "Short-Term",
            "short-term",
            |p| p.question_priority == PriorityWord::ShortTerm && p.answer_questions,
        ),
    ];
    for (key, absent, typed, stored, took) in rows {
        assert_eq!(super::section_of(key), super::Section::Harness, "{key}");
        assert_eq!(super::environment_precedence(key), None, "{key}");
        assert_eq!(super::application_timing(key), None, "{key}");
        assert!(super::application_has_live_effect(key), "{key}");
        assert!(super::footnote_of(key).is_some(), "{key}");
        assert_eq!(seed(&Config::default(), key).as_deref(), absent, "{key}");

        let written = apply_prefs_edits("font_px = 14\n", &[(key, set(typed))]).expect("set");
        assert!(
            written.contains("font_px = 14") && written.contains("[harness]"),
            "{written}"
        );
        let cfg = Config::parse(&written).expect("re-parses as Config");
        assert!(cfg.harness_notices().is_empty(), "{written}");
        assert!(took(&cfg.harness.policy), "{written}");
        assert_eq!(seed(&cfg, key).as_deref(), Some(stored), "{written}");

        let cleared = apply_prefs_edits(&written, &[(key, None)]).expect("unset");
        let back = Config::parse(&cleared).expect("re-parses as Config");
        assert_eq!(back.harness.policy, full, "{cleared}");
    }
    for (key, word) in [
        (super::EDIT_HARNESS_QUESTION_PRIORITY, "long_term"),
        (super::EDIT_HARNESS_AUTOPILOT, "hover"),
    ] {
        assert!(
            apply_prefs_edits("", &[(key, set(word))]).is_err(),
            "{key}: a word outside the three is refused at Save"
        );
    }

    let (autopilot, approve, answer, priority) = (
        super::EDIT_HARNESS_AUTOPILOT,
        super::EDIT_HARNESS_APPROVE,
        super::EDIT_HARNESS_ANSWER_QUESTIONS,
        super::EDIT_HARNESS_QUESTION_PRIORITY,
    );
    for (line, key, seeded, limits) in [
        ("autopilot = \"no\"", autopilot, Some("off"), true),
        ("autopilot = \"sometimes\"", autopilot, None, false),
        ("approve = \"no\"", approve, Some("none"), true),
        ("approve = \"most\"", approve, None, false),
        ("answer_questions = 0", answer, Some("false"), true),
        (
            "answer_questions = \"sometimes\"",
            answer,
            Some("true"),
            false,
        ),
        ("question_priority = \"sometimes\"", priority, None, false),
    ] {
        let cfg = Config::parse(&format!("[harness]\n{line}\n")).expect("parses");
        assert_eq!(seed(&cfg, key).as_deref(), seeded, "{line}");
        assert_eq!(cfg.harness.policy != full, limits, "{line}");
    }
}

#[test]
fn each_ai_use_switch_round_trips_through_set_and_unset() {
    use crate::app_config::AiPolicy;
    let field = |cfg: &Config, key: &str| {
        editable_fields(cfg)
            .into_iter()
            .find(|f| f.key == key)
            .unwrap_or_else(|| panic!("{key}: no row"))
    };
    let seed = |cfg: &Config, key: &str| field(cfg, key).seed;
    let ids = aterm_agent::ai::connector::ids();
    let key_of = |id: &str| format!("ai.{id}.use");
    for id in ids.iter().copied() {
        let key = key_of(id);
        let key = key.as_str();
        let cli = aterm_agent::ai::connector::connector(id).expect("a connection");
        let row = super::NESTED_LEAVES
            .iter()
            .find(|leaf| leaf.key == key)
            .unwrap_or_else(|| panic!("{key}: no Settings row for {id}"));
        assert_eq!(
            row.label,
            format!("Use {} on this machine", cli.product),
            "{key}"
        );
        assert_eq!(super::ai_use_switch(key).map(|c| c.id), Some(id), "{key}");
        assert_eq!(super::section_of(key), super::Section::Harness, "{key}");
        assert_eq!(super::group_of(key), ("AI CLIs", 1), "{key}");
        assert_eq!(super::application_timing(key), None, "{key}: live");
        assert!(super::application_has_live_effect(key), "{key}");
        assert_eq!(super::footnote_of(key), Some(super::AI_USE_NOTE), "{key}");
        assert!(!keywords_of(key).is_empty(), "{key}");
        assert_eq!(super::edit_kind(key), EditKind::Bool, "{key}");
        assert_eq!(seed(&Config::default(), key).as_deref(), Some("true"));

        let written = apply_prefs_edits("font_px = 14\n", &[(key, set("false"))]).expect("set");
        assert!(
            written.contains("font_px = 14")
                && written.contains(&format!("[ai.{id}]\nuse = false")),
            "{written}"
        );
        let cfg = Config::parse(&written).expect("re-parses as Config");
        assert!(cfg.ai.notes.is_empty(), "{:?}", cfg.ai.notes);
        assert!(!cfg.ai.settings().wants(id), "{written}");
        assert_eq!(seed(&cfg, key).as_deref(), Some("false"), "{written}");
        for other in ids.iter().copied().filter(|other| *other != id) {
            assert!(cfg.ai.settings().wants(other), "{other}: {written}");
            assert_eq!(
                seed(&cfg, &key_of(other)).as_deref(),
                Some("true"),
                "{other}"
            );
        }

        let cleared = apply_prefs_edits(&written, &[(key, None)]).expect("unset");
        assert!(!cleared.contains("[ai"), "no empty table left: {cleared}");
        let back = Config::parse(&cleared).expect("re-parses as Config");
        assert!(back.ai.settings().wants(id), "{cleared}");
        assert_eq!(seed(&back, key).as_deref(), Some("true"), "{cleared}");
    }
    for key in [
        "ai.nonesuch.use",
        "ai.codex.enabled",
        "ai.use",
        "harness.codex.enabled",
        super::EDIT_AI_ENABLED,
        super::EDIT_AI_PREFERRED,
    ] {
        assert!(super::ai_use_switch(key).is_none(), "{key}");
    }

    let preferred = super::EDIT_AI_PREFERRED;
    assert_eq!(
        super::edit_kind(preferred),
        EditKind::Enum {
            options: &aterm_agent::ai::connector::IDS
        }
    );
    let default = aterm_agent::ai::config::default_preferred().id;
    let bare = field(&Config::default(), preferred);
    assert_eq!(bare.seed, None);
    assert_eq!(bare.placeholder, format!("{default} (default)"));
    let written = apply_prefs_edits("", &[(preferred, set("codex"))]).expect("set");
    let cfg = Config::parse(&written).expect("re-parses as Config");
    assert_eq!(
        cfg.ai.settings().preferred.1,
        aterm_agent::ai::config::From::Local
    );
    assert_eq!(seed(&cfg, preferred).as_deref(), Some("codex"));
    let written = apply_prefs_edits("", &[(super::EDIT_AI_ENABLED, set("false"))]).expect("set");
    let cfg = Config::parse(&written).expect("re-parses as Config");
    assert!(!cfg.ai.enabled());
    assert_eq!(seed(&cfg, super::EDIT_AI_ENABLED).as_deref(), Some("false"));
    assert_eq!(
        seed(&Config::default(), super::EDIT_AI_ENABLED).as_deref(),
        Some("true")
    );

    let scratch = aterm_tempfile::tempdir().expect("scratch folder");
    let dir = scratch.path();
    crate::test_scratch::write_config(
        &dir.join(aterm_agent::ai::config::FLEET_FILE),
        "schema = 1\npreferred = \"codex\"\n[muse]\nuse = false\n",
    );
    let over = |text: &str| {
        let mut cfg = Config::parse(text).expect("parses");
        cfg.ai = AiPolicy::read_beside(text, Some(dir));
        cfg
    };
    let fleet = over("");
    assert_eq!(
        fleet.ai.settings().preferred.1,
        aterm_agent::ai::config::From::Fleet
    );
    assert_eq!(seed(&fleet, preferred).as_deref(), Some("codex"));
    assert_eq!(seed(&fleet, &key_of("muse")).as_deref(), Some("false"));
    assert_eq!(
        fleet.path_feed_paths(),
        vec![dir.join(aterm_agent::ai::config::FLEET_FILE)]
    );
    let local = over("[ai]\npreferred = \"claude\"\n[ai.muse]\nuse = true\n");
    assert_eq!(seed(&local, preferred).as_deref(), Some("claude"));
    assert_eq!(seed(&local, &key_of("muse")).as_deref(), Some("true"));
    let reset =
        apply_prefs_edits("[ai]\npreferred = \"claude\"\n", &[(preferred, None)]).expect("unset");
    assert_eq!(
        seed(&over(&reset), preferred).as_deref(),
        Some("codex"),
        "{reset}"
    );
    #[cfg(unix)]
    {
        let path = dir.join(aterm_agent::ai::config::FLEET_FILE);
        std::fs::remove_file(&path).expect("remove own fleet fixture");
        std::os::unix::fs::symlink(dir.join("missing-fleet"), &path).expect("dangling fleet entry");
        let unknown = over("[ai]\nreviewers = [\"codex\"]\n");
        assert!(matches!(
            unknown.ai.settings().fleet,
            aterm_agent::ai::config::FleetState::Refused { .. }
        ));
        assert_eq!(
            unknown.ai.settings().reviewers.1,
            aterm_agent::ai::config::From::Local,
            "a refused document applies nothing: this machine's list stands"
        );
        assert!(unknown.path_feed_paths().contains(&path));
    }
}

#[test]
fn each_agent_switch_round_trips_through_set_and_unset() {
    use aterm_agent::supervise::SupervisorConfig;
    use aterm_phase::Program;
    let seed = |cfg: &Config, key: &str| {
        editable_fields(cfg)
            .into_iter()
            .find(|f| f.key == key)
            .expect("the row exists")
            .seed
    };
    let full = SupervisorConfig::default();
    let agents: Vec<Program> = Program::ALL
        .into_iter()
        .filter(|p| p.harness_table())
        .collect();
    let key_of = |agent: Program| format!("harness.{}.enabled", agent.name());
    for agent in agents.iter().copied() {
        let key = key_of(agent);
        let key = key.as_str();
        let row = super::NESTED_LEAVES
            .iter()
            .find(|leaf| leaf.key == key)
            .unwrap_or_else(|| panic!("{key}: no Settings row for {agent:?}"));
        assert_eq!(
            row.label,
            super::agent_switch_label(agent.product()),
            "{key}"
        );
        assert_eq!(super::agent_switch(key), Some(agent), "{key}");
        assert_eq!(super::section_of(key), super::Section::Harness, "{key}");
        assert_eq!(super::application_timing(key), None, "{key}: live");
        assert!(super::application_has_live_effect(key), "{key}");
        assert!(!keywords_of(key).is_empty(), "{key}");
        assert_eq!(
            super::footnote_of(key),
            Some(super::HARNESS_AGENT_NOTE),
            "{key}"
        );
        assert_eq!(super::edit_kind(key), EditKind::Bool, "{key}");
        assert_eq!(seed(&Config::default(), key).as_deref(), Some("true"));

        let written = apply_prefs_edits("font_px = 14\n", &[(key, set("false"))]).expect("set");
        assert!(
            written.contains("font_px = 14")
                && written.contains(&format!("[harness.{}]\nenabled = false", agent.name())),
            "{written}"
        );
        let cfg = Config::parse(&written).expect("re-parses as Config");
        assert!(cfg.harness_notices().is_empty(), "{written}");
        assert!(cfg.harness.notes.is_empty(), "{:?}", cfg.harness.notes);
        let policy = &cfg.harness.policy;
        assert!(!policy.on_for(agent), "{written}");
        assert!(policy.enabled, "{written}");
        assert_eq!(seed(&cfg, key).as_deref(), Some("false"), "{written}");
        for other in agents.iter().copied().filter(|other| *other != agent) {
            assert!(policy.on_for(other), "{other:?}: {written}");
            assert_eq!(
                seed(&cfg, &key_of(other)).as_deref(),
                Some("true"),
                "{other:?}: {written}"
            );
        }

        let cleared = apply_prefs_edits(&written, &[(key, None)]).expect("unset");
        assert!(
            !cleared.contains("[harness"),
            "no empty table left: {cleared}"
        );
        let back = Config::parse(&cleared).expect("re-parses as Config");
        assert_eq!(back.harness.policy, full, "{cleared}");
        assert!(back.harness.policy.on_for(agent), "{cleared}");
        assert_eq!(seed(&back, key).as_deref(), Some("true"), "{cleared}");
    }
    for key in [
        super::EDIT_HARNESS_ENABLED,
        "harness.generic.enabled",
        "harness.claude-code.enabled",
        "harness.disk.enabled",
        "harness.claude.approve",
        "claude.enabled",
    ] {
        assert_eq!(super::agent_switch(key), None, "{key}");
    }
    let (claude, codex) = (
        super::EDIT_HARNESS_CLAUDE_ENABLED,
        super::EDIT_HARNESS_CODEX_ENABLED,
    );

    for text in [
        "[harness.codex]\nenabled = false\n",
        "[harness.codex]\nenabled = \"no\"\n",
        "[harness]\ncodex.enabled = false\n",
    ] {
        let cfg = Config::parse(text).expect("parses");
        assert_eq!(seed(&cfg, codex).as_deref(), Some("false"), "{text}");
        assert_eq!(seed(&cfg, claude).as_deref(), Some("true"), "{text}");
        assert_eq!(
            seed(&cfg, super::EDIT_HARNESS_ENABLED).as_deref(),
            Some("true"),
            "{text}"
        );
    }
    let off = Config::parse("[harness]\nenabled = false\n").expect("parses");
    for agent in agents {
        assert!(!off.harness.policy.on_for(agent), "{agent:?}");
        assert_eq!(
            seed(&off, &key_of(agent)).as_deref(),
            Some("true"),
            "{agent:?}"
        );
    }
}

#[test]
fn packages_consent_rows_surface_in_the_settings_model() {
    for key in [
        super::EDIT_PACKAGES_ENABLED,
        super::EDIT_PACKAGES_AUTO_INSTALL,
    ] {
        assert_eq!(super::edit_kind(key), EditKind::Bool, "{key} types Bool");
    }

    let fields = editable_fields(&Config::default());
    for (retired, _) in super::RETIRED_PACKAGES_SPELLINGS {
        assert!(
            fields.iter().all(|f| f.key != *retired),
            "{retired} is retired: no row writes it"
        );
    }
    let enabled = fields
        .iter()
        .find(|f| f.key == super::EDIT_PACKAGES_ENABLED)
        .expect("Automatic updates row exists");
    assert_eq!(enabled.seed.as_deref(), Some("true"), "default ON");
    let auto_install = fields
        .iter()
        .find(|f| f.key == super::EDIT_PACKAGES_AUTO_INSTALL)
        .expect("install consent row exists");
    assert_eq!(
        auto_install.seed.as_deref(),
        Some("true"),
        "batteries included: default ON"
    );

    let seed = |text: &str, key: &str| {
        editable_fields(&aterm_toml::from_str::<Config>(text).unwrap())
            .into_iter()
            .find(|f| f.key == key)
            .unwrap()
            .seed
    };
    assert_eq!(
        seed(
            "[packages]\nauto_update = false\n",
            super::EDIT_PACKAGES_ENABLED
        )
        .as_deref(),
        Some("false"),
        "the retired auto_update = false seeds the switch off"
    );
    assert_eq!(
        seed(
            "[packages]\nseed_install = false\n",
            super::EDIT_PACKAGES_AUTO_INSTALL
        )
        .as_deref(),
        Some("false"),
        "the retired seed_install = false seeds the consent off"
    );
    assert_eq!(
        seed(
            "[packages]\nenabled = false\nauto_install = true\n",
            super::EDIT_PACKAGES_ENABLED
        )
        .as_deref(),
        Some("false")
    );
}

#[test]
fn the_update_and_reroute_switches_are_rows_and_the_repoint_is_not() {
    let fields = editable_fields(&Config::default());
    for key in [super::EDIT_UPDATE_ENABLED, super::EDIT_REROUTE_ANNOUNCE] {
        assert_eq!(super::edit_kind(key), EditKind::Bool, "{key}");
        let row = fields.iter().find(|f| f.key == key).expect("a row");
        assert_eq!(row.seed.as_deref(), Some("true"), "{key} defaults ON");
        assert_eq!(super::environment_precedence(key), None, "{key}");
    }
    for key in [
        "update.owner",
        "update.repo",
        "update.require_team_id",
        "packages.account",
    ] {
        assert!(fields.iter().all(|f| f.key != key), "{key} is not a row");
        assert_eq!(super::environment_precedence(key), None, "{key}");
    }
    let off: Config =
        aterm_toml::from_str("[update]\nenabled = false\n[reroute]\nannounce = false\n").unwrap();
    let fields = editable_fields(&off);
    for key in [super::EDIT_UPDATE_ENABLED, super::EDIT_REROUTE_ANNOUNCE] {
        let row = fields.iter().find(|f| f.key == key).unwrap();
        assert_eq!(row.seed.as_deref(), Some("false"), "{key}");
    }
}

#[test]
fn packages_dotted_writes_preserve_the_atpkg_owned_siblings() {
    let existing = "# consent ledger\n[packages]\naccount = \"alabsystems\" # index owner\nexclude = [\"trust\"]\nseed_install = false\nauto_update = false\n\n[packages.links]\nay = \"~/ay\"\n";
    let out = apply_prefs_edits(
        existing,
        &[(super::EDIT_PACKAGES_AUTO_INSTALL, set("true"))],
    )
    .expect("dotted write");
    assert!(out.contains("# consent ledger"), "comment survives");
    assert!(
        out.contains("account = \"alabsystems\" # index owner"),
        "atpkg-owned sibling + inline note survive: {out:?}"
    );
    assert!(out.contains("[packages.links]") && out.contains("ay = \"~/ay\""));
    assert!(out.contains("auto_install = true"));
    assert!(
        !out.contains("seed_install"),
        "writing the consent drops its retired spelling: {out:?}"
    );
    assert!(
        out.contains("auto_update = false"),
        "an unrelated retired key is left for its own switch: {out:?}"
    );
    let config: Config = aterm_toml::from_str(&out).expect("round-trips through serde");
    assert!(config.packages_auto_install());
    assert!(
        !config.packages_enabled(),
        "auto_update = false still reads as off"
    );
    let on = apply_prefs_edits(&out, &[(super::EDIT_PACKAGES_ENABLED, set("true"))])
        .expect("the switch");
    assert!(!on.contains("auto_update"), "{on:?}");
    let config: Config = aterm_toml::from_str(&on).unwrap();
    assert!(
        config.packages_enabled(),
        "the switch decides once it is written"
    );

    let reverted = apply_prefs_edits(&out, &[(super::EDIT_PACKAGES_AUTO_INSTALL, None)])
        .expect("dotted revert");
    assert!(!reverted.contains("auto_install"));
    assert!(
        reverted.contains("[packages]") && reverted.contains("account = \"alabsystems\""),
        "atpkg-owned keys keep the table alive: {reverted:?}"
    );
    let config: Config = aterm_toml::from_str(&reverted).unwrap();
    assert!(
        config.packages_auto_install(),
        "back to the batteries-included default"
    );

    assert!(matches!(
        apply_prefs_edits("", &[(super::EDIT_PACKAGES_ENABLED, set("yes"))]),
        Err(PrefsEditError::BadValue { .. })
    ));
}

#[test]
fn removing_a_key_keeps_the_comment_lines_above_it() {
    for (existing, key, expected) in [
        (
            "# keep me\nfont_px = 14.0  # cozy\nfont_family = \"Menlo\"\n",
            EDIT_FONT_PX,
            "# keep me\nfont_family = \"Menlo\"\n",
        ),
        (
            "[matrix_rain]\n# mine\nenabled = true\nfps = 24\n",
            super::EDIT_MATRIX_RAIN_ENABLED,
            "[matrix_rain]\n# mine\nfps = 24\n",
        ),
        (
            "# rain\nmatrix_rain.enabled = true\nfont_px = 13.0\n",
            super::EDIT_MATRIX_RAIN_ENABLED,
            "# rain\nfont_px = 13.0\n",
        ),
        (
            "font_px = 13.0\n# alt themes\ntheme = \"X\"\n\n[update]\nenabled = false\n",
            EDIT_THEME,
            "font_px = 13.0\n# alt themes\n\n[update]\nenabled = false\n",
        ),
    ] {
        let out = apply_prefs_edits(existing, &[(key, None)]).expect("remove");
        assert_eq!(out, expected, "removing {key} from {existing:?}");
        aterm_toml::from_str::<Config>(&out).expect("the result still parses");
    }
}

#[test]
fn reset_then_undo_keeps_the_comment_lines_above_the_key() {
    use crate::native_config_service::{ConfigPatchResult, VersionedConfigService};
    let comments = "# ---- My fonts ----\n# keep these in sync with my editor\n";
    let mut service = VersionedConfigService::new(format!(
        "{comments}font_px = 14.0\nfont_family = \"Menlo\"\n"
    ))
    .unwrap();
    let base = service.snapshot().revision;
    let ConfigPatchResult::Applied { snapshot, undo } =
        service.reset_all(base, [EDIT_FONT_PX.to_string()])
    else {
        panic!("the reset should commit");
    };
    assert_eq!(
        *snapshot.text,
        format!("{comments}font_family = \"Menlo\"\n")
    );
    let ConfigPatchResult::Applied { snapshot, .. } = service.undo(undo) else {
        panic!("the undo should commit");
    };
    assert_eq!(
        *snapshot.text,
        format!("{comments}font_px = 14.0\nfont_family = \"Menlo\"\n")
    );
}

#[test]
fn preserves_comments_and_unrelated_keys() {
    let existing = "\
# my aterm config
font_px = 12.0  # cozy
gpu = true
[keybindings]
\"cmd+shift+t\" = \"new_tab\"
";
    let out = apply_prefs_edits(existing, &[(EDIT_THEME, set("Nord"))]).unwrap();
    assert!(out.contains("# my aterm config"), "{out}");
    assert!(out.contains("# cozy"), "{out}");
    assert!(out.contains("gpu = true"), "{out}");
    assert!(out.contains("[keybindings]"), "{out}");
    assert!(out.contains("\"cmd+shift+t\" = \"new_tab\""), "{out}");
    let c: Config = aterm_toml::from_str(&out).unwrap();
    assert_eq!(c.theme.as_deref(), Some("Nord"));
    assert_eq!(c.font_px, Some(12.0));
}

#[test]
fn value_edit_preserves_the_edited_keys_inline_comment() {
    let existing = "\
# header comment stays
font_px = 12.0  # cozy size
scrollback_lines = 5000 # deep history

[update]
auto_apply = true # hands off
";
    let out = apply_prefs_edits(
        existing,
        &[
            (EDIT_FONT_PX, set("18")),
            ("update.auto_apply", set("false")),
        ],
    )
    .unwrap();
    assert!(out.contains("font_px = 18  # cozy size"), "{out}");
    assert!(out.contains("auto_apply = false # hands off"), "{out}");
    assert!(out.contains("# header comment stays"), "{out}");
    assert!(
        out.contains("scrollback_lines = 5000 # deep history"),
        "{out}"
    );
    let c: Config = aterm_toml::from_str(&out).unwrap();
    assert_eq!(c.font_px, Some(18.0));
    assert_eq!(c.update.as_ref().and_then(|u| u.auto_apply), Some(false));
    assert_eq!(c.scrollback_lines, Some(5000));
}

#[test]
fn w2_w9_motion_keys_round_trip_and_surface() {
    let out = apply_prefs_edits(
        "",
        &[
            (super::EDIT_TEXT_BLENDING, set("linear")),
            (super::EDIT_FONT_THICKEN, set("true")),
            (super::EDIT_STEM_GAMMA, set("1.4")),
            (super::EDIT_FONT_WEIGHT, set("600")),
            (super::EDIT_FONT_VARIATION, set("wght=450, opsz=14")),
            (super::EDIT_MOTION, set("reduced")),
            (super::EDIT_SERIOUS_MODE, set("true")),
            (super::EDIT_LOAD_ADAPTIVE_MOTION, set("false")),
        ],
    )
    .expect("writes typed values");
    let c: Config = aterm_toml::from_str(&out).expect("round-trips through serde");
    assert_eq!(c.load_adaptive_motion, Some(false));
    assert!(!c.load_adaptive_motion_or_default());
    assert_eq!(
        c.text_blending_or_default(),
        aterm_render::TextBlending::Linear
    );
    assert!(c.font_thicken_or_default());
    assert_eq!(c.stem_gamma, Some(1.4));
    assert_eq!(c.font_weight, Some(600));
    assert_eq!(c.motion.as_deref(), Some("reduced"));
    assert_eq!(c.motion_mode(), crate::motion::MotionMode::Reduced);
    assert_eq!(c.serious_mode, Some(true));
    assert!(c.serious_mode_or_default());
    assert_eq!(
        c.font_variation.as_ref().map(|l| l.0.clone()),
        Some(vec!["wght=450".to_string(), "opsz=14".to_string()])
    );
    let arr: Config = aterm_toml::from_str("font_variation = [\"wght=450\", \"opsz=14\"]").unwrap();
    assert_eq!(
        arr.font_variation.map(|l| l.0),
        c.font_variation.as_ref().map(|l| l.0.clone())
    );
    let (reqs, warns) = c.font_variation_requests();
    assert!(warns.is_empty(), "clean specs warn nothing");
    assert!(
        reqs.iter().any(|&(t, _)| t == u32::from_be_bytes(*b"opsz")),
        "opsz axis reaches the engine"
    );
    assert_eq!(
        reqs.last().map(|&(t, v)| (t, v)),
        Some((aterm_render::variation::WGHT_TAG, 600.0)),
        "font_weight is appended last so it wins on conflict"
    );

    let fields = editable_fields(&c);
    let row = fields
        .iter()
        .find(|f| f.key == super::EDIT_FONT_VARIATION)
        .unwrap();
    assert_eq!(row.seed.as_deref(), Some("wght=450, opsz=14"));
}

#[test]
fn types_each_field_correctly() {
    let out = apply_prefs_edits(
        "",
        &[
            (EDIT_FONT_PX, set("14")),
            (EDIT_SCROLLBACK, set("50000")),
            (EDIT_COPY_ON_SELECT, set("true")),
            (EDIT_THEME, set("Dracula")),
            (EDIT_FONT_FAMILY, set("JetBrains Mono")),
            (EDIT_CURSOR_STYLE, set("bar")),
        ],
    )
    .unwrap();
    let c: Config = aterm_toml::from_str(&out).expect("typed values round-trip");
    assert_eq!(c.font_px, Some(14.0)); // float, even from "14"
    assert_eq!(c.scrollback_lines, Some(50000)); // integer
    assert_eq!(c.copy_on_select, Some(true)); // bool
    assert_eq!(c.theme.as_deref(), Some("Dracula")); // string
    assert_eq!(c.font_family.as_deref(), Some("JetBrains Mono")); // string w/ space
    assert_eq!(c.cursor_style.as_deref(), Some("bar")); // string
    assert!(out.contains("font_px = 14"), "float unquoted: {out}");
    assert!(
        out.contains("scrollback_lines = 50000"),
        "int unquoted: {out}"
    );
    assert!(
        out.contains("copy_on_select = true"),
        "bool unquoted: {out}"
    );
}

#[test]
fn bad_numeric_value_is_rejected() {
    let err = apply_prefs_edits("", &[(EDIT_FONT_PX, set("not-a-number"))]).unwrap_err();
    match err {
        PrefsEditError::BadValue { key, .. } => assert_eq!(key, EDIT_FONT_PX),
        other => panic!("expected BadValue, got {other:?}"),
    }
}

#[test]
fn font_px_outside_the_resolvers_range_is_rejected_with_the_range() {
    let range = format!("{}..={}", crate::FONT_PX_MIN, crate::FONT_PX_MAX);
    for raw in [
        "0", "500", "-5", "0.0", "200.5", "5.99", "NaN", "inf", "-inf",
    ] {
        let err = apply_prefs_edits("theme = \"Nord\"\n", &[(EDIT_FONT_PX, set(raw))]).unwrap_err();
        match &err {
            PrefsEditError::OutOfRange { key, range: got } => {
                assert_eq!(key, EDIT_FONT_PX, "{raw}");
                assert_eq!(got, &range, "{raw}");
            }
            other => panic!("{raw}: expected OutOfRange, got {other:?}"),
        }
        assert_eq!(
            err.to_string(),
            format!("invalid value for font_px: must be {range}"),
            "{raw}: the wire reply is `ERR ` + this"
        );
    }
    for raw in ["6", "200", "14.5", "16"] {
        let out = apply_prefs_edits("", &[(EDIT_FONT_PX, set(raw))])
            .unwrap_or_else(|e| panic!("{raw} is inside the range: {e}"));
        let c: Config = aterm_toml::from_str(&out).expect("round-trips");
        assert_eq!(c.font_px, Some(raw.parse::<f32>().unwrap()), "{raw}");
    }
    assert!(apply_prefs_edits("", &[(super::EDIT_LINE_HEIGHT, set("0"))]).is_ok());
}

#[test]
fn float_for_integer_field_is_rejected() {
    for raw in ["1.5", "1.0000000000000001", "0.99999999999999999", "1e-400"] {
        let err = apply_prefs_edits("", &[(EDIT_SCROLLBACK, set(raw))]).unwrap_err();
        assert!(
            matches!(err, PrefsEditError::BadValue { .. }),
            "{raw}: {err:?}"
        );
    }
    for (raw, expected) in [
        ("5000.0", 5000i64),
        ("5e3", 5000),
        ("50e-1", 5),
        ("9007199254740993.0", 9_007_199_254_740_993),
        ("9223372036854775807.0", i64::MAX),
        ("-9223372036854775808.0", i64::MIN),
        ("0e999999999999999999999", 0),
    ] {
        let out = apply_prefs_edits("", &[(super::EDIT_ADJUST_BASELINE, set(raw))]).unwrap();
        let doc = out.parse::<aterm_toml::edit::DocumentMut>().unwrap();
        assert_eq!(
            doc[super::EDIT_ADJUST_BASELINE].as_integer(),
            Some(expected),
            "{raw}: {out}"
        );
    }
    for raw in [
        "9223372036854775808.0",
        "-9223372036854775809.0",
        "1e400",
        "1e+",
        ".",
    ] {
        assert!(
            apply_prefs_edits("", &[(super::EDIT_ADJUST_BASELINE, set(raw))]).is_err(),
            "{raw}"
        );
    }
}

fn written_spelling(toml: &str, key: &str) -> String {
    let doc = toml
        .parse::<aterm_toml::edit::DocumentMut>()
        .expect("written TOML parses");
    let mut item = doc.as_item();
    for part in key.split('.') {
        item = item
            .get(part)
            .unwrap_or_else(|| panic!("{key} is written: {toml}"));
    }
    item.as_value()
        .unwrap_or_else(|| panic!("{key} is a value: {toml}"))
        .to_string()
        .trim()
        .to_string()
}

#[test]
fn a_number_is_written_as_its_kind_types_it() {
    for (key, raw, spelled) in [
        (EDIT_WINDOW_PADDING, "20", "20"),
        (EDIT_WINDOW_PADDING_TOP, "4", "4"),
        (EDIT_FONT_PX, "15", "15"),
        (EDIT_FONT_PX, "15.0", "15"),
        (EDIT_FONT_PX, "1.5e1", "15"),
        (EDIT_FONT_PX, "13.25", "13.25"),
        (EDIT_LINE_HEIGHT, "1.15", "1.15"),
        (EDIT_LINE_HEIGHT, "1.0", "1"),
        (EDIT_BACKGROUND_OPACITY, "0.85", "0.85"),
        (EDIT_FONT_WEIGHT_DARK_NUDGE, "120", "120"),
        ("sparkle_words.ink.strength", "1", "1"),
        ("sparkle_words.ink.strength", "0.35", "0.35"),
        (EDIT_SCROLLBACK, "5000", "5000"),
        (EDIT_SCROLLBACK, "5000.0", "5000"),
        (EDIT_FONT_WEIGHT, "4e2", "400"),
    ] {
        let out = apply_prefs_edits("", &[(key, set(raw))])
            .unwrap_or_else(|e| panic!("{key} = {raw}: {e}"));
        assert_eq!(written_spelling(&out, key), spelled, "{key} = {raw}: {out}");
        let config = Config::parse(&out).unwrap_or_else(|e| panic!("{out}: {e}"));
        let seed = editable_fields(&config)
            .into_iter()
            .find(|field| field.key == key)
            .and_then(|field| field.seed)
            .unwrap_or_else(|| panic!("{key} reads back: {out}"));
        assert_eq!(
            seed.parse::<f32>().ok(),
            raw.parse::<f32>().ok(),
            "{key} = {raw} reads back as written: {out}"
        );
    }

    let fields = editable_fields(&Config::default());
    let floats = fields
        .iter()
        .filter(|field| field.kind == EditKind::Float)
        .collect::<Vec<_>>();
    assert!(floats.len() >= 15, "the fractional settings are found");
    for field in floats {
        let range = range_of(field.key).unwrap_or(Range {
            min: 0.0,
            max: 100.0,
            step: 0.5,
        });
        let whole = range.max.floor();
        let fraction = (((range.min + range.max) * 50.0).round() + 1.0) / 100.0;
        assert!(fraction.fract() != 0.0, "{}: {fraction}", field.key);
        for (value, spelled) in [
            (whole, format!("{whole:.0}")),
            (fraction, fraction.to_string()),
        ] {
            let out = apply_prefs_edits("", &[(field.key, set(&value.to_string()))])
                .unwrap_or_else(|e| panic!("{} = {value}: {e}", field.key));
            assert_eq!(
                written_spelling(&out, field.key),
                spelled,
                "{}: {out}",
                field.key
            );
            let config = Config::parse(&out).unwrap_or_else(|e| panic!("{out}: {e}"));
            let seed = editable_fields(&config)
                .into_iter()
                .find(|read| read.key == field.key)
                .and_then(|read| read.seed)
                .unwrap_or_else(|| panic!("{} reads back: {out}", field.key));
            assert_eq!(
                seed.parse::<f32>().ok(),
                value.to_string().parse::<f32>().ok(),
                "{} = {value} reads back as written: {out}",
                field.key
            );
        }
    }

    let old = "window_padding = 20.0\nfont_px = 15.0\n";
    let config = Config::parse(old).expect("an old float file reads");
    assert_eq!(
        (config.window_padding, config.font_px),
        (Some(20.0), Some(15.0))
    );
    let out = apply_prefs_edits(old, &[(EDIT_LINE_HEIGHT, set("1.2"))]).unwrap();
    assert_eq!(
        out,
        "window_padding = 20.0\nfont_px = 15.0\nline_height = 1.2\n"
    );
    let out = apply_prefs_edits(old, &[(EDIT_WINDOW_PADDING, set("24"))]).unwrap();
    assert_eq!(out, "window_padding = 24\nfont_px = 15.0\n");
}

#[test]
fn a_number_setting_refuses_a_value_that_is_not_finite() {
    let fields = editable_fields(&Config::default());
    let numbers = fields
        .iter()
        .filter(|field| matches!(field.kind, EditKind::Float | EditKind::Integer))
        .collect::<Vec<_>>();
    assert!(numbers.len() >= 30, "the number settings are found");
    for field in numbers {
        for raw in ["nan", "NaN", "inf", "-inf", "infinity", "1e400", "wide"] {
            let err = apply_prefs_edits("font_px = 15.0\n", &[(field.key, set(raw))])
                .expect_err(&format!("{} = {raw} must be refused", field.key));
            if field.key == EDIT_FONT_PX && raw.parse::<f64>().is_ok() {
                assert!(
                    matches!(err, PrefsEditError::OutOfRange { .. }),
                    "{raw}: {err:?}"
                );
                continue;
            }
            assert!(
                matches!(err, PrefsEditError::BadValue { .. }),
                "{} = {raw}: {err:?}",
                field.key
            );
            if field.kind == EditKind::Float {
                assert!(
                    err.to_string().ends_with("; it must be a number"),
                    "{} = {raw}: {err}",
                    field.key
                );
            }
        }
    }
}

#[test]
fn malformed_existing_file_is_refused() {
    let err = apply_prefs_edits("this = = broken", &[(EDIT_THEME, set("Nord"))]).unwrap_err();
    assert!(matches!(err, PrefsEditError::Parse(_)));
}

#[test]
fn batched_set_update_remove() {
    let existing = "font_px = 12.0\ntheme = \"Dracula\"\ngpu = true\n";
    let out = apply_prefs_edits(
        existing,
        &[
            (EDIT_FONT_PX, set("20.0")), // update
            (EDIT_THEME, None),          // remove
            (EDIT_SCROLLBACK, set("0")), // set new (0 = unlimited)
        ],
    )
    .unwrap();
    let c: Config = aterm_toml::from_str(&out).unwrap();
    assert_eq!(c.font_px, Some(20.0));
    assert_eq!(c.theme, None);
    assert_eq!(c.scrollback_lines, Some(0));
    assert!(out.contains("gpu = true"), "unrelated key survives: {out}");
}

#[test]
fn split_theme_string_round_trips() {
    let out =
        apply_prefs_edits("", &[(EDIT_THEME, set("dark:Dracula,light:GitHub Light"))]).unwrap();
    let c: Config = aterm_toml::from_str(&out).unwrap();
    assert_eq!(c.theme.as_deref(), Some("dark:Dracula,light:GitHub Light"));
}

#[test]
fn enum_value_is_validated_and_canonicalised() {
    let out = apply_prefs_edits("", &[(EDIT_CURSOR_TRAIL_STYLE, set("Phaser"))]).unwrap();
    assert!(
        out.contains("cursor_trail_style = \"phaser\""),
        "canonical lower-case spelling written: {out}"
    );
    let c: Config = aterm_toml::from_str(&out).unwrap();
    assert_eq!(c.cursor_trail_style.as_deref(), Some("phaser"));
    let out = apply_prefs_edits("", &[(EDIT_CURSOR_TRAIL_STYLE, set("fire"))]).unwrap();
    let c: Config = aterm_toml::from_str(&out).unwrap();
    assert_eq!(c.cursor_trail_style.as_deref(), Some("fire"));
}

#[test]
fn color_value_is_validated_on_save() {
    let out = apply_prefs_edits("", &[(EDIT_FOREGROUND, set("#1E2030"))]).unwrap();
    let c: Config = aterm_toml::from_str(&out).unwrap();
    assert_eq!(c.foreground.as_deref(), Some("#1E2030"));
    let err = apply_prefs_edits("", &[(EDIT_BACKGROUND, set("not-a-color"))]).unwrap_err();
    match err {
        PrefsEditError::BadValue { key, .. } => assert_eq!(key, EDIT_BACKGROUND),
        other => panic!("expected BadValue, got {other:?}"),
    }
}

#[test]
fn the_palette_permission_seeds_on_and_the_rest_off() {
    let seed = |cfg: &Config, key: &str| {
        super::editable_fields(cfg)
            .into_iter()
            .find(|f| f.key == key)
            .and_then(|f| f.seed)
            .unwrap_or_else(|| panic!("{key} has a seeded row"))
    };
    let unset = Config::default();
    for &k in super::SECURITY_BOOL_KEYS {
        let want = if k == super::EDIT_ALLOW_PALETTE_RECONFIGURE {
            "true"
        } else {
            "false"
        };
        assert_eq!(seed(&unset, k), want, "{k}");
    }
    let off: Config = aterm_toml::from_str("allow_palette_reconfigure = false").unwrap();
    assert_eq!(seed(&off, super::EDIT_ALLOW_PALETTE_RECONFIGURE), "false");
}

#[test]
fn scalar_schema_keys_classify_and_round_trip() {
    for k in super::SECURITY_BOOL_KEYS {
        assert!(
            matches!(super::edit_kind(k), super::EditKind::Bool),
            "{k} is Bool"
        );
    }
    for k in [
        super::EDIT_COLUMNS,
        super::EDIT_LINES,
        super::EDIT_TAB_STRIP_ROWS,
        super::EDIT_SEARCH_HISTORY_LINES,
    ] {
        assert!(
            matches!(super::edit_kind(k), super::EditKind::Integer),
            "{k} is Integer"
        );
    }
    let out = apply_prefs_edits(
        "",
        &[
            (super::EDIT_ALLOW_OSC52_QUERY, set("true")),
            (super::EDIT_COLUMNS, set("100")),
        ],
    )
    .unwrap();
    let c: Config = aterm_toml::from_str(&out).unwrap();
    assert_eq!(c.allow_osc52_query, Some(true)); // TOML bool, not a string
    assert_eq!(c.columns, Some(100)); // TOML int, not a string
}

#[test]
fn enum_domain_keys_classify_and_round_trip_canonical() {
    let cases: [(&str, &[&str]); 7] = [
        (EDIT_CURSOR_STYLE, super::CURSOR_STYLES),
        (super::EDIT_WINDOW_THEME, super::WINDOW_THEMES),
        (super::EDIT_PALETTE_GENERATE, super::PALETTE_GENERATE_MODES),
        (super::EDIT_BIDI, super::BIDI_MODES),
        (super::EDIT_AMBIGUOUS_WIDTH, super::AMBIGUOUS_WIDTHS),
        (super::EDIT_TRAIL_SOUND_STYLE, super::TRAIL_SOUND_STYLES),
        (
            super::EDIT_MACHINE_UNIVERSAL_CONTROL,
            super::UNIVERSAL_CONTROL_POLICIES,
        ),
    ];
    for (key, opts) in cases {
        match super::edit_kind(key) {
            super::EditKind::Enum { options } => assert_eq!(options, opts, "{key} options"),
            other => panic!("{key} should be Enum, got {other:?}"),
        }
        for &o in opts {
            let out = apply_prefs_edits("", &[(key, set(o))]).unwrap();
            let (table, leaf) = key
                .rsplit_once('.')
                .map_or((None, key), |(t, l)| (Some(t), l));
            assert!(
                out.contains(&format!("{leaf} = \"{o}\"")),
                "{key}={o} writes its canonical string: {out}"
            );
            if let Some(table) = table {
                assert!(out.contains(&format!("[{table}]")), "{key}={o}: {out}");
            }
        }
    }
}

#[test]
fn bracket_prefixed_comma_lists_keep_their_literal_members() {
    let out = apply_prefs_edits("", &[(super::EDIT_SHELL_ARGS, set("[working], --flag"))]).unwrap();
    let config: Config = aterm_toml::from_str(&out).unwrap();
    assert_eq!(
        config.shell_args,
        Some(vec!["[working]".to_string(), "--flag".to_string()])
    );
    let out = apply_prefs_edits(
        "",
        &[(super::EDIT_SHELL_ARGS, set(r#"["one,two", "three"]"#))],
    )
    .unwrap();
    let config: Config = aterm_toml::from_str(&out).unwrap();
    assert_eq!(
        config.shell_args,
        Some(vec!["one,two".to_string(), "three".to_string()])
    );
    assert!(apply_prefs_edits("", &[(super::EDIT_SHELL_ARGS, set("[1]"))]).is_err());
}

#[test]
fn windows_paths_round_trip_through_scalar_and_list_rows() {
    let bold = r"C:\Users\example\AppData\Local\Microsoft\Windows\Fonts\Cascadia.ttf";
    let fallback_a = r"C:\Windows\Fonts\seguiemj.ttf";
    let fallback_b = r"\\fileserver\share\fonts\Sarasa-Mono.ttf";
    let shell = r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe";
    let script = r"C:\Users\example\.config\aterm\profile.ps1";

    let out = apply_prefs_edits(
        "",
        &[
            (super::EDIT_FONT_FAMILY_BOLD, set(bold)),
            (
                super::EDIT_FALLBACK_FONTS,
                set(&format!("{fallback_a}, {fallback_b}")),
            ),
            (super::EDIT_SHELL, set(shell)),
            (super::EDIT_SHELL_ARGS, set(script)),
        ],
    )
    .expect("native Windows paths are writable Settings values");

    let cfg: Config = aterm_toml::from_str(&out)
        .unwrap_or_else(|error| panic!("Windows-path TOML re-parses: {error}\n{out}"));
    assert_eq!(cfg.font_family_bold.as_deref(), Some(bold));
    assert_eq!(cfg.shell.as_deref(), Some(shell));
    assert_eq!(
        cfg.fallback_fonts.map(|list| list.0),
        Some(vec![fallback_a.to_string(), fallback_b.to_string()]),
        "the comma-joined FontList keeps every entry, UNC prefix included"
    );
    assert_eq!(
        cfg.shell_args,
        Some(vec![script.to_string()]),
        "the LIST_KEYS array branch writes a survivable path too"
    );

    let again = apply_prefs_edits(&out, &[(super::EDIT_FONT_PX, set("14.0"))])
        .expect("a Windows-path config is editable again, not wedged");
    let cfg: Config = aterm_toml::from_str(&again).expect("second Save re-parses");
    assert_eq!(cfg.font_family_bold.as_deref(), Some(bold));
    assert_eq!(cfg.font_px, Some(14.0));
}

#[test]
fn editable_fields_seed_from_config() {
    let c: Config = aterm_toml::from_str(
        "font_px = 13.0\ntheme = \"Nord\"\nscrollback_lines = 4000\ncopy_on_select = true\n",
    )
    .unwrap();
    let fields = editable_fields(&c);
    let keys: Vec<&str> = fields.iter().map(|f| f.key).collect();
    let mut uniq: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for k in &keys {
        assert!(uniq.insert(k), "duplicate key {k} in editable_fields");
    }
    let mut last_section = 0usize;
    for f in &fields {
        let idx = super::section_of(f.key).order_index();
        assert!(
            idx >= last_section,
            "row {} ({:?}) breaks section grouping order",
            f.key,
            super::section_of(f.key)
        );
        last_section = idx;
    }
    for k in [
        EDIT_FOREGROUND,
        EDIT_BACKGROUND,
        EDIT_CURSOR_COLOR,
        EDIT_SELECTION_COLOR,
    ] {
        assert!(keys.contains(&k), "missing key {k}");
    }
    let seed = |k: &str| {
        fields
            .iter()
            .find(|f| f.key == k)
            .and_then(|f| f.seed.clone())
    };
    assert_eq!(seed(EDIT_FONT_PX).as_deref(), Some("13"));
    assert_eq!(seed(EDIT_THEME).as_deref(), Some("Nord"));
    assert_eq!(seed(EDIT_SCROLLBACK).as_deref(), Some("4000"));
    assert_eq!(seed(EDIT_COPY_ON_SELECT).as_deref(), Some("true"));
    assert_eq!(seed(EDIT_LIGATURES).as_deref(), Some("true"));
    assert_eq!(seed(EDIT_FONT_FAMILY), None);
    assert_eq!(seed(EDIT_CURSOR_STYLE), None);
}

#[test]
fn editable_fields_default_config_is_blank() {
    let fields = editable_fields(&Config::default());
    let seed = |k: &str| {
        fields
            .iter()
            .find(|f| f.key == k)
            .and_then(|f| f.seed.clone())
    };
    assert_eq!(seed(EDIT_FONT_PX), None);
    assert_eq!(seed(EDIT_FONT_FAMILY), None);
    assert_eq!(seed(EDIT_THEME), None);
    assert_eq!(seed(EDIT_CURSOR_STYLE), None);
    assert_eq!(seed(EDIT_SCROLLBACK), None);
    assert_eq!(
        seed(EDIT_COPY_ON_SELECT).as_deref(),
        Some(if cfg!(target_os = "linux") {
            "false"
        } else {
            "true"
        })
    );
    assert_eq!(seed(EDIT_LIGATURES).as_deref(), Some("true"));
    assert_eq!(seed(super::EDIT_MACHINE_UNIVERSAL_CONTROL), None);
    let placeholder = fields
        .iter()
        .find(|f| f.key == super::EDIT_MACHINE_UNIVERSAL_CONTROL)
        .map(|f| f.placeholder.as_str());
    assert_eq!(placeholder, Some("off (default)"));
}

#[test]
fn range_of_covers_bounded_numerics_only() {
    for f in editable_fields(&Config::default()) {
        match f.kind {
            EditKind::Float | EditKind::Integer => {
                let bounded = range_of(f.key).is_some();
                let unbounded = matches!(
                    f.key,
                    EDIT_SCROLLBACK
                        | EDIT_COLUMNS
                        | EDIT_LINES
                        | EDIT_SEARCH_HISTORY_LINES
                        | "matrix_rain.seed"
                );
                assert!(
                    bounded ^ unbounded,
                    "{} must be exactly one of slider (range_of) / free-form field",
                    f.key
                );
            }
            _ => assert!(
                range_of(f.key).is_none(),
                "{} is non-numeric and must have no slider range",
                f.key
            ),
        }
    }
    let r = range_of(EDIT_FONT_PX).expect("font size is a slider");
    assert!(r.min < r.max && r.step > 0.0);
    let range = range_of(super::EDIT_TAB_STRIP_ROWS).expect("tab strip range");
    assert_eq!(range.min, 0.0);
    assert_eq!(
        range.max,
        f64::from(crate::app_config::MAX_TAB_STRIP_ROWS),
        "control and shipping config clamp must agree",
    );
}

#[test]
fn editable_fields_blank_string_seeds_none() {
    let c: Config = aterm_toml::from_str("theme = \"   \"\nfont_family = \"\"\n").unwrap();
    let fields = editable_fields(&c);
    let seed = |k: &str| {
        fields
            .iter()
            .find(|f| f.key == k)
            .and_then(|f| f.seed.clone())
    };
    assert_eq!(seed(EDIT_THEME), None);
    assert_eq!(seed(EDIT_FONT_FAMILY), None);
}
#[test]
fn a_row_draws_its_own_note_and_neighbours_share_one_once() {
    use super::RowNote::{Own, Shared};
    let own = |key: &str| super::footnote_of(key).map(Own);
    assert_eq!(
        super::row_notes(&[super::EDIT_OPTION_AS_META, super::EDIT_PREDICTIVE_ECHO]),
        vec![None, own(super::EDIT_PREDICTIVE_ECHO)]
    );
    assert_eq!(
        super::row_notes(&[super::EDIT_MINIMUM_CONTRAST, super::EDIT_SELECTION_INACTIVE]),
        vec![own(super::EDIT_MINIMUM_CONTRAST), None],
        "Minimum contrast's note stays in its own row"
    );
    let sound = [
        super::EDIT_TRAIL_SOUNDS,
        super::EDIT_TRAIL_SOUND_VOLUME,
        super::EDIT_TONE_MELODY,
        super::EDIT_BELL_SOUND,
    ];
    assert_eq!(
        super::row_notes(&sound),
        sound.map(own).to_vec(),
        "each Sound row's note is its own"
    );
    for voice in [
        super::EDIT_TRAIL_SOUND_STYLE,
        super::EDIT_TONE_MELODY,
        super::EDIT_TRAIL_SOUND_BED,
        super::EDIT_TRAIL_SOUND_RIFF,
        super::EDIT_SPARKLE_BONK,
        super::EDIT_SPARKLE_BONK_DETONATION,
        super::EDIT_OUTPUT_STREAK_SOUND,
    ] {
        let note = super::footnote_of(voice);
        assert!(
            note.is_some_and(
                |note| note != super::TRAIL_SOUNDS_NOTE && note != super::TRAIL_SOUND_VOLUME_NOTE
            ),
            "{voice} says what it sounds like: {note:?}"
        );
    }
    let permissions = super::SECURITY_BOOL_KEYS;
    assert_eq!(
        super::row_notes(permissions),
        permissions.iter().map(|key| own(key)).collect::<Vec<_>>()
    );
    for key in permissions {
        assert!(super::footnote_of(key).is_some(), "{key} has a note");
        assert_eq!(super::row_notes(&[key]), vec![own(key)], "{key} alone");
    }
    let questions = [
        super::EDIT_HARNESS_ANSWER_QUESTIONS,
        super::EDIT_HARNESS_QUESTION_PRIORITY,
    ];
    assert_eq!(
        super::row_notes(&questions),
        vec![None, Some(Shared(super::HARNESS_QUESTIONS_NOTE))]
    );
    assert_eq!(
        super::row_notes(&questions[1..]),
        vec![Some(Own(super::HARNESS_QUESTIONS_NOTE))],
        "a shared note listed under one of its rows is that row's own"
    );
    assert_eq!(
        super::row_notes(&[super::EDIT_PACKAGES_ENABLED]),
        vec![own(super::EDIT_PACKAGES_ENABLED)],
        "Modified listing one ALab tools switch explains only that switch"
    );
    assert_eq!(
        super::footnote_of(EDIT_FOREGROUND),
        None,
        "the field says Theme (default)"
    );
}

#[test]
fn off_keeps_the_style_it_replaced_and_a_drawn_style_takes_it_out() {
    use super::{
        EDIT_CURSOR_MOMENTUM_GLOW, EDIT_CURSOR_TRAIL_STYLE_BEFORE_OFF as BEFORE_OFF,
        RETIRED_CURSOR_TRAIL,
    };
    let read = |toml: &str, key: &str| {
        toml.parse::<aterm_toml::edit::DocumentMut>()
            .unwrap()
            .get(key)
            .map(|item| item.to_string().trim().trim_matches('"').to_string())
    };
    let write =
        |toml: &str, edits: &[(&str, Option<String>)]| apply_prefs_edits(toml, edits).unwrap();
    let off = write(
        "cursor_trail_style = \"comet\"\n",
        &[(EDIT_CURSOR_TRAIL_STYLE, set("off"))],
    );
    assert_eq!(read(&off, EDIT_CURSOR_TRAIL_STYLE).as_deref(), Some("off"));
    assert_eq!(read(&off, BEFORE_OFF).as_deref(), Some("comet"));
    let from_default = write("", &[(EDIT_CURSOR_TRAIL_STYLE, set("off"))]);
    assert_eq!(
        read(&from_default, BEFORE_OFF),
        crate::app_config::DEFAULT_DECORATIVE_EFFECTS
            .then(|| super::DEFAULT_CURSOR_TRAIL_STYLE.to_string())
    );
    assert_eq!(
        read(
            &write(&off, &[(EDIT_CURSOR_TRAIL_STYLE, set("OFF"))]),
            BEFORE_OFF
        )
        .as_deref(),
        Some("comet")
    );
    assert_eq!(
        read(&write(&off, &[(EDIT_FONT_PX, set("15"))]), BEFORE_OFF).as_deref(),
        Some("comet")
    );
    let water = write(&off, &[(EDIT_CURSOR_TRAIL_STYLE, set("water"))]);
    assert_eq!(read(&water, BEFORE_OFF), None);
    let reset = write(&off, &[(EDIT_CURSOR_TRAIL_STYLE, None)]);
    assert_eq!(
        read(&reset, BEFORE_OFF).is_none(),
        crate::app_config::DEFAULT_DECORATIVE_EFFECTS
    );
    let forgot = write(
        &off,
        &[(EDIT_CURSOR_TRAIL_STYLE, set("off")), (BEFORE_OFF, None)],
    );
    assert_eq!(read(&forgot, BEFORE_OFF), None);
    let held = "cursor_trail = false\ncursor_trail_style = \"fire\"\n";
    let glow = write(held, &[(EDIT_CURSOR_MOMENTUM_GLOW, set("true"))]);
    assert_eq!(read(&glow, RETIRED_CURSOR_TRAIL), None);
    assert_eq!(read(&glow, EDIT_CURSOR_TRAIL_STYLE).as_deref(), Some("off"));
    assert_eq!(read(&glow, BEFORE_OFF).as_deref(), Some("fire"));
    let chose_off = write(held, &[(EDIT_CURSOR_TRAIL_STYLE, set("off"))]);
    assert_eq!(read(&chose_off, BEFORE_OFF).as_deref(), Some("fire"));
    let chose_water = write(held, &[(EDIT_CURSOR_TRAIL_STYLE, set("water"))]);
    assert_eq!(read(&chose_water, BEFORE_OFF), None);
    let alias = write(
        "cursor_trail_style = \"nyan\"\n",
        &[(EDIT_CURSOR_TRAIL_STYLE, set("off"))],
    );
    // A retired name the rewrite did not reach drew the default: Off keeps that.
    assert_eq!(read(&alias, BEFORE_OFF).as_deref(), Some("rainbow"));
    let unknown = write(
        "cursor_trail_style = \"cometz\"\n",
        &[(EDIT_CURSOR_TRAIL_STYLE, set("off"))],
    );
    assert_eq!(
        read(&unknown, BEFORE_OFF).as_deref(),
        Some(super::DEFAULT_CURSOR_TRAIL_STYLE)
    );
    for (kept, valid) in [
        ("laser", true),
        ("Classic Mono", true),
        ("rainbow kitty flying", false),
        ("nyan", false),
    ] {
        assert_eq!(
            crate::native_config_language::registered_enum_value_is_valid(
                BEFORE_OFF,
                kept,
                super::CURSOR_TRAIL_STYLES
            ),
            valid,
            "{kept}"
        );
    }
    let config: Config = aterm_toml::from_str(&off).unwrap();
    assert_eq!(config.cursor_trail_style_raw(), "off");

    // Written itself: kept while the style is Off, refused while one draws (it wrote the
    // default style into the file, where nothing read it), and `off` is no style at all.
    let chosen = write(&off, &[(BEFORE_OFF, set("rainbow"))]);
    assert_eq!(
        read(&chosen, BEFORE_OFF).as_deref(),
        Some(super::DEFAULT_CURSOR_TRAIL_STYLE)
    );
    assert_eq!(
        read(&write(&off, &[(BEFORE_OFF, set("off"))]), BEFORE_OFF),
        None
    );
    let unset_draws = crate::app_config::DEFAULT_DECORATIVE_EFFECTS.then_some("");
    for drawn in ["cursor_trail_style = \"comet\"\n"]
        .into_iter()
        .chain(unset_draws)
    {
        let refused =
            apply_prefs_edits(drawn, &[(BEFORE_OFF, set("rainbow"))]).expect_err("a style draws");
        assert!(
            matches!(refused, super::PrefsEditError::NotKept { .. }),
            "{refused}"
        );
        assert_eq!(
            read(&write(drawn, &[(BEFORE_OFF, set("off"))]), BEFORE_OFF),
            None
        );
    }
}

#[test]
fn a_classic_mono_spelling_keeps_its_colour_when_rewritten() {
    use super::{
        EDIT_CURSOR_MOMENTUM_GLOW, EDIT_CURSOR_TRAIL_COLOR as COLOR,
        EDIT_CURSOR_TRAIL_STYLE_BEFORE_OFF as BEFORE_OFF,
    };
    use aterm_effects::cursor_glow::GlowStyle;
    let read = |toml: &str, key: &str| {
        toml.parse::<aterm_toml::edit::DocumentMut>()
            .unwrap()
            .get(key)
            .map(|item| item.to_string().trim().trim_matches('"').to_string())
    };
    let write =
        |toml: &str, edits: &[(&str, Option<String>)]| apply_prefs_edits(toml, edits).unwrap();
    let theme_cursor = 0x0009_69DA;
    let drawn = |toml: &str| {
        let config: Config = aterm_toml::from_str(toml).unwrap();
        let presentation = crate::app_config::resolve_trail_presentation(
            config.cursor_trail_style_raw(),
            config.cursor_pet_raw(),
        );
        let glow = crate::app_config::resolve_cursor_glow(
            config.cursor_glow_inputs(true, theme_cursor, presentation),
            presentation,
            theme_cursor,
            true,
            0x00C8_D3F5,
            0x001A_1B26,
            0.5,
        );
        (
            glow.enabled.then_some(glow.style),
            glow.classic_mono,
            config.trail_color(),
        )
    };
    let classic_in_cursor_color = (
        Some(GlowStyle::Classic),
        true,
        aterm_effects::cursor_fx::TrailColor::Cursor,
    );
    for mono in ["classic mono", "Classic Lumen", " mono classic "] {
        let file = format!("cursor_trail_style = \"{mono}\"\n");
        assert_eq!(drawn(&file).0, Some(GlowStyle::Classic));
        assert!(drawn(&file).1, "{mono:?} draws the one-colour face");
        let off = write(&file, &[(EDIT_CURSOR_TRAIL_STYLE, set("off"))]);
        assert_eq!(
            read(&off, BEFORE_OFF).as_deref(),
            Some("classic"),
            "{mono:?}"
        );
        assert_eq!(read(&off, COLOR).as_deref(), Some("cursor"), "{mono:?}");
        assert_eq!(drawn(&off).0, None, "{mono:?}");
        let back = write(&off, &[(EDIT_CURSOR_TRAIL_STYLE, set("classic"))]);
        assert_eq!(drawn(&back), classic_in_cursor_color, "{mono:?}");
        assert_eq!(read(&back, BEFORE_OFF), None, "{mono:?}");
        for (style, glow) in [
            ("comet", GlowStyle::Comet),
            ("sparkle", GlowStyle::Sparkle),
            ("beam", GlowStyle::Beam),
            ("laser", GlowStyle::Laser),
            ("fire", GlowStyle::Fire),
        ] {
            let other = write(&file, &[(EDIT_CURSOR_TRAIL_STYLE, set(style))]);
            assert_eq!(read(&other, COLOR), None, "{mono:?} to {style}");
            assert_eq!(
                drawn(&other),
                (
                    Some(glow),
                    false,
                    aterm_effects::cursor_fx::TrailColor::Automatic
                ),
                "{mono:?} to {style}"
            );
        }
        let named = write(&file, &[(EDIT_CURSOR_TRAIL_STYLE, set("classic"))]);
        assert_eq!(drawn(&named), classic_in_cursor_color, "{mono:?}");
        let shifting = write(&named, &[(COLOR, None)]);
        assert_eq!(
            drawn(&shifting),
            (
                Some(GlowStyle::Classic),
                false,
                aterm_effects::cursor_fx::TrailColor::Automatic
            ),
            "{mono:?}"
        );
        let held = format!("cursor_trail = false\n{file}");
        let glow = write(&held, &[(EDIT_CURSOR_MOMENTUM_GLOW, set("true"))]);
        assert_eq!(
            read(&glow, BEFORE_OFF).as_deref(),
            Some("classic"),
            "{mono:?}"
        );
        assert_eq!(read(&glow, COLOR).as_deref(), Some("cursor"), "{mono:?}");
        let tinted = format!("{file}cursor_trail_color = \"#50FA7B\"\n");
        let tinted_off = write(&tinted, &[(EDIT_CURSOR_TRAIL_STYLE, set("off"))]);
        assert_eq!(read(&tinted_off, COLOR).as_deref(), Some("#50FA7B"));
        let typo = format!("{file}cursor_trail_color = \"#50FA7\"\n");
        assert_eq!(
            drawn(&typo),
            (
                Some(GlowStyle::Classic),
                true,
                aterm_effects::cursor_fx::TrailColor::Automatic
            ),
            "{mono:?}: the one-colour face, in the cursor's colour"
        );
        let typo_off = write(&typo, &[(EDIT_CURSOR_TRAIL_STYLE, set("off"))]);
        assert_eq!(
            read(&typo_off, COLOR).as_deref(),
            Some("cursor"),
            "{mono:?}"
        );
        let typo_back = write(&typo_off, &[(EDIT_CURSOR_TRAIL_STYLE, set("classic"))]);
        assert_eq!(drawn(&typo_back), classic_in_cursor_color, "{mono:?}");
        let reset = write(&file, &[(EDIT_CURSOR_TRAIL_STYLE, None), (COLOR, None)]);
        assert_eq!(read(&reset, COLOR), None, "{mono:?}");
        let other = write(&file, &[(EDIT_FONT_PX, set("15"))]);
        assert_eq!(read(&other, COLOR), None, "{mono:?}");
        assert_eq!(read(&other, EDIT_CURSOR_TRAIL_STYLE).as_deref(), Some(mono));
    }
    // A retired name is not written, as the style or as the one Off keeps.
    for was in aterm_effects::cursor_glow::spellings::RETIRED_TRAIL_STYLES {
        assert!(
            !GlowStyle::style_names_classic_mono(was.old),
            "{:?}",
            was.old
        );
        for key in [EDIT_CURSOR_TRAIL_STYLE, BEFORE_OFF] {
            assert!(
                apply_prefs_edits("", &[(key, set(was.old))]).is_err(),
                "{key} = {:?}",
                was.old
            );
        }
    }
    let chosen = write("", &[(COLOR, set(" CURSOR "))]);
    assert_eq!(read(&chosen, COLOR).as_deref(), Some("cursor"));
    assert!(apply_prefs_edits("", &[(EDIT_FOREGROUND, set("cursor"))]).is_err());
    assert!(apply_prefs_edits("", &[(super::EDIT_CURSOR_TRAIL_ACCENT, set("cursor"))]).is_err());
}
