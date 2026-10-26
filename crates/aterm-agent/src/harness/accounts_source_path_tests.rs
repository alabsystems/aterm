// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::login_tests::{Desk, NOW};
use super::*;
use aterm_spec::interp::State;
use std::sync::{Arc, Mutex};

#[test]
fn source_path_uses_the_captured_session_directory() {
    let desk = Desk::new("m27");
    let cwd = desk.home.join("project");
    fs::create_dir(&cwd).unwrap();
    fs::write(cwd.join("source.toml"), "captured source").unwrap();
    fs::write(desk.home.join("source.toml"), "wrong home source").unwrap();
    let mut env = quiet_env(
        &desk.home,
        &desk.state,
        &desk.config.join("aterm.toml"),
        NOW,
    );
    env.cwd = cwd;
    let result = fetch_source_text(&env, "source.toml");
    assert_eq!(result.as_deref(), Ok("captured source"));
}

#[test]
fn source_path_resolves_configured_files_beside_the_config() {
    let desk = Desk::new("m27");
    desk.default_login("owner@example.test");
    fs::write(
        desk.config.join("source.toml"),
        "[[account]]\nlabel='m27'\nmachines=['m27']\nemail='owner@example.test'\n",
    )
    .unwrap();
    fs::write(desk.home.join("source.toml"), "wrong home source").unwrap();
    fs::write(
        desk.config.join("aterm.toml"),
        "[fleet]\nmachine='m27'\naccounts_source='source.toml'\n",
    )
    .unwrap();
    let result = sync_accounts_for_init(&desk.home, &desk.state, &desk.roster(), NOW);
    assert!(
        result.is_ok(),
        "configured source must be read beside config: {result:?}"
    );
    assert_eq!(
        desk.rows().rows[0].email.as_deref(),
        Some("owner@example.test")
    );
}

fn document(label: &str, cli: Cli) -> String {
    let region = if cli == Cli::Kimi {
        "region='global'\n"
    } else {
        ""
    };
    format!(
        "[[account]]\nlabel='{label}'\nmachines=['m27']\nemail='{label}@example.test'\n{region}"
    )
}

fn sync_source(cli: Cli, env: &Env, source: Option<&str>) -> (ExitCode, String) {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = if cli == Cli::Claude {
        run_accounts_sync(env, source, None, false, &mut out, &mut err)
    } else {
        run_cli_accounts_sync(cli, env, (source, None, false), &mut out, &mut err)
    };
    (code, String::from_utf8(err).unwrap())
}

#[test]
fn source_path_explicit_cli_sync_uses_the_session_for_every_provider() {
    for cli in [Cli::Claude, Cli::Codex, Cli::Grok, Cli::Kimi] {
        let desk = Desk::new("m27");
        let cwd = desk.home.join("project");
        fs::create_dir(&cwd).unwrap();
        fs::write(cwd.join("source.toml"), document("chosen", cli)).unwrap();
        fs::write(desk.home.join("source.toml"), document("wrong-home", cli)).unwrap();
        fs::write(
            desk.config.join("source.toml"),
            document("wrong-config", cli),
        )
        .unwrap();
        let mut env = quiet_env(
            &desk.home,
            &desk.state,
            &desk.config.join("aterm.toml"),
            NOW,
        );
        env.cwd = cwd;
        let (code, err) = sync_source(cli, &env, Some("source.toml"));
        assert_eq!(code, ExitCode::SUCCESS, "{cli:?}: {err}");
        let roster = Roster::load_for(cli, &env.accounts_roster_for(cli).unwrap()).unwrap();
        assert!(roster.row("chosen").is_some(), "{cli:?}");
        assert!(roster.row("wrong-home").is_none() && roster.row("wrong-config").is_none());

        let store = accounts::Store::at_for(&env.state, cli, &desk.home);
        store
            .transact(NOW, &mut None, |state| {
                Ok(accounts::note_wall(
                    state,
                    "chosen",
                    "seven_day",
                    Some(NOW + 86_400),
                    NOW,
                    NOW,
                ))
            })
            .unwrap();
        let paths = [
            env.accounts_roster_for(cli).unwrap(),
            store.state_file(),
            store.state_witness_file(),
            store.logins_file(),
            store.logins_witness_file(),
            store.ledger_path(),
            store.wake_path(),
        ];
        let before: Vec<_> = paths.iter().map(|path| fs::read(path).ok()).collect();
        assert!(before[0].is_some());
        assert!(before[1].is_some(), "fixture records a budget wall");
        env.cwd = PathBuf::from("unresolved");
        let (code, err) = sync_source(cli, &env, Some("source.toml"));
        assert_eq!(code, ExitCode::FAILURE, "{cli:?}: {err}");
        assert!(err.contains("captured absolute directory"), "{err}");
        let after: Vec<_> = paths.iter().map(|path| fs::read(path).ok()).collect();
        assert_eq!(
            before, after,
            "{cli:?}: rejected source retains account records"
        );
    }
}

#[test]
fn source_path_config_reader_and_refresh_probe_use_the_same_directory() {
    let desk = Desk::new("m27");
    let source = desk.config.join("source.toml");
    fs::write(&source, document("first", Cli::Claude)).unwrap();
    fs::write(
        desk.home.join("source.toml"),
        document("decoy", Cli::Claude),
    )
    .unwrap();
    fs::write(
        desk.config.join("aterm.toml"),
        "[fleet]\nmachine='m27'\naccounts_source='source.toml'\n",
    )
    .unwrap();
    sync_accounts_for_init(&desk.home, &desk.state, &desk.roster(), NOW).unwrap();
    let spawn = |_work: Box<dyn FnOnce() + Send>| Ok(());
    let (cooldown, rediscovery) = (Mutex::new(None), Mutex::new(Rediscovery::default()));
    let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
    let fx = super::login_tests::upkeep_effects(
        &fetch_source_text,
        &spawn,
        &cooldown,
        &rediscovery,
        &guard,
    );
    let pass = || resync_with(&desk.home, &desk.state, (&desk.roster(), NOW, false), &fx);
    let first = pass();
    assert!(first.synced && first.failed.is_none(), "{first:?}");
    assert!(!pass().synced);
    fs::write(
        &source,
        format!(
            "{}{}",
            document("first", Cli::Claude),
            document("second", Cli::Claude)
        ),
    )
    .unwrap();
    let edited = pass();
    assert!(edited.synced && edited.failed.is_none(), "{edited:?}");
    assert!(desk.rows().row("second").is_some());
    assert!(desk.rows().row("decoy").is_none());
}

#[test]
fn source_path_relative_config_is_resolved_before_reading_or_publishing() {
    let desk = Desk::new("m27");
    fs::write(
        desk.config.join("source.toml"),
        document("chosen", Cli::Claude),
    )
    .unwrap();
    fs::write(
        desk.config.join("aterm.toml"),
        "[fleet]\nmachine='m27'\naccounts_source='source.toml'\n",
    )
    .unwrap();
    let mut env = quiet_env(
        &desk.home,
        &desk.state,
        &desk.config.join("aterm.toml"),
        NOW,
    );
    for (cwd, config) in [
        (desk.config.parent().unwrap(), "config/aterm.toml"),
        (desk.config.as_path(), "aterm.toml"),
    ] {
        env.cwd = cwd.to_path_buf();
        env.config = Some(PathBuf::from(config));
        let (code, err) = sync_source(Cli::Claude, &env, None);
        assert_eq!(code, ExitCode::SUCCESS, "{err}");
    }
    assert!(desk.rows().row("chosen").is_some());
}

#[test]
fn source_path_refuses_config_without_a_file_before_publication() {
    let desk = Desk::new("m27");
    let source = desk.config.join("source.toml");
    fs::write(&source, document("chosen", Cli::Claude)).unwrap();
    let canary = desk.config.parent().unwrap().join("accounts.toml");
    fs::write(&canary, "owner fixture kept").unwrap();
    let mut env = quiet_env(
        &desk.home,
        &desk.state,
        &desk.config.join("aterm.toml"),
        NOW,
    );
    env.cwd = desk.config.clone();
    for config in ["", ".", ".."] {
        env.config = Some(PathBuf::from(config));
        let (code, err) = sync_source(Cli::Claude, &env, source.to_str());
        assert_eq!(code, ExitCode::FAILURE, "{config}: {err}");
        assert!(err.contains("config path must name a file"), "{err}");
        assert_eq!(fs::read_to_string(&canary).unwrap(), "owner fixture kept");
        assert!(!desk.state.exists());
        assert!(!desk.roster().exists());
    }
}

#[test]
fn source_path_keeps_url_and_filesystem_semantics() {
    let desk = Desk::new("m27");
    let mut env = quiet_env(
        &desk.home,
        &desk.state,
        &desk.config.join("aterm.toml"),
        NOW,
    );
    env.cwd = PathBuf::from("unknown-relative-directory");
    assert!(matches!(
        fetch_plan(&env, "https://example.test/source.csv"),
        Ok(FetchPlan::Http(_))
    ));
    assert!(fetch_plan(&env, "rclone:private:source.csv").is_err());
    assert!(fetch_plan(&env, "source.csv").is_err());
    assert!(fetch_plan(&env, desk.config.join("source.csv").to_str().unwrap()).is_ok());
    env.home = Some(PathBuf::from("unknown-home"));
    assert!(fetch_plan(&env, "~/source.csv").is_err());
    env.home = None;
    assert!(fetch_plan(&env, "~/source.csv").is_err());
    env.home = Some(desk.home.clone());
    fs::write(desk.home.join("source.csv"), "home-source").unwrap();
    assert_eq!(
        fetch_source_text(&env, "~//source.csv").as_deref(),
        Ok("home-source")
    );
    #[cfg(windows)]
    {
        env.cwd = PathBuf::from(r"C:\captured");
        env.home = Some(PathBuf::from(r"C:\captured-home"));
        assert!(fetch_plan(&env, r"C:source.csv").is_err());
        assert!(fetch_plan(&env, r"~/C:source.csv").is_err());
        assert!(fetch_plan(&env, r"~/C:\source.csv").is_err());
        assert_eq!(
            fetch_plan(&env, r"\source.csv"),
            Ok(FetchPlan::File(PathBuf::from(r"C:\source.csv")))
        );
    }
    #[cfg(unix)]
    {
        let target = desk.home.join("target/child");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.parent().unwrap().join("source.csv"), "through-link").unwrap();
        std::os::unix::fs::symlink(&target, desk.config.join("linked")).unwrap();
        env.cwd = desk.config.clone();
        assert_eq!(
            fetch_source_text(&env, "linked/../source.csv").as_deref(),
            Ok("through-link")
        );
        fs::write(desk.config.join("remote:source.csv"), "local-colon").unwrap();
        assert_eq!(
            fetch_source_text(&env, "remote:source.csv").as_deref(),
            Ok("local-colon")
        );
        for source in ["", ".", ".."] {
            assert!(
                fetch_source_text(&env, source).is_err(),
                "a directory is refused: {source}"
            );
        }
    }
}

#[test]
fn source_path_configured_home_and_absolute_paths_keep_their_base() {
    let desk = Desk::new("m27");
    let absolute = desk.home.join("absolute.toml");
    fs::write(&absolute, document("absolute", Cli::Claude)).unwrap();
    fs::write(desk.home.join("source.toml"), document("home", Cli::Claude)).unwrap();
    fs::write(
        desk.config.join("source.toml"),
        document("decoy", Cli::Claude),
    )
    .unwrap();
    for (source, label) in [
        ("~/source.toml", "home"),
        (absolute.to_str().unwrap(), "absolute"),
    ] {
        fs::write(
            desk.config.join("aterm.toml"),
            format!(
                "[fleet]\naccounts_source={}\n",
                aterm_toml::Value::from(source)
            ),
        )
        .unwrap();
        let mut env = quiet_env(
            &desk.home,
            &desk.state,
            &desk.config.join("aterm.toml"),
            NOW,
        );
        env.cwd = PathBuf::from("unresolved");
        let fleet = load_fleet_source(&env, None).unwrap();
        assert_eq!(fleet.accounts[0].label, label);
    }
}

pub(crate) fn path_transition(state: &State, action: &str) -> State {
    let desk = Desk::new("m27");
    let project = desk.home.join("project");
    let absolute = desk.home.join("absolute");
    fs::create_dir(&project).unwrap();
    fs::create_dir(&absolute).unwrap();
    let paths = [&project, &desk.config, &absolute, &desk.home].map(|dir| dir.join("source.toml"));
    let texts: Vec<_> = (1..=4)
        .map(|i| document(&format!("source-{i}"), Cli::Claude))
        .collect();
    for (path, text) in paths.iter().zip(&texts) {
        fs::write(path, text).unwrap();
    }
    let before: Vec<_> = paths.iter().map(|path| fs::read(path).unwrap()).collect();
    let mut env = quiet_env(
        &desk.home,
        &desk.state,
        &desk.config.join("aterm.toml"),
        NOW,
    );
    env.cwd = project;
    let (case, result) = match action {
        "Explicit" => (1, fetch_source_text(&env, "source.toml")),
        "Configured" => {
            fs::write(
                desk.config.join("aterm.toml"),
                "[fleet]\naccounts_source='source.toml'\n",
            )
            .unwrap();
            (
                2,
                load_fleet_source_with(&env, None, &fetch_source_text).map(|(fleet, _)| {
                    texts
                        .iter()
                        .find(|text| text.contains(&format!("label='{}'", fleet.accounts[0].label)))
                        .unwrap()
                        .clone()
                }),
            )
        }
        "Absolute" => {
            env.cwd = PathBuf::from("relative");
            (3, fetch_source_text(&env, paths[2].to_str().unwrap()))
        }
        "Home" => {
            let plain = fetch_source_text(&env, "~/source.toml");
            assert_eq!(fetch_source_text(&env, "~///source.toml"), plain);
            (4, plain)
        }
        "NoBase" => {
            env.cwd = PathBuf::from("relative");
            (5, fetch_source_text(&env, "source.toml"))
        }
        "NoHome" => {
            env.home = None;
            (6, fetch_source_text(&env, "~/source.toml"))
        }
        "NoConfigFile" => {
            env.config = Some(PathBuf::new());
            (
                7,
                load_fleet_source_with(&env, paths[2].to_str(), &fetch_source_text)
                    .map(|_| texts[2].clone()),
            )
        }
        _ => panic!("unknown source path action"),
    };
    let selected = result.ok().map_or(0, |text| {
        texts
            .iter()
            .position(|candidate| candidate == &text)
            .unwrap() as i64
            + 1
    });
    let after: Vec<_> = paths.iter().map(|path| fs::read(path).unwrap()).collect();
    let mut next = state.clone();
    next.insert("case", case);
    next.insert("selected", selected);
    next.insert("kept", i64::from(before == after));
    next
}

#[test]
fn source_path_context_and_refusal_conform() {
    let model = aterm_models_harness::account_source_path_model();
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    let initial = model.init_state();
    for action in &model.actions {
        let next = path_transition(&initial, action.name);
        assert!(
            model.successors(action.name, &initial).contains(&next),
            "{}: {next:?}",
            action.name
        );
        let mut wrong = next.clone();
        wrong.insert("selected", if next["selected"] == 0 { 1 } else { 0 });
        assert!(!model.successors(action.name, &initial).contains(&wrong));
        assert!(!model.check_invariant(
            if next["case"] > 4 {
                "UnknownRefused"
            } else {
                "ContextSelects"
            },
            &wrong
        ));
    }
    let mut wrong = path_transition(&initial, "NoBase");
    wrong.insert("kept", 0);
    assert!(!model.check_invariant("InputsRetained", &wrong));
}
