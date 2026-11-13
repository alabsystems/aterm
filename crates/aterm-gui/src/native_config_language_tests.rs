// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::collections::BTreeSet;

#[test]
fn settings_set_names_a_hand_written_key_as_one() {
    for key in ["conpty", "windowing_behavior", "fleet.peers"] {
        let refusal = control_settings_refusal(key);
        assert!(
            refusal.starts_with(&format!("{key} is set in aterm.toml by hand"))
                && refusal.contains("Manual"),
            "{refusal}"
        );
    }
    for (key, verb) in [
        ("fleet.allow", "`aterm fleet allow facts|content`"),
        (
            "fleet.drive_from",
            "`aterm fleet allow drive --from <host|node>`",
        ),
        ("fleet.listen", "`aterm fleet init` or `aterm fleet join`"),
    ] {
        let refusal = control_settings_refusal(key);
        assert!(
            refusal.starts_with(&format!("{key} is written by `aterm fleet`"))
                && refusal.contains(verb)
                && !refusal.contains("by hand"),
            "{refusal}"
        );
    }
    assert_eq!(
        control_settings_refusal("title_summary_provider"),
        compatibility_only_message("title_summary_provider")
    );
    let unknown = control_settings_refusal("not_a_real_setting");
    assert!(
        unknown.starts_with("unknown key \"not_a_real_setting\"") && unknown.contains("Manual"),
        "{unknown}"
    );
}

#[test]
fn settings_set_never_calls_a_key_a_reader_takes_unknown() {
    assert!(
        !MANUAL_SCHEMA
            .iter()
            .any(|entry| entry.key == "harness.occupancy"),
        "no Manual row names it"
    );
    assert_eq!(
        settings_refusal_of("harness.occupancy"),
        SettingsRefusal::HandWritten
    );
    for key in reader_accepted_paths() {
        assert_ne!(settings_refusal_of(key), SettingsRefusal::Unknown, "{key}");
    }
    assert_eq!(
        settings_refusal_of("not_a_real_setting"),
        SettingsRefusal::Unknown
    );
}

/// THE OWNER'S TABLES ARE MANUAL'S, HAND-WRITTEN: Manual names them with no Reset, Settings
/// answers that they are written by hand, and the design's table draws no warning, while a key
/// no rule takes and a misspelt table still do (NEGATIVE CONTROL: the walker is live there).
#[test]
fn the_owner_tables_are_hand_written_manual_rows() {
    for key in [
        "harness.occupancy_source",
        "harness.occupancy_rule",
        "harness.occupancy_rule.when",
        "harness.occupancy_rule.text",
    ] {
        assert_eq!(
            settings_refusal_of(key),
            SettingsRefusal::HandWritten,
            "{key}"
        );
        let entry = config_schema_entry(key).unwrap_or_else(|| panic!("{key} is a Manual row"));
        assert!(
            !entry.manual_reset_safe && !entry.native_scalar,
            "{key}: {entry:?}"
        );
    }
    let design = "[harness]\noccupancy = \"prompt\"\n\n\
         [harness.occupancy_source.trust]\nkind = \"git-ref\"\nrepo = \"$HOME/trust\"\n\
         remote = \"origin\"\nref = \"refs/heads/main\"\npath = \"src/stage0\"\nevery = \"5m\"\n\n\
         [harness.occupancy_source.ty]\nkind = \"atpkg\"\nprogram = \"ty\"\n\n\
         [[harness.occupancy_rule]]\nname = \"release-base-moved\"\nwhen = \"git(trust).moved\"\n\
         scope = { jobs_cwd = \"~/scratch.noindex/trust-release-*\" }\n\
         modes = [\"cruise\", \"land\"]\nurgent = true\nmode = \"dry-run\"\n\
         text = \"Re-read trust main and the atpkg index before any cut, mint or publish goes on.\"\n";
    let (_, tables, notes) = aterm_agent::supervise::SupervisorConfig::with_owner_tables(design);
    assert!(notes.is_empty(), "the agent's reader: {notes:?}");
    assert_eq!((tables.sources().len(), tables.rules().len()), (2, 1));
    assert_eq!(ignored_key_warnings(design), Vec::<String>::new());
    assert_eq!(analyze(design).diagnostics, Vec::new());

    let unknown = format!("{design}bogus = 1\n");
    assert!(
        ignored_key_warnings(&unknown).iter().any(|warning| warning
            .contains("harness.occupancy_rule.bogus is unknown in [[harness.occupancy_rule]]")),
        "{:?}",
        ignored_key_warnings(&unknown)
    );
    let misspelt = "[harness.occupancy_sources.trust]\nkind = \"git-ref\"\n";
    assert!(
        ignored_key_warnings(misspelt)
            .iter()
            .any(|warning| warning.contains("harness.occupancy_sources")),
        "{:?}",
        ignored_key_warnings(misspelt)
    );
}

#[test]
fn candidate_match_score_ranks_each_rung() {
    use MatchReach::{Loose, Words};
    let words = |candidate: &str, query: &str| candidate_match_score(candidate, query, Words);
    let loose = |candidate: &str, query: &str| candidate_match_score(candidate, query, Loose);
    assert_eq!(words("font size", "font"), Some(WHOLE_AT_START));
    assert_eq!(words("display font", "font"), Some(WHOLE_AT_WORD));
    assert_eq!(words("cursor_trail", "cur"), Some(PREFIX_AT_START));
    assert_eq!(words("enable_cursor_trail", "cur"), Some(PREFIX_AT_WORD));
    assert_eq!(words("nothing_here", "cur"), None);
    assert!(
        words("supervise claude code sessions", "code") < words("codex", "code"),
        "a whole word mid-name before a prefix at the start"
    );
    assert!(words("codex", "code") < words("watch codex-alab sessions", "code"));
    assert_eq!(words("codex code", "code"), Some(WHOLE_AT_WORD));
    assert_eq!(words("tabs", "tab"), Some(WHOLE_AT_START));
    assert_eq!(words("text boxes", "box"), Some(WHOLE_AT_WORD));
    assert_eq!(words("tables", "tab"), Some(PREFIX_AT_START));
    assert_eq!(words("bus", "bu"), Some(PREFIX_AT_START));
    assert_eq!(
        words("terminal bell sound", "bell sound"),
        Some(WHOLE_AT_WORD)
    );
    assert_eq!(
        words("cursor_trail_style", "trail_style"),
        Some(WHOLE_AT_WORD)
    );
    assert_eq!(words("cursor_trail_style", "_style"), Some(WHOLE_AT_WORD));
    assert_eq!(words("cursor_trail_style", "ail_style"), None);
    assert_eq!(words("thecursorthing", "cur"), None);
    assert_eq!(
        words("allow programs to show desktop notifications", "cat"),
        None
    );
    assert_eq!(words("indicator", "cat"), None);
    assert_eq!(words("feline", "line"), None);
    assert_eq!(words("line height", "lh"), None);
    assert_eq!(words("color and theme", "cat"), None);
    assert_eq!(loose("colour_under_rail", "cur"), Some(ACRONYM_RUNG));
    assert_eq!(loose("line height", "lh"), Some(ACRONYM_RUNG));
    assert_eq!(loose("color and theme", "cat"), Some(ACRONYM_RUNG));
    assert_eq!(loose("a a b", "aab"), Some(ACRONYM_RUNG));
    assert_eq!(loose("keyboard & input", "board"), Some(INSIDE_WORD_RUNG));
    assert_eq!(loose("wallpaper", "paper"), Some(INSIDE_WORD_RUNG));
    assert_eq!(loose("feline", "line"), Some(INSIDE_WORD_RUNG));
    assert_eq!(loose("enable_cursor_trail", "cur"), Some(PREFIX_AT_WORD));
    assert_eq!(loose("clear_tab", "cat"), None);
    assert_eq!(
        loose("show claude's working time and tokens (σ)", "cat"),
        None
    );
    assert_eq!(loose("colour_under_rail_link", "curl"), None);
    assert_eq!(loose("keyboard", "yb"), None);
    assert_eq!(loose("nothing_here", "zzz"), None);
    assert!(words("cursor", "cur") < words("my_cursor", "cur"));
    let model = |candidate: &str, query: &str| -> Option<u8> {
        if query.is_empty() {
            return Some(WHOLE_AT_START);
        }
        let bytes = candidate.as_bytes();
        let alnum = |byte: &u8| byte.is_ascii_alphanumeric();
        candidate
            .match_indices(query)
            .filter(|(i, _)| !alnum(&query.as_bytes()[0]) || *i == 0 || !alnum(&bytes[i - 1]))
            .map(|(i, _)| {
                let end = i + query.len();
                let rest: String = candidate[end..]
                    .chars()
                    .take_while(char::is_ascii_alphanumeric)
                    .collect();
                let whole = !alnum(query.as_bytes().last().unwrap_or(&b'_'))
                    || rest.is_empty()
                    || (query.len() >= 3 && (rest == "s" || rest == "es"));
                match (whole, i == 0) {
                    (true, true) => WHOLE_AT_START,
                    (true, false) => WHOLE_AT_WORD,
                    (false, true) => PREFIX_AT_START,
                    (false, false) => PREFIX_AT_WORD,
                }
            })
            .min()
    };
    let candidates = [
        "",
        "_",
        "a",
        "cursor",
        "cursors",
        "enable_cursor_trail",
        "x-cur",
        "é_cur",
        "écur",
        "a1_2b",
        "über_font",
        "font.size",
        "cur_",
        "日本cur",
        "a  b",
        "trail__cur9",
        "terminal bell sound",
        "cur curl cur",
    ];
    let queries = [
        "",
        "c",
        "cur",
        "cur9",
        "1",
        "2b",
        "b",
        "_cur",
        "é",
        "cur_",
        "font.",
        "ü",
        "日",
        " b",
        "bell sound",
        "l sound",
        "cursor",
    ];
    for candidate in candidates.into_iter().chain(
        config_schema()
            .iter()
            .flat_map(|entry| std::iter::once(entry.key).chain(entry.keywords.iter().copied())),
    ) {
        for query in queries {
            assert_eq!(
                words(candidate, query),
                model(candidate, query),
                "{candidate:?} / {query:?}"
            );
        }
    }
}

#[test]
fn an_own_name_hit_ranks_before_a_synonym_hit_on_the_same_rung() {
    let rank = |own: &[&str], synonyms: &[&str], query: &str| {
        query_match_rank(
            own.iter().copied(),
            synonyms.iter().copied(),
            query,
            MatchReach::Words,
        )
    };
    let named = rank(&["supervise codex sessions", "codex"], &["agent"], "codex");
    let listed = rank(
        &["keep alab tools up to date"],
        &["codex", "claude"],
        "codex",
    );
    assert_eq!(named, Some(SearchRank::own(WHOLE_AT_START)));
    assert_eq!(listed, Some(SearchRank::synonym(WHOLE_AT_START)));
    assert!(named < listed);
    assert!(rank(&["x"], &["codex"], "codex") < rank(&["supervise codex sessions"], &[], "codex"));
    assert_eq!(
        rank(&["dark sky"], &["mode"], "dark mode"),
        Some(SearchRank::synonym(WHOLE_AT_START))
    );
    assert_eq!(rank(&["dark sky"], &["mode"], "sun"), None);
    assert!(
        rank(&["supervise claude code sessions"], &[], "claude code")
            < rank(
                &["keep alab tools up to date"],
                &["claude", "codex"],
                "claude code"
            )
    );
}

#[test]
fn query_match_score_takes_every_word() {
    let set = ["window appearance", "window_theme", "dark", "light", "mode"];
    let score = |query: &str| query_match_score(set.iter().copied(), query, MatchReach::Words);
    assert_eq!(score("dark mode"), Some(WHOLE_AT_START));
    assert_eq!(
        score("mode dark"),
        Some(WHOLE_AT_START),
        "word order does not matter"
    );
    assert_eq!(score("dark sky"), None, "every word must match");
    assert_eq!(
        score("the dark mode"),
        Some(WHOLE_AT_START),
        "filler words need not"
    );
    assert_eq!(score("appearance of the window"), Some(WHOLE_AT_WORD));
    assert_eq!(score("dark appear"), Some(PREFIX_AT_WORD));
    assert_eq!(score("dark ppear"), None);
    assert_eq!(score("window app"), Some(PREFIX_AT_START));
    let words = |set: &[&str], query: &str| {
        query_match_score(set.iter().copied(), query, MatchReach::Words)
    };
    let kitties = ["keyword kitties", "sparkle_words.feline.enabled"];
    assert_eq!(words(&kitties, "kitty"), Some(WHOLE_AT_WORD));
    assert_eq!(words(&["smart title"], "titles"), Some(WHOLE_AT_WORD));
    assert_eq!(words(&["copy and trim"], "cats"), None);
    assert_eq!(words(&["user"], "series"), None);
    assert_eq!(words(&["serial port"], "series"), None);
    assert_eq!(
        query_match_score(["keyboard & input"].into_iter(), "board", MatchReach::Loose),
        Some(INSIDE_WORD_RUNG)
    );
    assert_eq!(
        query_match_score(["keyboard & input"].into_iter(), "board", MatchReach::Words),
        None
    );
}

fn highlighted<'a>(
    source: &'a str,
    analysis: &ConfigAnalysis,
    class: EditorSyntaxClass,
) -> Vec<&'a str> {
    analysis
        .syntax
        .iter()
        .filter(|span| span.class == class)
        .map(|span| &source[span.bytes.clone()])
        .collect()
}

#[test]
fn native_toml_lexer_projects_keys_values_tables_and_comments() {
    let source = "theme = \"Nord\" # terminal colors\n[sparkle_words]\nenabled = true\n";
    let analysis = analyze(source);
    assert!(!analysis.has_errors(), "{:?}", analysis.diagnostics);
    assert_eq!(
        highlighted(source, &analysis, EditorSyntaxClass::Key),
        ["theme", "enabled"]
    );
    assert_eq!(
        highlighted(source, &analysis, EditorSyntaxClass::String),
        ["\"Nord\""]
    );
    assert_eq!(
        highlighted(source, &analysis, EditorSyntaxClass::Table),
        ["[sparkle_words]"]
    );
    assert_eq!(
        highlighted(source, &analysis, EditorSyntaxClass::Boolean),
        ["true"]
    );
    assert_eq!(
        highlighted(source, &analysis, EditorSyntaxClass::Comment),
        ["# terminal colors"]
    );
}

#[test]
fn a_retired_packages_key_is_never_told_to_rename_onto_a_written_key() {
    let message = |source: &str| -> String {
        analyze(source)
            .diagnostics
            .iter()
            .map(|d| d.message.clone())
            .find(|m| m.contains("auto_update") || m.contains("seed_install"))
            .unwrap_or_else(|| panic!("a retired-key diagnostic for {source:?}"))
    };
    let both = message("[packages]\nenabled = true\nauto_update = false\n");
    assert!(!both.contains("rename"), "{both}");
    assert!(
        both.contains("remove it and set `enabled = false`"),
        "{both}"
    );
    let alone = message("[packages]\nauto_update = false\n");
    assert!(alone.contains("rename it to `enabled`"), "{alone}");
    let seed_both = message("[packages]\nauto_install = true\nseed_install = false\n");
    assert!(!seed_both.contains("rename"), "{seed_both}");
    assert!(seed_both.contains("remove it"), "{seed_both}");
    let seed_alone = message("[packages]\nseed_install = false\n");
    assert!(
        seed_alone.contains("rename it to `packages.auto_install`"),
        "{seed_alone}"
    );
}

#[test]
fn unterminated_parser_diagnostic_targets_the_authored_line_before_trailing_newline() {
    let source = "# Manual\nfont_px = [ \n";
    let analysis = analyze(source);
    let diagnostic = analysis.first_error().expect("syntax diagnostic");
    let authored_end = source.len() - 1;

    assert_eq!(diagnostic.bytes, authored_end..authored_end);
    assert_eq!((diagnostic.line, diagnostic.column), (2, 13));
    assert!(
        analysis
            .summary()
            .is_some_and(|summary| summary.contains("Ln 2, Col 13"))
    );
}

#[test]
fn syntax_schema_and_metadata_errors_block_saves_but_unknown_keys_only_warn() {
    for blocking in [
        "theme = \"Nord\n",
        "font_px = \"large\"\n",
        "window_theme = \"sepia\"\n",
        "theme = \"dark:Dracula,sepia\"\n",
        "foreground = \"#abcd\"\n",
    ] {
        assert!(analyze(blocking).has_errors(), "{blocking:?}");
    }
    for color in [
        "foreground = \"#12abEF\"\n",
        "foreground = \"12abEF\"\n",
        "foreground = \"#abc\"\n",
    ] {
        assert!(!analyze(color).has_errors(), "{color:?}");
    }
    let custom_theme = analyze("theme = \"My Private Theme\"\n");
    assert!(!custom_theme.has_errors());
    assert!(custom_theme.diagnostics.is_empty());
    assert!(
        analyze("theme = \"dark:Dracula,light:GitHub Light\"\n")
            .diagnostics
            .is_empty()
    );
    assert!(
        analyze("theme = \"dark:My Dark Theme,light:My Light Theme\"\n")
            .diagnostics
            .is_empty()
    );

    let forward = analyze("future_setting = 1\n");
    assert!(!forward.has_errors());
    assert_eq!(
        forward.diagnostics[0].severity,
        ConfigDiagnosticSeverity::Warning
    );

    let compatibility = analyze("matrix_rain.materialize = true\n");
    assert!(!compatibility.has_errors());
    assert_eq!(
        compatibility.diagnostics[0].severity,
        ConfigDiagnosticSeverity::Warning
    );
}

#[test]
fn retired_feline_controls_are_preserved_diagnosed_and_never_completed() {
    let source = r##"[sparkle_words.feline]
idle = false
gaze = false
color = "#112233"
intensity = 0.25
"##;
    aterm_toml::from_str::<crate::app_config::Config>(source)
        .expect("retired feline keys remain loadable for compatibility");

    let analysis = analyze(source);
    assert!(!analysis.has_errors(), "{:?}", analysis.diagnostics);
    let expected = [
        "sparkle_words.feline.idle",
        "sparkle_words.feline.gaze",
        "sparkle_words.feline.color",
        "sparkle_words.feline.intensity",
    ];
    assert_eq!(analysis.diagnostics.len(), expected.len());
    for key in expected {
        assert!(
            analysis
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.starts_with(&format!("{key} "))),
            "missing compatibility diagnostic for {key}: {:?}",
            analysis.diagnostics
        );
    }

    let key_source = "[sparkle_words.feline]\nid";
    assert!(
        assist(key_source, key_source.len())
            .completions
            .iter()
            .all(|completion| !completion.insertion.starts_with("idle =")),
        "retired keys must not be offered as active configuration"
    );
    let value_source = "[sparkle_words.feline]\nidle = ";
    let assistance = assist(value_source, value_source.len());
    assert!(assistance.completions.is_empty());
    assert_eq!(
        assistance.help,
        Some(compatibility_only_message("sparkle_words.feline.idle"))
    );
}

#[test]
fn the_repaint_nudge_words_are_the_engines() {
    use aterm_core::terminal::repaint_nudge::Mode;
    let read = |word: &str| -> Mode {
        let toml = format!("[render]\nrepaint_nudge = \"{word}\"\n");
        aterm_toml::from_str::<crate::app_config::Config>(&toml)
            .unwrap()
            .repaint_nudge_mode()
    };
    for &word in REPAINT_NUDGE_MODES {
        assert_eq!(read(word).word(), word, "the Manual offers {word:?}");
    }
    assert_eq!(
        read("whenever"),
        Mode::Log,
        "a word it cannot read never acts"
    );
}

#[test]
fn a_reader_accepted_key_without_a_schema_row_is_never_unknown() {
    #[derive(serde::Deserialize)]
    #[expect(dead_code, reason = "only the reader's field list is read")]
    struct Reader {
        privacy: Option<ReaderPrivacy>,
    }
    #[derive(serde::Deserialize)]
    #[expect(dead_code, reason = "only the reader's field list is read")]
    struct ReaderPrivacy {
        no_schema_row: Option<bool>,
    }
    assert!(config_schema_entry("privacy.no_schema_row").is_none());
    let readers: BTreeSet<String> = reader_paths::<Reader>().into_iter().collect();
    let unknown = |source: &str| {
        let document = source.parse::<aterm_toml::edit::DocumentMut>().unwrap();
        let mut analysis = ConfigAnalysis::default();
        let walk = KeyWalk {
            source,
            document: &document,
            disk: &[],
            readers: &readers,
        };
        warn_unknown_values(walk, &mut analysis);
        analysis.diagnostics
    };
    let accepted = unknown("[privacy]\nno_schema_row = true\n");
    assert!(accepted.is_empty(), "{accepted:?}");
    let typo = unknown("[privacy]\nno_schema_rwo = true\n");
    assert!(
        typo.iter()
            .any(|diagnostic| diagnostic.message.contains("privacy.no_schema_rwo")),
        "{typo:?}"
    );
    for path in [
        "agents_auto_prime",
        "privacy.auto_accept",
        "harness.headless",
        "disk.auto_free_gib",
    ] {
        assert!(reader_accepted_paths().contains(path), "{path}");
    }
    assert!(!reader_accepted_paths().contains("privacy.no_schema_row"));
    let fabric = analyze(
        "[fabric]\ncommand = \"aterm-link serve\"\nreceipts = true\npresence = \"minimal\"\n",
    );
    assert!(fabric.diagnostics.is_empty(), "{:?}", fabric.diagnostics);
    let argv = analyze("[fabric]\ncommand = [\"/a b/aterm\", \"link\", \"serve\"]\n");
    assert!(argv.diagnostics.is_empty(), "{:?}", argv.diagnostics);
    let wrong = analyze("[fabric]\ncommand = 3\n");
    assert!(
        wrong
            .diagnostics
            .iter()
            .any(|d| d.message.contains(aterm_types::fabric_command::EXPECTED)),
        "{:?}",
        wrong.diagnostics
    );
}

#[test]
fn the_operator_switch_is_a_key_a_removed_feature_left() {
    for (source, line) in [
        ("[operator]\nenabled = true\n", 2),
        ("operator.enabled = false\n", 1),
    ] {
        let keys = key_warnings(source);
        assert!(keys.ignored.is_empty(), "{source:?}: {keys:?}");
        assert_eq!(
            keys.retired,
            vec![format!(
                "line {line}: {}",
                compatibility_only_message("operator.enabled")
            )],
            "{source:?}"
        );
        assert!(
            keys.retired[0].contains("was removed"),
            "{source:?}: {keys:?}"
        );
    }
}

#[test]
fn every_retired_top_level_key_says_it_was_removed_and_never_that_it_is_from_the_future() {
    let top_level: Vec<&str> = RETIRED_CONFIG_KEYS
        .iter()
        .map(|entry| entry.key)
        .filter(|key| !key.contains('.'))
        .collect();
    assert!(
        top_level.len() >= 8,
        "the sweep should cover every un-nested retired key: {top_level:?}"
    );
    for key in top_level {
        let source = format!("{key} = true\n");
        let analysis = analyze(&source);
        assert!(!analysis.has_errors(), "{key}: {:?}", analysis.diagnostics);
        let [diagnostic] = analysis.diagnostics.as_slice() else {
            panic!("{key} gets exactly one story: {:?}", analysis.diagnostics);
        };
        assert_eq!(diagnostic.severity, ConfigDiagnosticSeverity::Warning);
        let message = &diagnostic.message;
        assert_eq!(message, &compatibility_only_message(key));
        assert!(!message.contains("unknown"), "{key}: {message}");
        assert!(
            !message.contains("forward compatibility"),
            "{key}: {message}"
        );
        let value_source = format!("{key} = ");
        let value = assist(&value_source, value_source.len());
        assert!(
            value.completions.is_empty(),
            "{key}: retired values have no active runtime choices: {value:?}"
        );
        assert_eq!(
            value.help,
            Some(compatibility_only_message(key)),
            "{key}: the caret help tells the diagnostic's story"
        );
        let prefix = &key[..key.len() - 2];
        assert!(
            assist(prefix, prefix.len())
                .completions
                .iter()
                .all(|completion| !completion.insertion.starts_with(key)),
            "{key}: a retired key is never completed"
        );
    }
}

#[test]
fn host_analysis_resolves_assets_off_the_pure_language_path() {
    let source = concat!(
        "theme = \"Manual Theme That Cannot Exist 7F1C\"\n",
        "font_family_bold = \"Manual Font That Cannot Exist 7F1C\"\n",
        "cursor_nyan_sprite = \"/aterm/no-such-manual-nyan-7f1c.png\"\n",
    );
    let pure = analyze(source);
    assert!(
        pure.diagnostics.is_empty(),
        "custom theme names are neutral until the host resolves them"
    );
    let host = analyze_host(source, true);
    let missing = host
        .iter()
        .find(|diagnostic| {
            diagnostic.message.starts_with("theme:")
                && diagnostic.message.contains("is unavailable")
        })
        .expect("host reports the loader's theme fallback")
        .clone();
    assert_eq!(missing.severity, ConfigDiagnosticSeverity::Warning);
    assert_eq!(
        &source[missing.bytes.clone()],
        "\"Manual Theme That Cannot Exist 7F1C\""
    );
    for expected in ["font_family_bold", "cursor_nyan_sprite"] {
        assert!(
            host.iter()
                .any(|diagnostic| diagnostic.message.contains(expected)),
            "host diagnostics must mirror the runtime fallback for {expected}: {host:?}"
        );
    }

    let mut merged = pure;
    assert!(merged.merge_host_diagnostics(host.clone()));
    assert!(!merged.merge_host_diagnostics(host));
    assert_eq!(
        merged
            .diagnostics
            .iter()
            .filter(|diagnostic| *diagnostic == &missing)
            .count(),
        1,
        "replayed host completions cannot duplicate editor diagnostics"
    );
}

#[test]
fn merged_lexicon_host_warnings_address_external_path_and_exact_custom_word() {
    let thread = std::thread::current();
    let root = std::env::temp_dir().join(format!(
        "aterm-manual-merged-lexicon-{}-{}",
        std::process::id(),
        thread.name().unwrap_or("test").replace(':', "-")
    ));
    std::fs::create_dir_all(&root).unwrap();
    let lexicon = root.join("conflict.toml");
    std::fs::write(
        &lexicon,
        "[[entry]]\nclass=\"emphasis\"\nlang=\"en\"\nmode=\"forms\"\nforms=[\"abc猫\"]\n",
    )
    .unwrap();
    let path_token = format!("{:?}", lexicon.to_string_lossy());
    let source = format!(
        "[sparkle_words]\nlexicon = {path_token}\n\
[sparkle_words.feline]\ncjk_single_char = false\n\
[[sparkle_words.custom]]\nwords = [\"犬\", \"mix猫\"]\nink = \"rainbow\"\n"
    );
    let host = analyze_host(&source, true);
    for (message, token) in [
        (
            "sparkle_words.lexicon merged-layer conflict",
            path_token.as_str(),
        ),
        ("record 1 word 1", "\"犬\""),
        ("record 1 word 2", "\"mix猫\""),
    ] {
        let diagnostic = host
            .iter()
            .find(|diagnostic| diagnostic.message.contains(message))
            .unwrap_or_else(|| panic!("missing {message:?}: {host:?}"));
        assert_eq!(&source[diagnostic.bytes.clone()], token, "{message}");
    }

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn manual_surfaces_the_canonical_keybinding_sequence_and_palette_warnings() {
    let source = r##"palette = ["#102030", "nope"]
[key_sequences]
"shift+entr" = "hi"
"ctrl+x" = '\q'
"cmd+c" = "shadow"
[keybindings]
"cmd+k" = "no_such_action"
"##;
    let analysis = analyze(source);
    assert!(
        !analysis.has_errors(),
        "warnings do not make TOML unsaveable"
    );
    assert!(
        analysis
            .diagnostics
            .iter()
            .all(|diagnostic| diagnostic.severity == ConfigDiagnosticSeverity::Warning)
    );
    let mut expected_spans = vec![
        ("palette[1]", "\"nope\""),
        ("chord \"shift+entr\" invalid", "\"shift+entr\""),
        ("key_sequences[\"ctrl+x\"]: value invalid", "'\\q'"),
        ("unknown action \"no_such_action\"", "\"no_such_action\""),
    ];
    if crate::app_input::HARDCODED_SUPER_CHORDS {
        expected_spans.push(("chord \"cmd+c\" conflicts with built-in", "\"cmd+c\""));
    } else {
        assert!(
            !analysis
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains("conflicts with built-in")),
            "no built-in conflict with the suite gated off: {:?}",
            analysis.diagnostics
        );
    }
    for (message, token) in expected_spans {
        let diagnostic = analysis
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.message.contains(message))
            .unwrap_or_else(|| panic!("missing {message:?}: {:?}", analysis.diagnostics));
        assert_eq!(&source[diagnostic.bytes.clone()], token, "{message}");
    }
}

#[test]
fn the_autocompact_offer_key_takes_its_readers_spellings() {
    for value in ["\"400k\"", "\"off\"", "false", "true", "500", "600000"] {
        let source = format!("[harness]\nautocompact_offer = {value}\n");
        let analysis = analyze(&source);
        assert!(
            !analysis.has_errors(),
            "{value}: {:?}",
            analysis.diagnostics
        );
    }
}

#[test]
fn every_registered_enum_accepts_its_runtime_domain_and_aliases() {
    // A member of a list of records is read inside its record, never as a dotted key.
    let enums = config_schema()
        .iter()
        .filter(|setting| config_key_record_root(setting.key).is_none())
        .filter_map(|setting| match setting.kind {
            ConfigSchemaKind::Scalar(EditKind::Enum { options }) => Some((setting.key, options)),
            _ => None,
        })
        .collect::<Vec<_>>();

    let source_for = |key: &str, value: &str| format!("{key} = {value:?}\n");
    let admitted = |source: &str, what: &str| {
        let analysis = analyze(source);
        assert!(!analysis.has_errors(), "{what}: {:?}", analysis.diagnostics);
        let band = unaccepted_value_warnings(source, &[]);
        assert!(band.is_empty(), "{what}: {band:?}");
    };
    for (key, options) in &enums {
        for option in *options {
            admitted(
                &source_for(key, option),
                &format!("canonical {key}={option:?}"),
            );
        }
        let source = source_for(key, "definitely-not-a-real-enum-value");
        assert!(analyze(&source).has_errors(), "invalid {key} must fail");
    }

    for (key, aliases) in [
        (crate::prefs::EDIT_CURSOR_STYLE, CURSOR_STYLE_ALIASES),
        (crate::prefs::EDIT_BIDI, BIDI_ALIASES),
        (crate::prefs::EDIT_AMBIGUOUS_WIDTH, AMBIGUOUS_WIDTH_ALIASES),
        (crate::prefs::EDIT_PREDICTIVE_ECHO, PREDICTIVE_ECHO_ALIASES),
        (crate::prefs::EDIT_TEXT_BLENDING, TEXT_BLENDING_ALIASES),
        (crate::prefs::EDIT_MOTION, MOTION_ALIASES),
        (
            crate::prefs::EDIT_WINDOW_COLORSPACE,
            WINDOW_COLORSPACE_ALIASES,
        ),
        (
            crate::prefs::EDIT_BACKGROUND_MATERIAL,
            BACKGROUND_MATERIAL_ALIASES,
        ),
        ("windowing_behavior", WINDOWING_BEHAVIOR_ALIASES),
        ("right_click", RIGHT_CLICK_ALIASES),
        ("tab_menu_chord", TAB_MENU_CHORD_ALIASES),
        (
            crate::prefs::EDIT_HARNESS_AUTOPILOT,
            HARNESS_AUTOPILOT_ALIASES,
        ),
    ] {
        for alias in aliases {
            admitted(&source_for(key, alias), &format!("alias {key}={alias:?}"));
        }
    }
    for alias in aterm_effects::cursor_glow::spellings::CLASSIC_MONO_SPELLINGS {
        let source = source_for(crate::prefs::EDIT_CURSOR_TRAIL_STYLE, alias);
        admitted(&source, &format!("Classic's one-colour face {alias:?}"));
    }
    for &(alias, _) in aterm_effects::trail_sound::SoundVoice::ALIASES {
        let source = source_for(crate::prefs::EDIT_TRAIL_SOUND_STYLE, alias);
        admitted(&source, &format!("typing-sound alias {alias:?}"));
    }
}

#[test]
fn machine_policy_domain_matches_atpkg_and_bad_types_remain_errors() {
    use atpkg::config::{MachineConfig, UniversalControlPolicy};
    for (value, policy) in [
        ("off", UniversalControlPolicy::Off),
        ("leave", UniversalControlPolicy::Leave),
        (" off ", UniversalControlPolicy::Off),
        (" leave ", UniversalControlPolicy::Leave),
        ("", UniversalControlPolicy::Off),
        ("  ", UniversalControlPolicy::Off),
    ] {
        let fields = format!("universal_control = {value:?}\n");
        let machine: MachineConfig = aterm_toml::from_str(&fields).unwrap();
        assert_eq!(machine.universal_control(), policy, "{value:?}");
        let source = format!("[machine]\n{fields}");
        assert!(analyze(&source).diagnostics.is_empty(), "{value:?}");
        assert!(unaccepted_value_warnings(&source, &[]).is_empty());
        assert!(ignored_key_warnings(&source).is_empty(), "{value:?}");
    }
    for value in ["OFF", "Leave", "leav"] {
        let fields = format!("universal_control = {value:?}\n");
        let machine: MachineConfig = aterm_toml::from_str(&fields).unwrap();
        assert_eq!(machine.universal_control(), UniversalControlPolicy::Leave);
        let source = format!("[machine]\n{fields}");
        assert!(analyze(&source).has_errors(), "unsupported {value:?}");
        let warnings = unaccepted_value_warnings(&source, &[]).join("\n");
        assert!(warnings.contains("machine.universal_control"), "{warnings}");
    }
    let fields = "universal_control = true\n";
    assert!(aterm_toml::from_str::<MachineConfig>(fields).is_err());
    let source = format!("[machine]\n{fields}");
    assert!(analyze(&source).has_errors());
    assert!(!crate::app_config::unaccepted_value_notices(&source, &[]).is_empty());
    assert!(
        crate::app_config::ignored_key_notices(&source).is_empty(),
        "{source:?}"
    );
    assert!(analyze("machine = false\n").has_errors());
    assert!(!ignored_key_warnings("machine = false\n").is_empty());
    for value in ["true", "false"] {
        let source = format!("[machine]\nrosetta = {value}\n");
        assert!(analyze(&source).diagnostics.is_empty(), "{value}");
        assert!(ignored_key_warnings(&source).is_empty(), "{value}");
        assert_eq!(
            atpkg::config::parse_machine(&source).rosetta(),
            value == "true"
        );
    }
    let source = "[machine]\nrosetta = \"yes\"\n";
    assert!(analyze(source).has_errors(), "{source:?}");
    assert!(!atpkg::config::parse_machine(source).rosetta());
}

#[test]
fn retired_native_hinting_renders_as_light_and_is_never_offered() {
    assert!(!FONT_HINTINGS.contains(&"native"));
    let analysis = analyze("font_hinting = \"native\"\n");
    assert!(!analysis.has_errors(), "{:?}", analysis.diagnostics);
    let warnings = analysis
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity == ConfigDiagnosticSeverity::Warning)
        .map(|diagnostic| diagnostic.message.as_str())
        .collect::<Vec<_>>();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains("\"light\""), "{warnings:?}");
    assert!(unaccepted_enum_values("font_hinting = \"native\"\n").is_empty());
    assert!(analyze("font_hinting = \"bytecode\"\n").has_errors());
}

#[test]
fn nested_unknown_scalars_warn_but_manual_maps_lists_and_custom_tables_do_not() {
    let typo = analyze("[sparkle_words]\nenabld = false\n[sparkle_words.feline]\ngzae = true\n");
    assert!(!typo.has_errors());
    let messages = typo
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.message.as_str())
        .collect::<Vec<_>>();
    let named_as_unknown = |path: &str| {
        messages
            .iter()
            .any(|message| message.starts_with(path) && message.contains("no effect"))
    };
    assert!(named_as_unknown("sparkle_words.enabld"), "{messages:?}");
    assert!(
        named_as_unknown("sparkle_words.feline.gzae"),
        "{messages:?}"
    );

    let allowed = [
        r#"[keybindings]
"cmd+shift+k" = "new_tab"
[key_sequences]
"shift+enter" = '\e[13;2u'
"#,
        r#"[packages]
enabled = true
account = "alabsystems"
exclude = ["trust"]
[packages.links]
ay = "~/ay"
"#,
        r##"[sparkle_words]
languages = ["en"]
toy_packs = ["~/toy.toml"]
deny = ["plain"]
[sparkle_words.profanity]
palette = ["#ffffff"]
extra_words = ["zap"]
ignore_words = ["skip"]
[sparkle_words.feline]
extra_words = ["kit"]
ignore_words = ["skip"]
[sparkle_words.emphasis]
extra_words = ["wow"]
ignore_words = ["skip"]
"##,
        r#"[[sparkle_words.custom]]
words = ["ultrathink"]
ink = { colorway = "rainbow", sweep_once = true }
burst = { kind = "starburst", chance = 10 }
graphic = { collection = "cats" }
"#,
    ];
    for source in allowed {
        let analysis = analyze(source);
        assert!(
            analysis.diagnostics.is_empty(),
            "manual-native structure must be allowed: {:?}",
            analysis.diagnostics
        );
    }
}

#[test]
fn structured_record_typos_warn_per_record_without_blocking_or_hiding_repeats() {
    let source = r#"[[sparkle_words.custom]]
wrods = ["typo"]
ink = { colorway = "rainbow" }

[[sparkle_words.custom]]
words = ["valid"]
burst = { kind = "nova" }
"#;
    let analysis = analyze(source);
    assert!(
        !analysis.has_errors(),
        "unknown record members remain forward-compatible warnings: {:?}",
        analysis.diagnostics
    );
    let unknown = analysis
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.message.contains(" is unknown in [["))
        .collect::<Vec<_>>();
    assert_eq!(unknown.len(), 1, "{:?}", analysis.diagnostics);
    assert!(
        unknown[0].message.contains("sparkle_words.custom.wrods")
            && unknown[0].message.contains("record 1"),
        "{:?}",
        unknown[0].message
    );
    assert!(
        unknown
            .iter()
            .all(|diagnostic| diagnostic.severity == ConfigDiagnosticSeverity::Warning)
    );

    let clean_repeats = r##"[[sparkle_words.custom]]
words = ["one"]
ink = { colorway = "twotone:#112233,#445566" }

[[sparkle_words.custom]]
words = ["two"]
burst = { kind = "super-nova", chance = 10 }
graphic = { collection = "cats" }
"##;
    let clean = analyze(clean_repeats);
    assert!(
        clean.diagnostics.is_empty(),
        "valid repeated records stay clean: {:?}",
        clean.diagnostics
    );
}

#[test]
fn runtime_fallback_diagnostics_address_their_authored_tokens() {
    type Case = (
        &'static str,
        bool,
        &'static [(&'static str, &'static str)],
        bool,
    );
    let cases: &[Case] = &[
        (
            r##"font_px = 201
font_variation = ["", "bad-axis"]
font_features = ["+ss01", "toolong"]

[update]
owner = "bad/owner"

[matrix_rain]
hue = "blue"

[sparkle_words.profanity]
palette = ["#112233", "bad-color"]

[[sparkle_words.custom]]
words = [" "]

[[sparkle_words.custom]]
words = ["quiet"]
burst = { kind = "glow", chance = 0 }
"##,
            false,
            &[
                ("font_px: 201.0 is not accepted", "201"),
                ("font_variation[0]", "\"bad-axis\""),
                ("font_features[1]", "\"toolong\""),
                ("update.owner: \"bad/owner\"", "\"bad/owner\""),
                ("matrix_rain.hue value", "\"blue\""),
                ("sparkle_words.profanity.palette[1]", "\"bad-color\""),
                ("record 1 has no nonblank word", "[\" \"]"),
                ("record 1 is inert", "[[sparkle_words.custom]]"),
                ("record 2 is inert", "0"),
            ],
            false,
        ),
        (
            "sparkle_words.custom = [{ words = [\"quiet\"] }]\n",
            false,
            &[("record 1 is inert", "{ words = [\"quiet\"] }")],
            false,
        ),
        (
            "font_features = [\n  \"+ss01\",\n  \"toolong\",\n]\n",
            false,
            &[("font_features[1]", "\"toolong\"")],
            false,
        ),
        (
            "[[sparkle_words.custom]]\nink = { colorway = \"rainbow\" }\n",
            false,
            &[("has no nonblank word", "[[sparkle_words.custom]]")],
            false,
        ),
        (
            "[packages]\naccount = \"bad owner\"\n[packages.links]\nbad = \"a b\"\n",
            false,
            &[
                ("packages.account", "\"bad owner\""),
                ("packages.links.bad", "\"a b\""),
            ],
            false,
        ),
        (
            "[packages.links]\nay = 5\n",
            true,
            &[("packages.links.ay must be text", "5")],
            false,
        ),
        (
            "[[sparkle_words.custom]]\nwords = [\"zap\"]\n\
             burst = { kind = \"starburst\", chance = 101 }\n",
            false,
            &[("sparkle_words.custom.burst.chance", "101")],
            false,
        ),
        (
            "[sparkle_words.ink]\nloop = true\nsweep_ms = 400\n\
             [matrix_rain]\nalpha = 120\nhead_alpha = 20\n",
            false,
            &[("effectively 600 ms", "400"), ("effectively 120", "20")],
            true,
        ),
        (
            "allow_osc52_query = true\nallow_window_ops = true\n",
            false,
            &[("allow_osc52_query", "true"), ("allow_window_ops", "true")],
            true,
        ),
    ];
    for &(source, blocks, expected, exactly) in cases {
        let analysis = analyze(source);
        assert_eq!(
            analysis.has_errors(),
            blocks,
            "{source:?}: {:?}",
            analysis.diagnostics
        );
        if exactly {
            assert_eq!(
                analysis.diagnostics.len(),
                expected.len(),
                "{source:?}: one story each: {:?}",
                analysis.diagnostics
            );
        }
        for (fragment, token) in expected {
            let diagnostic = analysis
                .diagnostics
                .iter()
                .find(|diagnostic| diagnostic.message.contains(fragment))
                .unwrap_or_else(|| panic!("missing {fragment:?}: {:?}", analysis.diagnostics));
            assert_eq!(
                &source[diagnostic.bytes.clone()],
                *token,
                "{}",
                diagnostic.message
            );
        }
    }
    for clean in [
        "[packages.links]\nrepo = \"alabsystems/aterm\"\nhome = \"~/aterm\"\n",
        "[packages.links]\nay = \"~/ay\"\n",
        "[packages]\nlinks = { ay = \"~/ay\" }\n",
        "[[sparkle_words.custom]]\nwords = [\"zap\"]\n\
         burst = { kind = \"starburst\", chance = 100 }\n",
        "[sparkle_words.ink]\nloop = true\nsweep_ms = 600\n\
         [matrix_rain]\nalpha = 120\nhead_alpha = 120\n",
    ] {
        let analysis = analyze(clean);
        assert!(
            analysis.diagnostics.is_empty(),
            "{clean:?}: {:?}",
            analysis.diagnostics
        );
    }
}

#[test]
fn capability_warnings_are_backend_truthful_and_source_addressed() {
    use crate::diagnostics::{
        ConfigCapabilityPlatform as Platform, config_backend_capability_warnings as warnings,
    };
    let config_of =
        |source: &str| aterm_toml::from_str::<crate::app_config::Config>(source).unwrap();
    let backend = "background_opacity = 0.7\nbackground_material = \"hud\"\n\
window_colorspace = \"display-p3\"\n\
cursor_trail_bloom = false\n\
cursor_trail_bloom_strength = 1.25\n\
cursor_trail_bloom_radius = 3.5\n\
cursor_fire_shimmer = false\n\
hdr_glow = true\n\
cursor_glow_sdr_boost = 0.4\n";
    let platform = "trail_sounds = true\ntrail_sound_volume = 0.4\n\
confirm_multiline_paste = true\nallow_notifications = true\n";
    for (source, gpu, on, expected) in [
        (
            backend,
            false,
            Platform::MacOs,
            &[
                (crate::prefs::EDIT_BACKGROUND_OPACITY, "0.7"),
                (crate::prefs::EDIT_BACKGROUND_MATERIAL, "\"hud\""),
                (crate::prefs::EDIT_WINDOW_COLORSPACE, "\"display-p3\""),
                (crate::prefs::EDIT_CURSOR_TRAIL_BLOOM, "false"),
                (crate::prefs::EDIT_CURSOR_TRAIL_BLOOM_STRENGTH, "1.25"),
                (crate::prefs::EDIT_CURSOR_TRAIL_BLOOM_RADIUS, "3.5"),
                (crate::prefs::EDIT_CURSOR_FIRE_SHIMMER, "false"),
                (crate::prefs::EDIT_HDR_GLOW, "true"),
                (crate::prefs::EDIT_CURSOR_GLOW_SDR_BOOST, "0.4"),
            ][..],
        ),
        (
            platform,
            true,
            Platform::Unsupported,
            &[
                ("trail_sounds", "true"),
                ("trail_sound_volume", "0.4"),
                ("allow_notifications", "true"),
            ][..],
        ),
    ] {
        let document = source.parse::<aterm_toml::edit::DocumentMut>().unwrap();
        let mut analysis = ConfigAnalysis::default();
        append_semantic_warnings(
            source,
            &document,
            warnings(&config_of(source), gpu, on),
            &mut analysis,
        );
        assert_eq!(
            analysis.diagnostics.len(),
            expected.len(),
            "{:?}",
            analysis.diagnostics
        );
        for (key, token) in expected {
            let diagnostic = analysis
                .diagnostics
                .iter()
                .find(|diagnostic| diagnostic.message.starts_with(key))
                .unwrap_or_else(|| panic!("missing capability diagnostic for {key}"));
            assert_eq!(&source[diagnostic.bytes.clone()], *token, "{key}");
        }
    }
    assert!(
        warnings(&config_of(backend), true, Platform::MacOs).is_empty(),
        "every value has a consumer on a translucent macOS GPU window"
    );
    let windows = warnings(&config_of(backend), true, Platform::Windows);
    assert!(
        windows
            .iter()
            .any(|warning| warning.key == crate::prefs::EDIT_BACKGROUND_OPACITY),
        "Windows GPU must disclose that per-pixel grid opacity is inert"
    );
    assert!(
        windows
            .iter()
            .all(|warning| warning.key != crate::prefs::EDIT_BACKGROUND_MATERIAL),
        "Windows GPU consumes materials as DWM backdrops without translucency"
    );
    assert!(
        windows
            .iter()
            .any(|warning| { warning.key == crate::prefs::EDIT_WINDOW_COLORSPACE })
    );

    let opaque = warnings(
        &config_of("background_material = \"sidebar\"\n"),
        true,
        Platform::MacOs,
    );
    assert_eq!(opaque.len(), 1);
    assert_eq!(opaque[0].key, crate::prefs::EDIT_BACKGROUND_MATERIAL);
    assert!(
        warnings(
            &config_of("background_material = \"sidebar\"\n"),
            true,
            Platform::Windows,
        )
        .is_empty(),
        "Windows DWM material is independent of background opacity"
    );
    assert!(
        warnings(&config_of(backend), true, Platform::Unsupported)
            .iter()
            .any(|warning| warning.key == crate::prefs::EDIT_BACKGROUND_MATERIAL)
    );
}

#[test]
fn custom_effect_fallback_values_warn_and_offer_runtime_canonical_completions() {
    let source = r#"[[sparkle_words.custom]]
words = ["one"]
ink = { colorway = "sepia" }
burst = { kind = "flashbang" }
graphic = { collection = "dogs" }

[[sparkle_words.custom]]
words = ["two"]
ink = { colorway = "twotone:#112233,#445566,#778899" }
"#;
    let analysis = analyze(source);
    assert!(!analysis.has_errors());
    for (path, fallback) in [
        ("sparkle_words.custom.ink.colorway", "rainbow"),
        ("sparkle_words.custom.burst.kind", "starburst"),
        ("sparkle_words.custom.graphic.collection", "cats"),
    ] {
        let diagnostic = analysis
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.message.starts_with(path))
            .unwrap_or_else(|| {
                panic!(
                    "missing fallback warning for {path}: {:?}",
                    analysis.diagnostics
                )
            });
        assert_eq!(diagnostic.severity, ConfigDiagnosticSeverity::Warning);
        assert!(
            diagnostic.message.contains(fallback),
            "{}",
            diagnostic.message
        );
        assert!(
            diagnostic.message.contains("record 1"),
            "{}",
            diagnostic.message
        );
    }
    let extra_twotone = analysis
        .diagnostics
        .iter()
        .find(|diagnostic| {
            diagnostic
                .message
                .starts_with("sparkle_words.custom.ink.colorway")
                && diagnostic.message.contains("record 2")
        })
        .expect("three-color twotone warning");
    assert_eq!(
        &source[extra_twotone.bytes.clone()],
        "\"twotone:#112233,#445566,#778899\""
    );

    for (source, expected) in [
        ("[sparkle_words.custom.ink]\ncolorway = \"", "\"rainbow\""),
        ("[sparkle_words.custom.burst]\nkind = \"n", "\"nova\""),
        (
            "[sparkle_words.custom.graphic]\ncollection = \"",
            "\"cats\"",
        ),
    ] {
        assert!(
            assist(source, source.len())
                .completions
                .iter()
                .any(|completion| completion.insertion == expected),
            "missing {expected} completion in {source:?}"
        );
    }
}

#[test]
fn saturated_range_warnings_cannot_hide_invalid_package_link_type() {
    let mut source = String::new();
    let mut authored = 0usize;
    for setting in config_schema() {
        let ConfigSchemaKind::Scalar(kind @ (EditKind::Float | EditKind::Integer)) = setting.kind
        else {
            continue;
        };
        let Some(range) = crate::prefs::range_of(setting.key) else {
            continue;
        };
        let outside = range.max + range.step.max(1.0);
        let value = if matches!(kind, EditKind::Integer) {
            format!("{outside:.0}")
        } else {
            outside.to_string()
        };
        source.push_str(&format!("{} = {value}\n", setting.key));
        authored += 1;
        if authored == MAX_DIAGNOSTICS {
            break;
        }
    }
    assert_eq!(authored, MAX_DIAGNOSTICS, "range-warning fixture drifted");
    source.push_str("[packages.links]\nay = 1\n");

    let analysis = analyze(&source);
    assert!(
        analysis.has_errors(),
        "late schema error must block Manual Save"
    );
    assert!(analysis.diagnostics.iter().any(|diagnostic| {
        diagnostic.severity == ConfigDiagnosticSeverity::Error
            && diagnostic
                .message
                .contains("packages.links.ay must be text")
    }));
    assert!(analysis.omitted_diagnostics > 0);
}

#[test]
fn key_lines_past_the_cap_end_in_one_count() {
    let source = (0..MAX_DIAGNOSTICS + 3)
        .map(|i| format!("qqq_unknown_{i} = 1\n"))
        .collect::<String>();
    let lines = ignored_key_warnings(&source);
    assert_eq!(lines.len(), MAX_DIAGNOSTICS + 1, "{lines:?}");
}

#[test]
fn literal_dotted_keys_are_unknown_and_cannot_impersonate_nested_schema_paths() {
    let source = r#""packages.exclude" = ["literal"]

[packages]
exclude = ["nested"]
"#;
    let analysis = analyze(source);
    assert!(!analysis.has_errors(), "{:?}", analysis.diagnostics);
    let messages = analysis
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.message.as_str())
        .collect::<Vec<_>>();
    assert!(
        messages
            .iter()
            .any(|message| message.contains(r#""packages.exclude" is unknown"#)),
        "the literal segment must retain an unknown, forward-compatible identity: {messages:?}"
    );
    assert!(
        messages
            .iter()
            .all(|message| !message.contains("packages.exclude is unknown")),
        "the nested registered path must not be diagnosed as unknown: {messages:?}"
    );

    let literal_value = r#""packages.exclude" = "#;
    let literal_assist = assist(literal_value, literal_value.len());
    assert!(literal_assist.completions.is_empty());
    assert!(literal_assist.help.is_none());

    let quoted_nested_value = "[packages]\n\"exclude\" = ";
    let nested_assist = assist(quoted_nested_value, quoted_nested_value.len());
    assert!(nested_assist.completions.iter().any(|completion| {
        completion.insertion == "[]" && completion.help.contains("list of text values")
    }));
}

#[test]
fn diagnostics_ignore_literal_and_multiline_lookalikes_when_locating_nested_values() {
    let source = r#"future_note = """
[matrix_rain] # fake header
fps = 5 # fake value
"""
"matrix_rain.fps" = 7

[matrix_rain]
fps = 999
"#;
    let analysis = analyze(source);
    assert!(!analysis.has_errors(), "{:?}", analysis.diagnostics);
    let range_warning = analysis
        .diagnostics
        .iter()
        .find(|diagnostic| {
            diagnostic.message.contains("matrix_rain.fps")
                && diagnostic.message.contains("outside the supported")
        })
        .expect("the real nested value receives its registered range warning");
    assert_eq!(
        &source[range_warning.bytes.clone()],
        "999",
        "neither a fake multiline assignment nor a literal dotted segment may steal the diagnostic range"
    );
    assert!(analysis.diagnostics.iter().any(|diagnostic| {
        diagnostic
            .message
            .contains(r#""matrix_rain.fps" is unknown"#)
    }));
}

#[test]
fn every_offered_key_and_value_completion_is_error_free_after_acceptance() {
    for setting in config_schema()
        .iter()
        .filter(|setting| setting.kind.is_assignable() && !is_compatibility_only_key(setting.key))
    {
        let (base, local) = match array_table_root(setting.key) {
            None => {
                let (table, local) = setting
                    .key
                    .rsplit_once('.')
                    .map_or(("", setting.key), |(table, local)| (table, local));
                let base = if table.is_empty() {
                    String::new()
                } else {
                    format!("[{table}]\n")
                };
                assert!(
                    !analyze(&base).has_errors(),
                    "completion base for {} must be valid",
                    setting.key
                );
                (base, local)
            }
            Some(record_root) => {
                let relative = setting
                    .key
                    .strip_prefix(record_root)
                    .and_then(|suffix| suffix.strip_prefix('.'))
                    .expect("record member path");
                let (parent, local) = relative
                    .rsplit_once('.')
                    .map_or(("", relative), |(parent, local)| (parent, local));
                let mut base = format!("[[{record_root}]]\n");
                if local != "words" && parent.is_empty() {
                    base.push_str("words = []\n");
                }
                if !parent.is_empty() {
                    base.push_str("words = []\n");
                    base.push_str(&format!("[{record_root}.{parent}]\n"));
                    if relative == "ink.sweep_once" {
                        base.push_str("colorway = \"rainbow\"\n");
                    }
                }
                (base, local)
            }
        };

        let key_source = format!("{base}{local}");
        let key_assist = assist(&key_source, key_source.len());
        let key_completion = key_assist
            .completions
            .iter()
            .find(|completion| completion.insertion.starts_with(&format!("{local} = ")))
            .unwrap_or_else(|| {
                panic!(
                    "{} must be offered from its exact prefix: {:?}",
                    setting.key, key_assist
                )
            });
        let mut completed = key_source.clone();
        completed.replace_range(
            key_completion.replacement.clone(),
            &key_completion.insertion,
        );
        let completed_analysis = analyze(&completed);
        assert!(
            !completed_analysis.has_errors(),
            "{} key completion produced errors in {completed:?}: {:?}",
            setting.key,
            completed_analysis.diagnostics
        );

        let value_source = format!("{base}{local} = ");
        for value_completion in assist(&value_source, value_source.len()).completions {
            let mut completed_value = value_source.clone();
            completed_value.replace_range(
                value_completion.replacement.clone(),
                &value_completion.insertion,
            );
            let value_analysis = analyze(&completed_value);
            assert!(
                !value_analysis.has_errors(),
                "{} value completion {:?} produced errors: {:?}",
                setting.key,
                value_completion.insertion,
                value_analysis.diagnostics
            );
        }
    }
}

#[test]
fn contextual_assist_is_derived_from_registered_metadata() {
    let top = assist("win", 3);
    let window = top
        .completions
        .iter()
        .find(|candidate| candidate.insertion.starts_with("window_theme ="))
        .expect("window-theme completion");
    assert!(!window.help.is_empty());

    let nested_source = "[sparkle_words]\nena";
    let nested = assist(nested_source, nested_source.len());
    assert!(
        nested
            .completions
            .iter()
            .any(|candidate| candidate.insertion == "enabled = true")
    );

    let bool_source = "[packages]\nenabled = ";
    assert_eq!(
        assist(bool_source, bool_source.len())
            .completions
            .iter()
            .map(|completion| completion.insertion.as_str())
            .collect::<Vec<_>>(),
        ["true", "false"]
    );

    let value_source = "window_theme = \"d";
    let values = assist(value_source, value_source.len());
    assert!(
        values
            .completions
            .iter()
            .any(|candidate| candidate.insertion == "\"dark\"")
    );

    let themes = aterm_types::scheme::builtin_names();
    assert!(themes.len() > MAX_COMPLETIONS);
    let later_theme = themes[MAX_COMPLETIONS];
    let theme_source = format!("theme = \"{later_theme}");
    let theme_values = assist(&theme_source, theme_source.len());
    assert!(
        theme_values
            .completions
            .iter()
            .any(|candidate| { candidate.insertion == format!("\"{later_theme}\"") })
    );
}

#[test]
fn theme_completion_offers_catalog_themes_and_the_split_form() {
    let source = "theme = \"";
    let mut catalog_less = analyze(source);
    let offered = |analysis: &ConfigAnalysis| {
        assist_with_analysis(source, source.len(), analysis)
            .completions
            .into_iter()
            .map(|completion| completion.insertion)
            .collect::<Vec<_>>()
    };
    let before = offered(&catalog_less);
    assert!(!before.iter().any(|insertion| insertion == "\"harbour\""));
    assert!(
        before.contains(&"\"dark:Default,light:Default Light\"".to_string()),
        "{before:?}"
    );
    let themes = crate::app_config::ThemeCatalog::from_schemes([(
        "harbour".to_string(),
        aterm_types::scheme::builtin("Nord").unwrap(),
    )]);
    catalog_less.adopt_theme_names(&themes);
    let typed = "theme = \"har";
    let after = assist_with_analysis(typed, typed.len(), &catalog_less)
        .completions
        .into_iter()
        .map(|completion| completion.insertion)
        .collect::<Vec<_>>();
    assert_eq!(after, ["\"harbour\""]);
}

fn accept_completion(source: &str, caret: usize, insertion: &str) -> String {
    let assistance = assist(source, caret);
    let candidate = assistance
        .completions
        .iter()
        .find(|completion| completion.insertion == insertion)
        .unwrap_or_else(|| panic!("{insertion:?} offered at {caret} in {source:?}"));
    let mut completed = source.to_string();
    completed.replace_range(candidate.replacement.clone(), &candidate.insertion);
    assert!(
        !analyze(&completed).has_errors(),
        "accepting {insertion:?} must leave a valid config: {completed:?}"
    );
    completed
}

#[test]
fn completion_suppresses_authored_paths_but_keeps_the_edited_token_and_records_repeatable() {
    let top_level = "theme = \"Nord\"\n";
    let top = assist(top_level, top_level.len());
    assert!(
        top.completions
            .iter()
            .all(|completion| !completion.insertion.starts_with("theme = "))
    );
    let other = top.completions.first().expect("another top-level scalar");
    accept_completion(top_level, top_level.len(), &other.insertion);

    let nested = "[packages]\nenabled = true\n";
    assert!(
        assist(nested, nested.len())
            .completions
            .iter()
            .all(|completion| !completion.insertion.starts_with("enabled = "))
    );
    accept_completion(nested, nested.len(), "exclude = []");

    let table = format!("{top_level}\n[pa");
    accept_completion(&table, table.len(), "[packages]");
    let duplicate_table = format!("{top_level}\n[packages]\n\n[");
    assert!(
        assist(&duplicate_table, duplicate_table.len())
            .completions
            .iter()
            .all(|completion| completion.insertion != "[packages]")
    );

    accept_completion("[packages]\n", "[pack".len(), "[packages]");

    let first_record = "[[sparkle_words.custom]]\nwords = [\"one\"]\nink = \"rainbow\"\n";
    let repeated = format!("{first_record}\n[[\nwords = [\"two\"]\nink = \"rainbow\"\n");
    accept_completion(
        &repeated,
        first_record.len() + "\n[[".len(),
        "[[sparkle_words.custom]]",
    );
    let nested_repeated = r#"[[sparkle_words.custom]]
words = ["one"]
[sparkle_words.custom.burst]
kind = "nova"

[[sparkle_words.custom]]
words = ["two"]

[sparkle_words.custom.b"#;
    accept_completion(
        nested_repeated,
        nested_repeated.len(),
        "[sparkle_words.custom.burst]",
    );
}

#[test]
fn assist_is_inert_inside_multiline_strings_and_ignores_fake_context() {
    let all = ["[packages]", "# not a comment", "exclude ="];
    for (source, markers) in [
        (
            r#"[sparkle_words.profanity]
note = """
[packages] # not a table
# not a comment
exclude =
"""
sty"#,
            &all[..],
        ),
        (
            r#"[sparkle_words.profanity]
note = '''
[packages] # not a table
# not a comment
exclude =
'''
sty"#,
            &all[..],
        ),
        (
            r#"[sparkle_words.profanity]
note = """
escaped delimiter: \"""
[packages]
exclude =
"""
sty"#,
            &all[2..],
        ),
    ] {
        for marker in markers {
            let caret = source.find(marker).unwrap() + marker.len();
            assert_eq!(
                assist(source, caret),
                ConfigAssist::default(),
                "assistance must be inert at {marker:?} inside a multiline string"
            );
        }

        let after = assist(source, source.len());
        assert!(
            after
                .completions
                .iter()
                .any(|completion| completion.insertion.starts_with("style = ")),
            "the fake [packages] header must not replace the real profanity table: {after:?}"
        );
        assert!(
            after
                .completions
                .iter()
                .all(|completion| !completion.insertion.starts_with("exclude = "))
        );
    }
}

#[test]
fn worker_built_assist_index_matches_standalone_language_results() {
    for source in [
        "",
        "win",
        "[packages]\ninc",
        "[sparkle_words.profanity]\nnote = \"\"\"\n[packages]\n\"\"\"\nsty",
        "[keybindings]\n\"literal.key\" = \"value\"\n",
        "[[foo]]\ndisk.apply = false\n",
    ] {
        let analysis = analyze(source);
        for caret in (0..=source.len()).filter(|caret| source.is_char_boundary(*caret)) {
            assert_eq!(
                assist_with_analysis(source, caret, &analysis),
                assist(source, caret),
                "indexed assistance diverged at byte {caret} in {source:?}"
            );
        }
    }
}

#[test]
fn completion_replaces_the_full_key_or_value_token_from_a_mid_token_caret() {
    let source = "window_theme = \"dark\" # keep this comment\n";

    let key_assistance = assist(source, 3);
    let key = key_assistance
        .completions
        .iter()
        .find(|candidate| candidate.display.starts_with("window_theme —"))
        .expect("mid-key completion");
    assert_eq!(&source[key.replacement.clone()], "window_theme");
    assert_eq!(key.expected, "window_theme");
    assert_eq!(key.insertion, "window_theme");
    let mut completed_key = source.to_string();
    completed_key.replace_range(key.replacement.clone(), &key.insertion);
    assert_eq!(completed_key, source);

    let value_start = source.find("\"dark\"").unwrap();
    let value_assistance = assist(source, value_start + 2);
    let value = value_assistance
        .completions
        .iter()
        .find(|candidate| candidate.insertion == "\"dark\"")
        .expect("mid-value completion");
    assert_eq!(&source[value.replacement.clone()], "\"dark\"");
    assert_eq!(value.expected, "\"dark\"");
    let mut completed_value = source.to_string();
    completed_value.replace_range(value.replacement.clone(), &value.insertion);
    assert_eq!(completed_value, source);
}

#[test]
fn completion_actions_bind_document_caret_range_and_candidate() {
    let source = "win";
    let assist = assist(source, source.len());
    let (index, completion) = assist
        .completions
        .iter()
        .enumerate()
        .find(|(_, candidate)| candidate.insertion.starts_with("window_theme ="))
        .expect("window-theme completion");
    let context = ConfigCompletionContext::new(7, 11, source.len());
    let action = config_completion_action(context, index, completion);
    assert!(is_config_completion_action(&action));
    assert!(action.len() <= MAX_COMPLETION_ACTION_BYTES);
    let wire = format!("app/v1 view 1 editor/config-completion/{index} {action}");
    assert!(
        aterm_types::app_inspection::parse_act(&wire).is_ok(),
        "bound completion action must fit the public control protocol"
    );
    assert_eq!(
        resolve_config_completion_action(source, context, &action),
        Some(completion.clone())
    );

    for stale in [
        ConfigCompletionContext::new(8, 11, source.len()),
        ConfigCompletionContext::new(7, 12, source.len()),
        ConfigCompletionContext::new(7, 11, source.len() - 1),
    ] {
        assert_eq!(
            resolve_config_completion_action(source, stale, &action),
            None
        );
    }

    let mut wrong_range = completion.clone();
    wrong_range.replacement.end -= 1;
    let wrong_range_action = config_completion_action(context, index, &wrong_range);
    assert_eq!(
        resolve_config_completion_action(source, context, &wrong_range_action),
        None
    );

    let mut wrong_candidate = completion.clone();
    wrong_candidate.insertion.push('x');
    let wrong_candidate_action = config_completion_action(context, index, &wrong_candidate);
    assert_eq!(
        resolve_config_completion_action(source, context, &wrong_candidate_action),
        None
    );

    let mut wrong_selection = completion.clone();
    wrong_selection.post_insert_selection = 0..0;
    let wrong_selection_action = config_completion_action(context, index, &wrong_selection);
    assert_eq!(
        resolve_config_completion_action(source, context, &wrong_selection_action),
        None,
        "post-insert editing intent is part of the stale-checked identity"
    );
}

#[test]
fn analysis_and_context_are_strictly_bounded() {
    let source = "x".repeat(MAX_CONFIG_ANALYSIS_BYTES + 1);
    let analysis = analyze(&source);
    assert!(analysis.has_errors());
    assert!(analysis.syntax.len() <= MAX_SYNTAX_SPANS);
    assert_eq!(assist(&source, source.len()), ConfigAssist::default());

    let source = format!("{}é", "x".repeat(MAX_CONFIG_ANALYSIS_BYTES - 1));
    assert!(!source.is_char_boundary(MAX_CONFIG_ANALYSIS_BYTES));
    let analysis = analyze(&source);
    assert!(analysis.has_errors());
    assert!(analysis.syntax.iter().all(|span| {
        source.is_char_boundary(span.bytes.start) && source.is_char_boundary(span.bytes.end)
    }));
}

#[test]
fn decoration_projects_only_visible_syntax_and_diagnostics() {
    let source = "font_px = \"large\"\n";
    let analysis = analyze(source);
    let mut projection = EditorViewportProjection {
        first_line: 1,
        total_lines: 1,
        lines: vec![crate::native_editor::EditorViewportLine {
            number: 1,
            source: 0..source.trim_end_matches('\n').len(),
            column_start: 0,
            text: source.trim_end_matches('\n').to_string(),
            selections: Vec::new(),
            carets: Vec::new(),
            syntax: Vec::new(),
            diagnostics: Vec::new(),
        }],
    };

    decorate_projection(&mut projection, &analysis);
    let line = &projection.lines[0];
    assert!(line.syntax.iter().any(|span| {
        span.class == EditorSyntaxClass::Key && &line.text[span.bytes.clone()] == "font_px"
    }));
    assert!(line.diagnostics.iter().any(|diagnostic| diagnostic.error));
    assert!(
        line.syntax.len() <= analysis.syntax.len()
            && line.diagnostics.len() <= analysis.diagnostics.len()
    );
}

fn projection_pin_source() -> String {
    let wide = "wide ".repeat(80);
    format!(
        "# aterm config\n\
         [window]\n\
         font_px = 18   # trailing comment after a key\n\
         title = \"α terminal ✨\"   # multibyte value\n\
         \n\
         opacity = 0.85\n\
         notes = \"{wide}\"   # forces a horizontally sliced window\n\
         [window.padding]\n\
         left = 4\n"
    )
}

fn for_each_config_projection(source: &str, mut inspect: impl FnMut(&EditorViewportProjection)) {
    let mut store = crate::test_document_store();
    let document = store.open("mem://config-projection".to_string(), source.to_string());
    let mut workspace = crate::native_editor::EditorWorkspace::new();
    let mut view = workspace
        .attach(
            &mut store,
            document,
            crate::document_store::DocumentViewId(91),
        )
        .expect("attaching a freshly opened in-memory document cannot fail");
    for anchor in (0..=source.len()).step_by(23) {
        for caret in (0..=source.len()).step_by(37) {
            view.viewport_anchor = anchor;
            view.selections = vec![crate::native_editor::Selection::caret(caret)];
            for (rows, columns) in [(1usize, 4usize), (3, 12), (7, 40), (usize::MAX, usize::MAX)] {
                inspect(&crate::native_editor::project_viewport(
                    source, &view, rows, columns,
                ));
            }
        }
    }
}

#[test]
fn windowed_decoration_matches_a_full_sweep() {
    let source = projection_pin_source();
    let analysis = analyze(&source);
    assert!(
        !analysis.syntax.is_empty(),
        "the pinned source must actually lex to spans"
    );
    for_each_config_projection(&source, |projection| {
        let mut decorated = projection.clone();
        decorate_projection(&mut decorated, &analysis);
        for (line, original) in decorated.lines.iter().zip(&projection.lines) {
            let swept: Vec<EditorSyntaxSpan> = analysis
                .syntax
                .iter()
                .filter_map(|span| {
                    relative_intersection(&original.source, &span.bytes).map(|bytes| {
                        EditorSyntaxSpan {
                            bytes,
                            class: span.class,
                        }
                    })
                })
                .collect();
            assert_eq!(
                line.syntax, swept,
                "windowed scan diverged from the full sweep on {:?}",
                original.source
            );
        }
    });
}

fn only_message(source: &str) -> String {
    let analysis = analyze(source);
    assert_eq!(
        analysis.diagnostics.len(),
        1,
        "expected exactly one diagnostic for {source:?}: {:?}",
        analysis.diagnostics
    );
    analysis.diagnostics[0].message.clone()
}

#[test]
fn a_mistyped_key_is_told_the_spelling_that_works() {
    for (source, intended) in [
        ("cursor_trail_stlye = 1\n", "cursor_trail_style"),
        ("colums = 1\n", "columns"),
        ("scrollback_lnies = 1\n", "scrollback_lines"),
        ("Columns = 100\n", "columns"),
        ("[keybindigns]\n\"ctrl+t\" = \"new_tab\"\n", "keybindings"),
        ("cursor_style = \"blok\"\n", "block"),
    ] {
        let message = only_message(source);
        assert!(
            message.contains(&format!("did you mean {intended:?}")),
            "{source:?} must name {intended}: {message}"
        );
    }
}

#[test]
fn a_suggestion_is_always_a_key_the_loader_accepts_and_never_a_working_one() {
    let suggestable = suggestable_config_paths();
    for dead in RETIRED_CONFIG_KEYS
        .iter()
        .map(|retired| retired.key)
        .chain(COMPATIBILITY_ONLY_KEYS.iter().copied())
    {
        assert!(
            !suggestable.contains(&dead),
            "{dead} must not be suggestable"
        );
    }
    for entry in config_schema() {
        assert_eq!(
            nearest_config_key(entry.key),
            None,
            "{} is registered and must never be corrected",
            entry.key
        );
    }
}

#[test]
fn the_network_drives_leftovers_are_told_once_as_removed() {
    for (text, key) in [
        (
            "[net]\nlisten = \"0.0.0.0:7100\"\ncert = \"c.der\"\nkey = \"k.der\"\n",
            "net",
        ),
        (
            "[[net.connections]]\nname = \"box\"\nhost = \"box:7100\"\n",
            "net.connections",
        ),
    ] {
        let warnings = key_warnings(text);
        assert!(warnings.ignored.is_empty(), "{text:?}: {warnings:?}");
        assert_eq!(warnings.retired.len(), 1, "{text:?}: {warnings:?}");
        let line = &warnings.retired[0];
        assert!(
            line.starts_with(&format!(
                "line 1: {key} does nothing: Network drive was removed"
            )),
            "{line}"
        );
        assert!(line.contains("aterm fleet join"), "{line}");
    }
}

#[test]
fn the_near_miss_budget_scales_with_how_much_of_the_name_is_left() {
    assert_eq!(
        nearest_config_key("gpx"),
        Some("gpu"),
        "one edit, short key"
    );
    assert_eq!(nearest_config_key("xzq"), None, "two edits, short key");
    assert_eq!(
        nearest_config_key("scrolback_lnes"),
        Some("scrollback_lines"),
        "two edits, long key"
    );
    assert_eq!(nearest_config_key("theme_of_everything"), None);
    assert_eq!(
        nearest_enum_value("wildly-different", &["block", "bar"]),
        None
    );
}

#[test]
fn every_registered_enum_key_is_graded_on_its_spelling_and_on_its_value() {
    let mut graded = 0;
    for setting in config_schema() {
        let ConfigSchemaKind::Scalar(EditKind::Enum { .. }) = setting.kind else {
            continue;
        };
        if is_compatibility_only_key(setting.key) {
            continue;
        }
        if config_key_record_root(setting.key).is_some() {
            // A record's member is graded inside its record (`warn_unknown_structured_records`).
            continue;
        }
        graded += 1;
        let mistyped = format!("{}zq = \"x\"\n", setting.key);
        assert!(
            ignored_key_warnings(&mistyped)
                .iter()
                .any(|line| line.contains(setting.key)),
            "a mistyped {} must be reported: {:?}",
            setting.key,
            ignored_key_warnings(&mistyped)
        );
        let refused = format!("{} = \"zq not a value\"\n", setting.key);
        assert!(
            unaccepted_value_warnings(&refused, &[])
                .iter()
                .any(|line| line.starts_with(setting.key)),
            "a value {} does not accept must be reported: {:?}",
            setting.key,
            unaccepted_value_warnings(&refused, &[])
        );
    }
    assert!(graded > 20, "the registry sweep found only {graded} keys");
}

#[test]
fn a_line_that_already_named_the_value_suppresses_the_generic_one_but_a_platform_note_does_not() {
    let source = "window_theme = \"drak\"\n";
    assert!(
        unaccepted_value_warnings(
            source,
            &["window_theme: expected auto|light|dark, got \"drak\"; using auto".to_string()],
        )
        .is_empty(),
        "a line naming the refused value has told this story"
    );
    assert_eq!(
        unaccepted_value_warnings(
            source,
            &["window_theme has no effect on this platform".to_string()],
        )
        .len(),
        1,
        "a platform note never said the value was wrong"
    );
}

#[test]
fn a_removed_features_key_is_a_record_and_a_typo_is_not() {
    let warned = key_warnings("show_hud = true\ncolums = 100\n[packages]\nchannel = \"beta\"\n");
    assert!(
        warned
            .retired
            .iter()
            .any(|l| l.contains("show_hud does nothing")),
        "{warned:?}"
    );
    assert!(
        warned
            .retired
            .iter()
            .any(|l| l.contains("packages.channel")),
        "{warned:?}"
    );
    assert!(
        warned
            .ignored
            .iter()
            .all(|l| !l.contains("show_hud") && !l.contains("channel")),
        "{warned:?}"
    );
    assert!(
        warned.ignored.iter().any(|l| l.contains("colums")),
        "{warned:?}"
    );
    let all = ignored_key_warnings("theme = \"Nord\"\ncolums = 100\nshow_hud = true\n");
    assert_eq!(all.len(), 2, "{all:?}");
    assert!(
        all.iter()
            .any(|line| line.starts_with("line 2:") && line.contains("\"columns\"")),
        "{all:?}"
    );
    assert!(
        all.iter().any(|line| line.starts_with("line 3:")),
        "{all:?}"
    );
    assert!(ignored_key_warnings("theme = \"Nord\"\n").is_empty());
    assert!(unaccepted_value_warnings("columns = 100\n", &[]).is_empty());
    assert!(ignored_key_warnings("theme = \"Nord\n").is_empty());
    assert!(unaccepted_value_warnings("theme = \"Nord\n", &[]).is_empty());
}

#[test]
fn every_ai_key_is_known_to_the_config_language() {
    let fleet = if cfg!(windows) {
        "C:/dotfiles/ai-fleet.toml"
    } else {
        "/dotfiles/ai-fleet.toml"
    };
    let mut text = format!(
        "[ai]\nenabled = true\npreferred = \"codex\"\nreviewers = [\"codex\"]\n\
         review_count = 2\nreview_timeout_s = 120\nmachine = \"m7\"\n\
         fleet = \"{fleet}\"\nreviews = true\n\
         review_permissions = \"inherit\"\n\
         fleet_key = \"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=\"\n",
    );
    for id in aterm_agent::ai::connector::ids() {
        text.push_str(&format!("[ai.{id}]\nuse = false\n"));
    }
    let (_, notes) = aterm_agent::ai::config::read(&text);
    assert!(notes.is_empty(), "the AI reader: {notes:?}");
    assert!(
        ignored_key_warnings(&text).is_empty(),
        "{:?}",
        ignored_key_warnings(&text)
    );
    for key in ["ai.reviewers", "ai.review_count", "ai.review_timeout_s"] {
        assert!(
            crate::prefs::nested_leaf(key).is_none(),
            "{key} has no Settings row"
        );
        assert!(
            MANUAL_SCHEMA.iter().any(|e| e.key == key),
            "{key} is a Manual row"
        );
    }
    let mut rows: Vec<String> = [
        crate::prefs::EDIT_AI_ENABLED,
        crate::prefs::EDIT_AI_PREFERRED,
        crate::prefs::EDIT_AI_REVIEWS,
        crate::prefs::EDIT_AI_REVIEW_PERMISSIONS,
        crate::prefs::EDIT_AI_MACHINE,
        crate::prefs::EDIT_AI_FLEET,
        crate::prefs::EDIT_AI_FLEET_KEY,
    ]
    .map(str::to_string)
    .to_vec();
    rows.extend(
        aterm_agent::ai::connector::ids()
            .into_iter()
            .map(|id| format!("ai.{id}.use")),
    );
    for key in &rows {
        assert!(
            crate::prefs::nested_leaf(key).is_some()
                && !MANUAL_SCHEMA.iter().any(|e| e.key == key.as_str()),
            "{key} is a Settings row: one registration, in NESTED_LEAVES"
        );
        assert!(
            config_schema().iter().any(|e| e.key == key.as_str()),
            "{key} is not served by config_schema()"
        );
    }
    for wrong in [
        "[ai]\nreviewer = [\"codex\"]\n",
        "[ai.nonesuch]\nuse = true\n",
    ] {
        assert!(
            !aterm_agent::ai::config::read(wrong).1.is_empty(),
            "{wrong}"
        );
        assert!(!ignored_key_warnings(wrong).is_empty(), "{wrong}");
    }
}

#[test]
fn a_disk_value_the_reader_cannot_read_is_told_once_in_its_words() {
    use crate::message_reporters::{ConfigFamily, ConfigWarnings};
    let dir = aterm_tempfile::TempDir::new().unwrap();
    let file = dir.path().join("aterm.toml");
    let reader = |text: &str| {
        crate::test_scratch::write_config(&file, text);
        aterm_agent::harness::cli::disk_config_read(Some(&file))
    };
    let record =
        |name: &str| format!("[[sparkle_words.custom]]\nwords = [\"{name}\"]\nink = \"rainbow\"\n");
    let cases: Vec<(String, usize)> = vec![
        ("disk = 5\n".into(), 1),
        ("[[disk]]\napply = false\n".into(), 1),
        ("disk = { apply = \"yes\" }\n".into(), 1),
        ("[disk]\napply = \"yes\"\n".into(), 2),
        ("[disk]\nauto_free_gib = 20.0\n".into(), 2),
        ("[disk]\ntarget_stale_days = -1.0\n".into(), 2),
        ("[harness]\ndisk.auto_free_gib = 0\n".into(), 2),
        (format!("{}disk.auto_free_gib = 0\n", record("box")), 4),
        (
            format!("{}{}disk.apply = false\n", record("a"), record("b")),
            7,
        ),
        ("[[foo]]\ndisk.apply = false\n".into(), 2),
        ("[harness.approve_all]\ndisk.apply = false\n".into(), 2),
    ];
    for (text, line) in &cases {
        let reading = reader(text);
        let off = reading.config;
        assert!(
            !off.apply && off.auto_free_gib == 0 && !off.dead_build_dirs,
            "{text:?}: {reading:?}"
        );
        let [note] = reading.notes.as_slice() else {
            panic!("{text:?}: {reading:?}");
        };
        let told = format!("line {line}: {note}");
        let diagnostics = analyze(text).diagnostics;
        assert!(
            matches!(diagnostics.as_slice(), [d] if d.severity == ConfigDiagnosticSeverity::Error
                && d.line == *line
                && &d.message == note),
            "{text:?}: {diagnostics:?}"
        );
        assert_eq!(
            crate::diagnostics::validate_config_text(text),
            Ok(vec![told.clone()]),
            "{text:?}"
        );
        let keys = key_warnings(text);
        assert!(
            keys.ignored.is_empty() && keys.retired.is_empty(),
            "{text:?}: {keys:?}"
        );
        assert_eq!(unaccepted_value_warnings(text, &[]), [told], "{text:?}");
        let mut warns = ConfigWarnings::default();
        crate::app_config::collect_key_notices(&mut warns, text);
        let unaccepted = crate::app_config::unaccepted_value_notices(text, &warns.told());
        warns.extend(ConfigFamily::UnacceptedValues, unaccepted);
        let rows = warns
            .into_messages()
            .into_iter()
            .map(|m| m.key)
            .collect::<Vec<_>>();
        assert_eq!(
            rows,
            [Some(ConfigFamily::UnacceptedValues.key().to_string())],
            "{text:?}"
        );
    }

    let text = "[a.disk.b.disk]\napply = false\n";
    assert!(reader(text).notes.is_empty(), "{:?}", reader(text));
    let keys = key_warnings(text);
    assert!(
        matches!(keys.ignored.as_slice(), [l] if l.contains("a.disk.b.disk.apply")),
        "{keys:?}"
    );
    assert!(unaccepted_value_warnings(text, &[]).is_empty());
    let text = "[harness]\ndisk.warn_free_gib = 5\n";
    let reading = reader(text);
    assert_eq!(
        reading.config,
        aterm_agent::harness::disk::Config::default()
    );
    let diagnostics = analyze(text).diagnostics;
    assert!(
        !diagnostics.is_empty()
            && diagnostics
                .iter()
                .all(|d| d.severity == ConfigDiagnosticSeverity::Warning
                    && reading.notes.contains(&d.message)),
        "{diagnostics:?} vs {:?}",
        reading.notes
    );
    let said =
        aterm_agent::harness::disk::Config::negative_reading("target_stale_days").expect("a floor");
    let whole = analyze("[disk]\ntarget_stale_days = -1\n").diagnostics;
    assert!(
        whole
            .iter()
            .any(|d| d.message.ends_with(&said) && d.severity == ConfigDiagnosticSeverity::Warning),
        "{whole:?}"
    );
    assert!(
        !whole
            .iter()
            .any(|d| d.severity == ConfigDiagnosticSeverity::Error),
        "{whole:?}"
    );
}

#[test]
fn no_walker_calls_a_table_that_holds_a_disk_off_switch_one_to_delete() {
    let record =
        |name: &str| format!("[[sparkle_words.custom]]\nwords = [\"{name}\"]\nink = \"rainbow\"\n");
    let cases: Vec<(String, usize, (usize, &str))> = vec![
        (
            "[harness.approve_all]\nbogus = 1\ndisk.apply = false\n".into(),
            3,
            (2, "harness.approve_all.bogus"),
        ),
        (
            "[[foo]]\nbogus = 1\ndisk.apply = false\n".into(),
            3,
            (2, "foo.bogus"),
        ),
        (
            "[harness.approve_all.x]\nbogus = 1\ndisk.apply = false\n".into(),
            3,
            (2, "harness.approve_all.x.bogus"),
        ),
        (
            format!("{}bogus = 1\ndisk.apply = false\n", record("box")),
            5,
            (4, "sparkle_words.custom.bogus"),
        ),
        (
            format!(
                "{}bogus = 1\n{}disk.apply = false\n",
                record("a"),
                record("b")
            ),
            8,
            (4, "sparkle_words.custom.bogus"),
        ),
        (
            format!(
                "{}[sparkle_words.custom.extra]\nbogus = 1\ndisk.apply = false\n",
                record("box")
            ),
            6,
            (5, "sparkle_words.custom.extra.bogus"),
        ),
    ];
    for (text, line, (told_line, told_key)) in &cases {
        let [fault] = aterm_agent::harness::cli::disk_faults(text)
            .try_into()
            .unwrap_or_else(|faults: Vec<_>| panic!("{text:?}: {faults:?}"));
        assert!(fault.removal_off, "{text:?}: {fault:?}");
        let reader_line = format!("line {line}: {}", fault.note);
        let lines = ignored_key_warnings(text);
        assert_eq!(
            lines.iter().filter(|l| **l == reader_line).count(),
            1,
            "{text:?}: {lines:?}"
        );
        let told = format!("line {told_line}: {told_key} ");
        assert!(
            matches!(
                lines
                    .iter()
                    .filter(|l| **l != reader_line)
                    .collect::<Vec<_>>()
                    .as_slice(),
                [l] if l.starts_with(&told)
            ),
            "{text:?}: {lines:?}"
        );
        assert_eq!(
            unaccepted_value_warnings(text, &[]),
            std::slice::from_ref(&reader_line)
        );
        let diagnostics = analyze(text).diagnostics;
        assert_eq!(diagnostics.len(), lines.len(), "{text:?}: {diagnostics:?}");
        assert!(
            diagnostics
                .iter()
                .all(|d| (d.severity == ConfigDiagnosticSeverity::Error)
                    == (d.message == fault.note)),
            "{text:?}: {diagnostics:?}"
        );
    }
}

#[test]
fn a_known_keys_value_error_is_never_a_key_with_no_effect() {
    let custom = "[[sparkle_words.custom]]\nwords = [\"one\"]\n";
    for (text, retired) in [
        (
            "windw_padding = 20\n[packages]\nauto_update = \"yes\"\n".to_string(),
            true,
        ),
        (
            format!("windw_padding = 20\n{custom}burst = {{ kind = \"blorp\" }}\n"),
            false,
        ),
        (
            format!("windw_padding = 20\n{custom}burst = {{ chance = 500 }}\n"),
            false,
        ),
    ] {
        let said = key_analysis(&text)
            .expect("the text parses")
            .unaccepted_values
            .iter()
            .map(key_warning_line)
            .collect::<Vec<_>>();
        assert_eq!(said.len(), 1, "{text:?}: {said:?}");
        let keys = key_warnings(&text);
        assert!(
            matches!(keys.ignored.as_slice(), [l] if l.starts_with("line 1: windw_padding")),
            "{text:?}: {keys:?}"
        );
        assert_eq!(
            keys.retired.len(),
            usize::from(retired),
            "{text:?}: {keys:?}"
        );
        let unaccepted = unaccepted_value_warnings(&text, &[]);
        for line in &said {
            assert!(
                !keys.ignored.contains(line) && !keys.retired.contains(line),
                "{text:?}: {keys:?}"
            );
            assert_eq!(
                unaccepted.iter().filter(|l| *l == line).count(),
                1,
                "{text:?}: {unaccepted:?}"
            );
        }
    }
}

#[test]
fn a_retired_harness_key_is_told_the_limit_it_still_is() {
    for key in aterm_agent::supervise::config::RETIRED_KEYS {
        for value in [false, true] {
            let warned = key_warnings(&format!("[harness]\n{key} = {value}\n"));
            assert!(warned.ignored.is_empty(), "{key}: {:?}", warned.ignored);
            let said = aterm_agent::supervise::config::retired_key_note(key, Some(value))
                .expect("a retired key");
            assert!(
                warned.retired.iter().any(|l| l.contains(&said)),
                "{key} = {value}: {:?}",
                warned.retired
            );
        }
    }
    let typo = key_warnings("[harness]\napprove_al = false\n");
    assert!(typo.retired.is_empty(), "{:?}", typo.retired);
    assert!(
        typo.ignored.iter().any(|l| l.contains("approve_al")),
        "{:?}",
        typo.ignored
    );
}
