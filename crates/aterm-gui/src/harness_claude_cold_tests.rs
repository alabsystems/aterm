// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn external_seed(c: &ColdCase) -> aterm_agent::harness::native_act::RestartCommand {
    use aterm_agent::harness::native_act::{Image, RestartCommand};
    RestartCommand {
        settles: None,
        account_intent: None,
        image: Image {
            path: b"/synthetic/claude".to_vec(),
            device: 1,
            inode: 1,
        },
        program: "/synthetic/claude".into(),
        argv: Vec::new(),
        env: Vec::new(),
        unset: None,
        cwd: Some(c.home.to_str().unwrap().into()),
        heal: None,
        dialect: aterm_agent::harness::upgrade::Dialect::Zsh,
    }
}

fn external_intent() -> aterm_agent::harness::native_act::ColdIntent {
    use aterm_agent::harness::native_act::{ColdBilling, ColdIntent};
    ColdIntent {
        login: None,
        home: None,
        auth_observed: false,
        route: aterm_agent::harness::accounts::AuthRoute::Outside,
        conversation: None,
        profile: None,
        mode: accounts::Mode::Wall,
        billing: ColdBilling::Keep,
    }
}

fn external_facts(
    c: &ColdCase,
    intent: &aterm_agent::harness::native_act::ColdIntent,
    extra: &[String],
) -> aterm_agent::harness::native_act::ColdEnvironmentFacts {
    let mut env = vec![format!("HOME={}", c.home.display())];
    env.extend_from_slice(extra);
    aterm_agent::harness::native_act::ColdEnvironmentFacts::observe_for_command(
        &env,
        intent,
        &external_seed(c),
        &c.home,
    )
    .unwrap()
}

fn missing_settings(
    _: &Path,
    _: Option<&str>,
    _: &[String],
    _: &str,
) -> Vec<(String, aterm_agent::harness::netwatch::Source)> {
    Vec::new()
}

#[test]
fn owned_helper_external_routes_use_actual_config_and_canonical_directory_guards() {
    let c = cold_case(0, None);
    let seed = external_seed(&c);
    let mut intent = external_intent();
    let custom = c.a.tmp.path().join("unmanaged-config");
    std::fs::create_dir(&custom).unwrap();
    intent.home = Some(custom.to_str().unwrap().into());
    intent.auth_observed = true;
    intent.route = aterm_agent::harness::accounts::AuthRoute::Stored;
    let facts = external_facts(&c, &intent, &[]);
    assert_eq!(facts.config.as_deref(), custom.to_str());
    assert_ne!(facts.profile_configs.as_deref(), facts.config.as_deref());
    let derive =
        |facts: &_, command: &_| cold_effective_context_using(facts, command, missing_settings);
    let (resolved, context, pooled) =
        cold_resolve_at(&c.seat, &seed, &intent, &facts, &derive).unwrap();
    assert!(!pooled);
    assert!(matches!(
        resolved.account_intent.as_deref(),
        Some(aterm_agent::harness::accounts_pool::ExecutionIntent::External { .. })
    ));
    assert!(
        cold_guard_at(&c.seat, &resolved, &context)
            .unwrap()
            .is_some()
    );
    assert_eq!(
        resolved.env, seed.env,
        "external resolution selects no pooled login"
    );

    let mut forbidden = intent.clone();
    forbidden.home = Some(c.original_dir.clone());
    let facts = external_facts(&c, &forbidden, &[]);
    assert!(
        cold_resolve_at(&c.seat, &seed, &forbidden, &facts, &derive).is_err(),
        "a managed login is not an unowned custom config"
    );
    #[cfg(unix)]
    {
        let alias = c.a.tmp.path().join("managed-alias");
        std::os::unix::fs::symlink(&c.original_dir, &alias).unwrap();
        forbidden.home = Some(alias.to_str().unwrap().into());
        let facts = external_facts(&c, &forbidden, &[]);
        assert!(cold_resolve_at(&c.seat, &seed, &forbidden, &facts, &derive).is_err());
    }
}

#[test]
fn owned_helper_provider_and_settings_routes_share_the_execute_interpretation() {
    use aterm_agent::harness::netwatch::Source;
    let c = cold_case(0, None);
    let seed = external_seed(&c);
    let intent = external_intent();
    for source in [
        Source::Missing,
        Source::Text(r#"{"env":{"ANTHROPIC_BASE_URL":"https://synthetic.invalid"}}"#.into()),
    ] {
        let extra = if source == Source::Missing {
            vec!["ANTHROPIC_API_KEY=synthetic-not-a-credential".into()]
        } else {
            Vec::new()
        };
        let facts = external_facts(&c, &intent, &extra);
        let derive = |facts: &_, command: &_| {
            cold_effective_context_using(facts, command, |_, _, _, _| {
                vec![("synthetic-settings".into(), source.clone())]
            })
        };
        let (resolved, context, pooled) =
            cold_resolve_at(&c.seat, &seed, &intent, &facts, &derive).unwrap();
        assert!(!pooled && !context.who.route.is_stored());
        assert!(
            cold_guard_at(&c.seat, &resolved, &context)
                .unwrap()
                .is_none()
        );
        assert!(resolved.account_intent.as_deref().unwrap().matches_process(
            &context.who,
            true,
            None
        ));
        if extra.is_empty() {
            assert!(
                facts.process_login().route.is_stored(),
                "settings-only route would fail a raw-facts Execute check"
            );
        }
        assert!(
            !facts
                .encode()
                .unwrap()
                .contains("synthetic-not-a-credential")
        );
        let proof = ColdCommandProof {
            context: c.seat.context(),
            intent: intent.clone(),
            facts: facts.clone(),
            effective: context.clone(),
            seed: seed.clone(),
            resolved: resolved.clone(),
            pooled,
        };
        assert!(proof.current_at(&c.seat, &derive));
        let changed = cold_effective_context_using(&facts, &resolved, |_, _, _, _| {
            vec![("synthetic-settings".into(), Source::Text("{}".into()))]
        })
        .unwrap();
        assert!(
            changed != context,
            "metadata changes invalidate the retained context"
        );
        assert!(
            !proof.current_at(&c.seat, &|facts, command| cold_effective_context_using(
                facts,
                command,
                |_, _, _, _| vec![("synthetic-settings".into(), Source::Text("{}".into()))]
            ))
        );
    }
    for source in [Source::Unreadable, Source::Text("{".into())] {
        let facts = external_facts(&c, &intent, &[]);
        assert!(
            cold_effective_context_using(&facts, &seed, |_, _, _, _| vec![(
                "unavailable".into(),
                source.clone()
            )])
            .is_err()
        );
    }
}

#[test]
fn owned_helper_profile_availability_and_billing_are_not_external_fallbacks() {
    use aterm_agent::harness::native_act::ColdBilling;
    let c = cold_case(0, None);
    let mut seed = external_seed(&c);
    let mut intent = external_intent();
    let discovery = external_facts(&c, &intent, &[]);
    let configs = PathBuf::from(discovery.profile_configs.as_deref().unwrap());
    std::fs::create_dir_all(&configs).unwrap();
    std::fs::write(configs.join("active.json"), "{}").unwrap();
    let derive =
        |facts: &_, command: &_| cold_effective_context_using(facts, command, missing_settings);
    assert!(
        cold_resolve_at(&c.seat, &seed, &intent, &discovery, &derive).is_err(),
        "a discovered JSON filename alone supplies no external namespace proof"
    );
    let facts = external_facts(
        &c,
        &intent,
        &["ANTHROPIC_PROFILE=synthetic-explicit-profile".into()],
    );
    let (resolved, context, pooled) =
        cold_resolve_at(&c.seat, &seed, &intent, &facts, &derive).unwrap();
    assert!(!context.who.route.is_stored());
    let proof = ColdCommandProof {
        context: c.seat.context(),
        intent: intent.clone(),
        facts: facts.clone(),
        effective: context.clone(),
        seed: seed.clone(),
        resolved: resolved.clone(),
        pooled,
    };
    assert!(proof.current_at(&c.seat, &derive));
    std::fs::remove_file(configs.join("active.json")).unwrap();
    let changed = derive(&facts, &resolved).unwrap();
    assert!(changed != context && !changed.who.route.is_stored());
    assert!(!proof.current_at(&c.seat, &derive));
    std::fs::remove_dir(&configs).unwrap();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(c.a.tmp.path().join("missing-profiles"), &configs).unwrap();
        assert!(
            derive(&facts, &seed).is_err(),
            "a dangling store is unavailable, not proven absent"
        );
        std::fs::remove_file(&configs).unwrap();
    }
    let standard = derive(&facts, &seed).unwrap();
    intent.profile = Some(standard.profile.clone());
    intent.billing = ColdBilling::PreserveProfile;
    seed.argv = vec!["--model".into(), "synthetic-model".into()];
    let custom = external_facts(
        &c,
        &intent,
        &[
            "CLAUDE_CODE_CUSTOM_OAUTH_URL=https://synthetic.invalid".into(),
            "ANTHROPIC_API_KEY=synthetic-not-a-credential".into(),
        ],
    );
    assert!(
        cold_resolve_at(&c.seat, &seed, &intent, &custom, &derive).is_err(),
        "captured Standard is not the helper's custom profile"
    );
    intent.billing = ColdBilling::Keep;
    assert!(cold_resolve_at(&c.seat, &seed, &intent, &custom, &derive).is_ok());
}

#[test]
fn owned_helper_retained_managed_authority_rechecks_owner_and_generation() {
    use aterm_agent::harness::accounts_pool;
    for change in 0..2 {
        let c = cold_case(0, None);
        let mut seed = external_seed(&c);
        let mut intent = external_intent();
        intent.auth_observed = true;
        intent.route = aterm_agent::harness::accounts::AuthRoute::Stored;
        intent.login = Some(c.original_dir.clone());
        let facts = external_facts(&c, &intent, &[]);
        let derive =
            |facts: &_, command: &_| cold_effective_context_using(facts, command, missing_settings);
        let context = derive(&facts, &seed).unwrap();
        let roster_path = c.a.roster_path();
        let pool = accounts_pool::Seat {
            config_dir: roster_path.parent().unwrap(),
            state: &c.seat.state,
            home: &c.home,
            now: c.a.clock(),
        };
        seed.account_intent = Some(Box::new(
            accounts_pool::capture_execution(&pool, accounts::Cli::Claude, &context.who).unwrap(),
        ));
        let (resolved, context, pooled) =
            cold_resolve_at(&c.seat, &seed, &intent, &facts, &derive).unwrap();
        assert!(pooled);
        assert_eq!(
            resolved, seed,
            "retained authority is checked without another selection"
        );
        assert!(
            cold_guard_at(&c.seat, &resolved, &context)
                .unwrap()
                .is_some()
        );
        if change == 0 {
            let mut rows = c.a.roster().rows;
            rows[0].held_by = Some("synthetic-other-owner".into());
            std::fs::write(
                c.a.roster_path(),
                accounts::append_rows_for(accounts::Cli::Claude, "", &rows).unwrap(),
            )
            .unwrap();
        } else {
            aterm_uds::atomic_file::write(&Path::new(&c.original_dir).join(".claude.json"),
                br#"{"oauthAccount":{"accountUuid":"synthetic-generation","emailAddress":"synthetic@example.invalid"}}"#, 0o600).unwrap();
            let fresh =
                accounts_pool::capture_execution(&pool, accounts::Cli::Claude, &context.who)
                    .expect("the replacement metadata is valid fresh authority");
            assert_ne!(Some(&fresh), resolved.account_intent.as_deref());
        }
        assert!(cold_guard_at(&c.seat, &resolved, &context).is_err());
    }
}

#[test]
fn owned_helper_namespace_is_actual_and_unavailable_is_not_known_absence() {
    let c = cold_case(0, None);
    let seed = external_seed(&c);
    let intent = external_intent();
    let actual = c.a.tmp.path().join("another-home");
    std::fs::create_dir_all(actual.join(".claude")).unwrap();
    let facts = aterm_agent::harness::native_act::ColdEnvironmentFacts::observe_for_command(
        &[format!("HOME={}", actual.display())],
        &intent,
        &seed,
        &c.home,
    )
    .unwrap();
    let derive = |facts: &_, command: &_| {
        cold_effective_context_using(facts, command, |home, config, _, cwd| {
            assert_eq!(home, actual.as_path());
            assert!(config.is_none());
            assert_eq!(cwd, c.home.to_str().unwrap());
            Vec::new()
        })
    };
    let (resolved, context, pooled) =
        cold_resolve_at(&c.seat, &seed, &intent, &facts, &derive).unwrap();
    assert!(!pooled && context.home == actual);
    assert!(
        cold_guard_at(&c.seat, &resolved, &context)
            .unwrap()
            .is_some()
    );
    let mut empty = facts.clone();
    empty.home = Some(String::new());
    assert!(
        cold_effective_context_using(&empty, &seed, |_, _, _, _| panic!(
            "empty HOME must refuse before settings reads"
        ))
        .is_err()
    );
    let mut unknown = facts.clone();
    unknown.selection = aterm_agent::harness::profile::Selection::Unknown;
    assert!(
        cold_effective_context_using(&unknown, &seed, |_, _, _, _| panic!(
            "unknown observation must refuse before settings reads"
        ))
        .is_err()
    );
    #[cfg(unix)]
    {
        let link = c.a.tmp.path().join("missing-profile-root");
        std::os::unix::fs::symlink(c.a.tmp.path().join("no-target"), &link).unwrap();
        let mut dangling = facts.clone();
        dangling.profile_configs = Some(link.join("configs").to_str().unwrap().into());
        assert!(cold_effective_context_using(&dangling, &seed, missing_settings).is_err());
    }
}

#[test]
fn owned_helper_settings_directory_overrides_cannot_mint_external_authority() {
    use aterm_agent::harness::netwatch::Source;
    let c = cold_case(0, None);
    let seed = external_seed(&c);
    let intent = external_intent();
    let facts = external_facts(&c, &intent, &[]);
    let paths = vec![PathBuf::from(&c.original_dir)];
    #[cfg(unix)]
    let paths = {
        let mut paths = paths;
        let alias = c.a.tmp.path().join("settings-managed-alias");
        std::os::unix::fs::symlink(&c.original_dir, &alias).unwrap();
        paths.push(alias);
        paths
    };
    for path in paths {
        for key in [accounts::LOGIN_ENV, accounts::HOME_ENV] {
            let env = std::collections::BTreeMap::from([(key, path.to_str().unwrap())]);
            let text = aterm_json::to_string(&aterm_json::json!({"env": env})).unwrap();
            let derive = |facts: &_, command: &_| {
                cold_effective_context_using(facts, command, |_, _, _, _| {
                    vec![("settings-selector".into(), Source::Text(text.clone()))]
                })
            };
            assert!(
                cold_resolve_at(&c.seat, &seed, &intent, &facts, &derive).is_err(),
                "a conservative nonstored bit does not admit a managed directory override"
            );
            let mut external = seed.clone();
            external.account_intent = Some(Box::new(
                aterm_agent::harness::accounts_pool::ExecutionIntent::external(),
            ));
            assert!(
                derive(&facts, &external).is_err(),
                "Execute uses the same unavailable interpretation"
            );
        }
    }
}

#[test]
fn owned_helper_external_model_moves_require_explicit_current_credits() {
    use aterm_agent::harness::native_act::ColdBilling;
    if !crate::test_alone::alone() {
        return;
    }
    let c = cold_case(0, None);
    let mut seed = external_seed(&c);
    let mut intent = external_intent();
    seed.argv = vec!["--model".into(), "synthetic-model".into()];
    let facts = external_facts(
        &c,
        &intent,
        &["ANTHROPIC_API_KEY=synthetic-not-a-credential".into()],
    );
    let derive =
        |facts: &_, command: &_| cold_effective_context_using(facts, command, missing_settings);
    let context = derive(&facts, &seed).unwrap();
    assert_eq!(
        context.profile.selection,
        aterm_agent::harness::profile::Selection::Standard
    );
    intent.profile = Some(context.profile.clone());
    intent.billing = ColdBilling::PreserveProfile;
    assert!(
        cold_resolve_at(&c.seat, &seed, &intent, &facts, &derive).is_err(),
        "the Standard filename alone cannot prove external billing equivalence"
    );
    intent.billing = ColdBilling::Keep;
    assert!(cold_resolve_at(&c.seat, &seed, &intent, &facts, &derive).is_ok());
    intent.billing = ColdBilling::UsageCredits;
    assert!(cold_resolve_at(&c.seat, &seed, &intent, &facts, &derive).is_ok());
    // This body runs alone in its own child process (`test_alone::alone`), so the policy it
    // sets ends with that process and reaches no other test.
    LIVE_ACCOUNTS_MODE.store(
        ACCOUNTS_SUPERVISED | ACCOUNTS_WALL | ACCOUNTS_USAGE_CREDITS,
        Ordering::SeqCst,
    );
    assert!(cold_intent_current(&intent));
    LIVE_ACCOUNTS_MODE.store(ACCOUNTS_SUPERVISED | ACCOUNTS_WALL, Ordering::SeqCst);
    assert!(
        !cold_intent_current(&intent),
        "credits revoked after Resolve must refuse the current launch"
    );
    intent.billing = ColdBilling::Keep;
    assert!(
        cold_intent_current(&intent),
        "retaining the model is a different policy"
    );
}

#[test]
fn a_deferred_helper_relative_login_context_is_refused_before_local_policy_reads() {
    use aterm_agent::harness::native_act::{
        ColdEnvironmentFacts, ColdIntent, Image, RestartCommand,
    };

    let intent = ColdIntent {
        login: None,
        home: None,
        auth_observed: true,
        route: aterm_agent::harness::accounts::AuthRoute::Stored,
        conversation: None,
        profile: None,
        mode: accounts::Mode::Wall,
        billing: aterm_agent::harness::native_act::ColdBilling::Keep,
    };
    let seed = RestartCommand {
        settles: None,
        account_intent: None,
        image: Image {
            path: b"/private-fixture/claude".to_vec(),
            device: 1,
            inode: 1,
        },
        program: "/private-fixture/claude".into(),
        argv: Vec::new(),
        env: Vec::new(),
        unset: None,
        cwd: Some("/private-fixture".into()),
        heal: None,
        dialect: aterm_agent::harness::upgrade::Dialect::Zsh,
    };
    for field in 0..3 {
        let mut facts =
            ColdEnvironmentFacts::observe(&[], &intent, std::path::Path::new("/private-fixture"))
                .unwrap();
        match field {
            0 => facts.login = Some("relative-login".into()),
            1 => facts.home = Some("relative-home".into()),
            _ => facts.profile_configs = Some("relative-profiles".into()),
        }
        match resolve_cold_command(&seed, &intent, &facts) {
            Err(reason) => assert_eq!(
                reason,
                "cold Claude credential context contains a relative path"
            ),
            Ok(_) => panic!("relative helper context resolved through the GUI's directory"),
        }
    }
}

/// One `ClaudeColdAdmission` case's world: its accounts, its retained snapshot and its seat.
struct ColdCase {
    a: Arc<Accts>,
    home: PathBuf,
    mode: accounts::Mode,
    default: bool,
    original_dir: String,
    renewed: String,
    original: Snapshot,
    seat: AccountSeat,
}

/// What a platform shell's kernel record gives another process on macOS: its argument words
/// and no environment (`/bin/zsh`, measured on macOS 27.0.1).
const PLATFORM_SHELL: Option<Vec<String>> = Some(Vec::new());

/// Case `case`'s world; where `record`, its process's own record outlived it, naming that
/// conversation.
fn cold_case(case: i64, record: Option<&str>) -> ColdCase {
    let a = Accts::new();
    let home = a.tmp.path().join("home");
    let mode = if case == 11 || case == 12 {
        accounts::Mode::Off
    } else {
        accounts::Mode::Wall
    };
    crate::test_scratch::write_config(
        &a.tmp.path().join("config/aterm.toml"),
        format!("[harness]\naccounts='{}'\n", mode.as_str()),
    );
    let mut roster = a.roster();
    let default = case == 15 || case == 16;
    roster
        .rows
        .retain(|row| row.label == if default { "main" } else { "work-2" });
    if case == 2 || case == 11 {
        roster.rows[0].held_by = Some("another-machine".into());
    }
    std::fs::write(
        a.roster_path(),
        accounts::append_rows_for(accounts::Cli::Claude, "", &roster.rows).unwrap(),
    )
    .unwrap();
    a.store()
        .transact(a.clock(), &mut None, |state| {
            state.home_accounts.insert(roster.rows[0].label.clone());
            if case == 1 {
                state.prepared_accounts.insert("work-2".into());
            }
            Ok(Vec::new())
        })
        .unwrap();
    let original_dir = a.login_dir("work-2");
    let renewed = a.login_dir("work-2+1");
    if case == 4 {
        std::fs::create_dir_all(&renewed).unwrap();
        std::fs::write(Path::new(&renewed).join("a-login-file"), "{}").unwrap();
        a.store()
            .transact_logins(a.clock(), &mut None, |state, logins| {
                Ok(accounts::note_renewed(
                    state,
                    logins,
                    "work-2",
                    Some(&original_dir),
                    &renewed,
                    None,
                    a.clock(),
                ))
            })
            .unwrap();
    }
    if case == 5 {
        a.store()
            .transact_logins(a.clock(), &mut None, |_, logins| {
                logins.labels.insert(
                    "work-2".into(),
                    accounts::Binding {
                        dir: Some(original_dir.clone()),
                        ..Default::default()
                    },
                );
                Ok(Vec::new())
            })
            .unwrap();
        std::fs::write(a.roster_path(), "").unwrap();
    }
    if case == 3 {
        std::fs::write(a.store().logins_file(), "{").unwrap();
    }
    if case == 13 {
        std::fs::write(
            home.join(".claude/settings.json"),
            r#"{"apiKeyHelper":"echo fixture"}"#,
        )
        .unwrap();
    }
    if case == 17 {
        let profiles = a.tmp.path().join("shell-xdg/anthropic/configs");
        std::fs::create_dir_all(&profiles).unwrap();
        std::fs::write(profiles.join("selected.json"), "{}").unwrap();
    }
    if case == 15 {
        std::fs::write(
            home.join(".claude/projects/proj")
                .join(format!("{ACCT_TALK_A}.jsonl")),
            "{}\n",
        )
        .unwrap();
    }
    let legacy = case == 18 || case == 19;
    let mut original = Snapshot {
        auth_observed: case != 14 && case != 15 && !legacy,
        route: aterm_agent::harness::accounts::AuthRoute::Stored
            .unless(case == 8 || case == 14 || case == 15 || legacy),
        login: (!default && case != 14).then(|| original_dir.clone()),
        session: Some(ACCT_TALK_A.into()),
        cwd: a.tmp.path().display().to_string(),
        ..snap("s-claude-cold")
    };
    if case == 20 || case == 21 || case == 23 || case == 24 {
        // What the snapshot read of the agent's own exec environment: what its shell passed it.
        let at = if case == 23 {
            "/another/home".into()
        } else {
            home.display().to_string()
        };
        original.launch_env = accounts::LaunchEnv::of_exec_env(&[
            format!("HOME={at}"),
            "PATH=/usr/bin".into(),
            format!("{}={original_dir}", accounts::LOGIN_ENV),
        ]);
        assert!(original.launch_env.is_some(), "a stored login's record");
    }
    if case == 7 {
        original.home = Some("/custom/claude".into());
    }
    if case == 9 {
        original.profile = Some(aterm_agent::harness::profile::Context {
            plain: home.join(".claude.json"),
            records: home.join(".claude"),
            selection: aterm_agent::harness::profile::Selection::Standard,
            home: Some(home.display().to_string()),
            config: None,
            login: Some(original_dir.clone()),
        });
    }
    if let Some(session) = record {
        let sessions = home.join(".claude/sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        std::fs::write(
            sessions.join(format!("{}.json", original.pid)),
            format!(
                r#"{{"pid":{},"sessionId":"{session}","cwd":"{}","procStart":"{}","version":"2.1.317","kind":"interactive","entrypoint":"cli","status":"idle","statusUpdatedAt":1790000000000}}"#,
                original.pid, original.cwd, original.start
            ),
        )
        .unwrap();
    }
    let seat = AccountSeat {
        roster: Some(a.roster_path()),
        state: a.state_root(),
        home: home.clone(),
        now: a.clock(),
        ladder: a.ladder(),
    };
    ColdCase {
        a,
        home,
        mode,
        default,
        original_dir,
        renewed,
        original,
        seat,
    }
}

/// The environment case `case`'s shell gives a reader: none for 6 and 21, the platform shell's
/// empty record for 20, 22 and 23.
fn shell_env(c: &ColdCase, case: i64) -> Option<Vec<String>> {
    if case == 6 || case == 21 {
        return None;
    }
    if case == 20 || case == 22 || case == 23 {
        return PLATFORM_SHELL;
    }
    let mut env = vec![format!("HOME={}", c.home.display()), "PATH=/usr/bin".into()];
    if case == 10 || case == 19 || case == 24 {
        env.push(format!("{}=fixture", accounts::CREDENTIAL_KEYS[0]));
    }
    if case == 18 {
        env.push("HOME=/another/home".into());
    }
    if case == 16 {
        env.push(format!("{}={}", accounts::LOGIN_ENV, c.original_dir));
    }
    if case == 17 {
        env.push(format!(
            "XDG_CONFIG_HOME={}",
            c.a.tmp.path().join("shell-xdg").display()
        ));
    }
    Some(env)
}

fn decide(case: i64) -> [i64; 3] {
    let c = cold_case(case, None);
    let result = account_cold_at(&c.seat, &c.original, c.mode, || shell_env(&c, case));
    let mut binding = true;
    let mut profile = true;
    if let Ok(Some(chosen)) = &result {
        let expected = if c.default {
            ""
        } else if case == 4 {
            &c.renewed
        } else {
            &c.original_dir
        };
        binding = chosen.login.as_deref() == Some(expected)
            && chosen.home == c.original.home
            && chosen.auth_observed
            && chosen.route.is_stored();
        profile = chosen.profile == c.original.profile;
    }
    [
        i64::from(result.is_ok()),
        i64::from(binding),
        i64::from(profile),
    ]
}

pub(crate) fn admission_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    if action == "NextCase" {
        next.insert("case", state["case"] + 1);
    } else {
        let [admitted, binding, profile] = decide(state["case"]);
        next.insert("admitted", admitted);
        next.insert("binding", binding);
        next.insert("profile", profile);
        next.insert("phase", 1);
    }
    next
}

#[test]
fn claude_cold_admission_conforms_with_current_custody_and_captured_profiles() {
    let model = aterm_models_harness::claude_cold_admission_model();
    aterm_spec::interp::prove_and_catch(&model);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = admission_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
    let mut denied = model.init_state();
    denied.insert("case", 1);
    denied = admission_transition(&denied, "Decide");
    denied.insert("admitted", 1);
    assert!(!model.check_invariant("Admission", &denied));
    let mut inherited = model.init_state();
    inherited.insert("case", 16);
    inherited = admission_transition(&inherited, "Decide");
    inherited.insert("binding", 0);
    assert!(!model.check_invariant("Binding", &inherited));
    let mut recorded = model.init_state();
    recorded.insert("case", 20);
    recorded = admission_transition(&recorded, "Decide");
    assert_eq!(recorded["admitted"], 1, "{recorded:?}");
    recorded.insert("admitted", 0);
    assert!(!model.check_invariant("RecordedBase", &recorded));
}

/// A recorded launch environment keeps only what admission reads, so the snapshot and the
/// layout it is written to carry directories and selectors, not the rest of the environment,
/// and a process on a credential of its own records nothing.
#[test]
fn a_launch_record_keeps_only_what_admission_reads_of_a_stored_login() {
    let env = [
        "HOME=/Users//fixture".to_string(),
        "PATH=/usr/bin".into(),
        "GH_TOKEN=fixture-not-kept".into(),
        format!("{}=/fixture/login", accounts::LOGIN_ENV),
        "XDG_CONFIG_HOME=/Users//fixture/.config".into(),
    ];
    let recorded = accounts::LaunchEnv::of_exec_env(&env).expect("a stored login's record");
    assert_eq!(
        recorded.entries(),
        [
            "HOME=/Users//fixture".to_string(),
            format!("{}=/fixture/login", accounts::LOGIN_ENV),
            "XDG_CONFIG_HOME=/Users//fixture/.config".to_string(),
        ]
    );
    assert!(!format!("{recorded:?}").contains("/fixture/login"));
    assert_eq!(
        accounts::LaunchEnv::of_record(recorded.entries()).as_ref(),
        Some(&recorded)
    );
    let mut credential = env.to_vec();
    credential.push(format!("{}=fixture", accounts::CREDENTIAL_KEYS[0]));
    assert_eq!(accounts::LaunchEnv::of_exec_env(&credential), None);
    assert_eq!(accounts::LaunchEnv::of_record(&credential), None);
    // A credential set empty may be the agent's own command line blanking its shell's value,
    // which the relaunch line would not: no evidence either way.
    let mut blanked = env.to_vec();
    blanked.push(format!("{}=", accounts::CREDENTIAL_KEYS[0]));
    assert_eq!(accounts::LaunchEnv::of_exec_env(&blanked), None);
    assert_eq!(accounts::LaunchEnv::of_record(&blanked), None);
    assert_eq!(accounts::LaunchEnv::of_exec_env(&[]), None);
}

/// Every cold admission's answer, in order: the login it admitted, or why it held.
type Answers = Arc<Mutex<Vec<Result<Option<String>, String>>>>;

/// A host restoring case `case`'s retained snapshot through the real admission, its shell's
/// environment the platform shell's empty record ([`restore_through_a_platform_shell`]).
struct PlatformRestore {
    c: Arc<ColdCase>,
    host: HostHandle,
    /// Every admission's answer, in order.
    answers: Answers,
    /// The login of every launch it reached.
    launched: Arc<Mutex<Vec<Option<String>>>>,
}

fn restore_through_a_platform_shell(case: i64) -> PlatformRestore {
    let c = Arc::new(cold_case(case, None));
    let world = Arc::new(World::default());
    let answers = Arc::new(Mutex::new(Vec::new()));
    let launched = Arc::new(Mutex::new(Vec::new()));
    let mut h = hooks(&world, parking_body(&world));
    h.pause = Arc::new(|_| Duration::from_millis(1));
    let (said, seat) = (Arc::clone(&answers), c.seat.clone());
    h.acts.account_cold = Arc::new(move |snapshot, mode, _| {
        let answer = account_cold_at(&seat, snapshot, mode, || PLATFORM_SHELL);
        said.lock().unwrap().push(
            answer
                .as_ref()
                .map(|chosen| chosen.as_ref().and_then(|s| s.login.clone()))
                .map_err(Clone::clone),
        );
        answer.map(relaunch::ColdAdmission::immediate)
    });
    let recorded = Arc::clone(&launched);
    h.acts.relaunch_restored = Arc::new(move |_, snapshot, _, _, _| {
        recorded.lock().unwrap().push(snapshot.login.clone());
        "adopted".into()
    });
    let mut cfg = on();
    cfg.set("accounts", c.mode.as_str()).unwrap();
    let host = HostHandle::start(cfg, false, false, h);
    host.relaunch_restored(vec![RestoredAgent {
        sid: c.original.tab.clone(),
        snap: c.original.clone(),
        place: "test".into(),
        cold: false,
        asked: None,
        was: None,
    }]);
    PlatformRestore {
        c,
        host,
        answers,
        launched,
    }
}

/// THE OWNER'S RULE THAT A RESTART RESUMES ITS WORK, ON A PLATFORM SHELL: a stored-login Claude
/// whose tab shell is `/bin/zsh`, whose environment macOS shows no other process, relaunches on
/// the launch environment its own exec environment recorded, under the current binding.
#[test]
fn a_stored_login_relaunch_on_a_platform_shell_resumes_on_its_recorded_launch_environment() {
    let r = restore_through_a_platform_shell(20);
    until("the relaunch on the recorded launch environment", || {
        !r.launched.lock().unwrap().is_empty()
            || r.answers.lock().unwrap().iter().any(Result::is_err)
    });
    r.host.shutdown_and_join();
    assert_eq!(
        r.answers.lock().unwrap()[0],
        Ok(Some(r.c.original_dir.clone()))
    );
    assert_eq!(
        r.launched.lock().unwrap()[0].as_deref(),
        Some(r.c.original_dir.as_str())
    );
}

/// With neither the shell's environment nor a launch record (a legacy layout, or an agent
/// whose own environment was unreadable), the relaunch holds its conversation and launches
/// nothing, past the ordinary retry count.
#[test]
fn a_stored_login_relaunch_with_no_readable_environment_waits_and_launches_nothing() {
    let r = restore_through_a_platform_shell(22);
    assert!(r.c.original.launch_env.is_none());
    until("the account hold outlives ordinary retries", || {
        r.answers.lock().unwrap().len() > RESTORED_TRIES as usize + 1
    });
    r.host.shutdown_and_join();
    assert!(r.launched.lock().unwrap().is_empty());
    assert!(r.answers.lock().unwrap().iter().all(Result::is_err));
}

#[test]
fn claude_cold_refusal_keeps_the_conversation_and_never_falls_back_to_its_old_line() {
    let world = Arc::new(World::default());
    let allowed = Arc::new(AtomicBool::new(false));
    let observations = Arc::new(AtomicUsize::new(0));
    let launched = Arc::new(Mutex::new(Vec::new()));
    let mut h = hooks(&world, parking_body(&world));
    h.pause = Arc::new(|_| Duration::from_millis(1));
    let admitted = Arc::clone(&allowed);
    let seen = Arc::clone(&observations);
    h.acts.account_cold = Arc::new(move |original, _, _| {
        seen.fetch_add(1, Ordering::SeqCst);
        if !admitted.load(Ordering::SeqCst) {
            return Err("held".into());
        }
        Ok(relaunch::ColdAdmission::immediate(Some(Snapshot {
            login: Some("/admitted/login".into()),
            ..original.clone()
        })))
    });
    let recorded = Arc::clone(&launched);
    h.acts.relaunch_restored = Arc::new(move |_, snapshot, _, _, _| {
        recorded.lock().unwrap().push(snapshot.login.clone());
        "refused:line".into()
    });
    let host = HostHandle::start(on(), false, false, h);
    host.relaunch_restored(vec![RestoredAgent {
        sid: "s-claude-cold".into(),
        snap: snap("s-claude-cold"),
        place: "test".into(),
        cold: false,
        asked: None,
        was: None,
    }]);
    until("Claude account hold outlives ordinary retries", || {
        observations.load(Ordering::SeqCst) > RESTORED_TRIES as usize + 1
    });
    assert!(launched.lock().unwrap().is_empty());
    allowed.store(true, Ordering::SeqCst);
    until("Claude admitted line is attempted", || {
        !launched.lock().unwrap().is_empty()
    });
    host.shutdown_and_join();
    assert!(
        launched
            .lock()
            .unwrap()
            .iter()
            .all(|login| login.as_deref() == Some("/admitted/login"))
    );
}

#[test]
fn claude_cold_decision_is_rechecked_when_live_rotation_changes() {
    let world = Arc::new(World::default());
    let observations = Arc::new(AtomicUsize::new(0));
    let launched = Arc::new(AtomicUsize::new(0));
    let (entered, entry) = std::sync::mpsc::channel();
    let (release, released) = std::sync::mpsc::channel();
    let released = Mutex::new(released);
    let mut h = hooks(&world, parking_body(&world));
    h.pause = Arc::new(|_| Duration::from_millis(1));
    let seen = Arc::clone(&observations);
    h.acts.account_cold = Arc::new(move |snapshot, mode, _| {
        if seen.fetch_add(1, Ordering::SeqCst) == 0 {
            entered.send(()).unwrap();
            released
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(60))
                .unwrap();
            Ok(relaunch::ColdAdmission::immediate(Some(snapshot.clone())))
        } else {
            assert_eq!(mode, accounts::Mode::Off);
            Err("new controls require a new decision".into())
        }
    });
    let acted = Arc::clone(&launched);
    h.acts.relaunch_restored = Arc::new(move |_, _, _, _, _| {
        acted.fetch_add(1, Ordering::SeqCst);
        "adopted".into()
    });
    let host = HostHandle::start(on(), false, false, h);
    host.relaunch_restored(vec![restored("s-cold-mode", "test")]);
    entry.recv_timeout(Duration::from_secs(60)).unwrap();
    let mut off = on();
    off.set("accounts", "off").unwrap();
    host.set_config(off);
    release.send(()).unwrap();
    until("fresh Claude decision under the new controls", || {
        observations.load(Ordering::SeqCst) > 1
    });
    host.shutdown_and_join();
    assert_eq!(launched.load(Ordering::SeqCst), 0);
}

/// A cold relaunch admitted onto a row's own login notes its conversation's stretch there before
/// its process starts (`State::login_dir_spans`), so the transcript scan books none of the rows
/// it writes to the row its transcript names; a note that cannot be written refuses it
/// (`note_cold_stretch`, which `account_cold_at` returns through). The conversation noted is the
/// one the relaunch resumes: where its process's own record outlived it, the one that record
/// names, though the snapshot still names the one it left. CONTROL: one onto the default login
/// notes nothing.
#[test]
fn claude_cold_admission_onto_a_rows_own_login_notes_its_stretch_first() {
    let admitted = cold_case(0, None);
    let result = account_cold_at(&admitted.seat, &admitted.original, admitted.mode, || {
        shell_env(&admitted, 0)
    });
    let chosen = result.expect("admitted").expect("a line");
    assert_eq!(
        chosen.login.as_deref(),
        Some(admitted.original_dir.as_str())
    );
    let spans = admitted.a.state().login_dir_spans;
    assert!(
        spans
            .get(ACCT_TALK_A)
            .is_some_and(|noted| noted.covers(admitted.a.clock())),
        "{spans:?}"
    );

    const CLEARED: &str = "0badf00d-7777-4b61-9d52-7f1e2c3a4b5c";
    let moved_on = cold_case(0, Some(CLEARED));
    let result = account_cold_at(&moved_on.seat, &moved_on.original, moved_on.mode, || {
        shell_env(&moved_on, 0)
    });
    assert!(matches!(&result, Ok(Some(_))), "{result:?}");
    let spans = moved_on.a.state().login_dir_spans;
    assert!(
        spans
            .get(CLEARED)
            .is_some_and(|noted| noted.covers(moved_on.a.clock())),
        "the conversation its record names: {spans:?}"
    );
    assert!(!spans.contains_key(ACCT_TALK_A), "{spans:?}");

    // The note alone, the store's lock held past the seat's wait: refused, nothing noted.
    let a = Accts::new();
    let held = a.hold_lock();
    let seat = AccountSeat {
        roster: Some(a.roster_path()),
        state: a.state_root(),
        home: a.tmp.path().join("home"),
        now: a.clock(),
        ladder: a.ladder(),
    };
    let onto = Snapshot {
        login: Some(a.login_dir("work-2")),
        ..snap("s-claude-cold")
    };
    let refused = note_cold_stretch(&seat, &onto, &onto);
    assert!(
        matches!(&refused, Err(why) if why.contains("its conversation not noted")),
        "{refused:?}"
    );
    drop(held);
    assert!(a.state().login_dir_spans.is_empty());
    assert_eq!(note_cold_stretch(&seat, &onto, &onto), Ok(()));
    assert!(
        a.state().login_dir_spans.contains_key(ACCT_TALK_A),
        "control: written once free"
    );

    let default = cold_case(15, None);
    let result = account_cold_at(&default.seat, &default.original, default.mode, || {
        shell_env(&default, 15)
    });
    assert!(
        matches!(&result, Ok(Some(chosen)) if chosen.login.as_deref() == Some("")),
        "{result:?}"
    );
    assert!(default.a.state().login_dir_spans.is_empty(), "CONTROL");
}

/// RS1, THE OWNER'S RULE THAT MACHINES COME IN AND OUT: on a host whose native cold
/// constructor is qualified (Apple Silicon), an agent that ended under an earlier aterm process
/// (a reboot) is admitted now, on its recorded launch environment, for the typed resume line;
/// only an agent that ended under this host is deferred to the constructor, whose death
/// witness can prove only that one.
#[test]
fn a_qualified_host_admits_an_agent_of_an_earlier_host_now_and_defers_only_its_own() {
    let c = cold_case(20, None);
    let earlier = account_cold_decided(
        &c.seat,
        &c.original,
        c.mode,
        ColdEnd::EarlierHost,
        true,
        || PLATFORM_SHELL,
    );
    match earlier {
        Ok(relaunch::ColdAdmission::Immediate(Some(chosen))) => {
            assert_eq!(chosen.login.as_deref(), Some(c.original_dir.as_str()));
            assert_eq!(chosen.session, c.original.session);
        }
        other => panic!(
            "an earlier host's agent is admitted now: {:?}",
            other.is_ok()
        ),
    }
    // NEGATIVE CONTROL: this host's own agent still goes to the constructor.
    match account_cold_decided(
        &c.seat,
        &c.original,
        c.mode,
        ColdEnd::ThisHost,
        true,
        || PLATFORM_SHELL,
    ) {
        Ok(relaunch::ColdAdmission::Deferred(intent)) => {
            assert_eq!(intent.conversation, c.original.session);
        }
        other => panic!("this host's agent is deferred: {:?}", other.is_ok()),
    }
    // An unqualified host admits both now, as before.
    for end in [ColdEnd::EarlierHost, ColdEnd::ThisHost] {
        assert!(matches!(
            account_cold_decided(&c.seat, &c.original, c.mode, end, false, || PLATFORM_SHELL),
            Ok(relaunch::ColdAdmission::Immediate(Some(_)))
        ));
    }
}

/// What a restore through the qualified decision handed the relaunch: each attempt's login
/// and whether it carried a deferred cold intent.
type Handed = Arc<Mutex<Vec<(Option<String>, bool)>>>;

/// A host restoring case 20's worker (a stored login, its launch environment recorded, its
/// tab's shell the platform shell) through the qualified host's real decision
/// ([`account_cold_decided`]), restored from the layout (`cold`) or not. The relaunch answers
/// as the agent does: a typed line lands (`adopted`), a deferred intent for a tab whose shell
/// is not its agent's waits on a witness it cannot have (the agent-side control
/// `a_cold_intent_for_an_agent_of_an_earlier_host_waits_on_a_witness_it_cannot_have`).
fn restore_qualified(cold: bool) -> (Arc<ColdCase>, Arc<World>, HostHandle, Handed) {
    let c = Arc::new(cold_case(20, None));
    let world = Arc::new(World::default());
    let handed: Handed = Arc::default();
    let mut h = hooks(&world, parking_body(&world));
    h.pause = Arc::new(|_| Duration::from_millis(1));
    let seat = c.seat.clone();
    h.acts.account_cold = Arc::new(move |snapshot, mode, end| {
        account_cold_decided(&seat, snapshot, mode, end, true, || PLATFORM_SHELL)
    });
    let recorded = Arc::clone(&handed);
    h.acts.relaunch_restored = Arc::new(move |_, snapshot, _, _, launch| {
        recorded
            .lock()
            .unwrap()
            .push((snapshot.login.clone(), launch.deferred.is_some()));
        if launch.deferred.is_some() {
            "wait:native-crash-observation".into()
        } else {
            "adopted".into()
        }
    });
    let mut cfg = on();
    cfg.set("accounts", c.mode.as_str()).unwrap();
    let host = HostHandle::start(cfg, false, false, h);
    host.relaunch_restored(vec![RestoredAgent {
        sid: c.original.tab.clone(),
        snap: c.original.clone(),
        place: "test".into(),
        cold,
        asked: None,
        was: None,
    }]);
    (c, world, host, handed)
}

fn restored_over_noted(world: &World, sid: &str) -> bool {
    world
        .notes
        .lock()
        .unwrap()
        .iter()
        .any(|note| matches!(note, HostNote::RestoredOver { sid: s } if s == sid))
}

/// RS1: A SUPERVISED WORKER RESTORED COLD AFTER A REBOOT RELAUNCHES ITS AGENT ON ITS RECORDED
/// ENVIRONMENT AND KEEPS ITS TASK. Its relaunch is the typed resume line, under the login its
/// recorded launch environment names, and the restore never tells the window its agent is not
/// coming back (which turns the tab's task from `working` to `exited why=exit`). NEGATIVE
/// CONTROL, the path the reboot of 2026-11-05 took: the same worker handed to the native cold
/// constructor waits on `native-crash-observation` every try, launches nothing, and is over
/// without its agent.
#[test]
fn a_worker_restored_cold_relaunches_on_its_recorded_environment_and_keeps_its_task() {
    let (c, world, host, handed) = restore_qualified(true);
    until("the restored worker's relaunch", || {
        !handed.lock().unwrap().is_empty()
    });
    host.shutdown_and_join();
    let handed = handed.lock().unwrap();
    assert_eq!(
        handed[0],
        (Some(c.original_dir.clone()), false),
        "the typed line on the recorded login, no deferred intent"
    );
    assert_eq!(handed.len(), 1, "one relaunch: {handed:?}");
    assert!(!restored_over_noted(&world, &c.original.tab));

    let (c, world, host, handed) = restore_qualified(false);
    until("the constructor path is over without its agent", || {
        restored_over_noted(&world, &c.original.tab)
    });
    host.shutdown_and_join();
    let handed = handed.lock().unwrap();
    assert!(handed.len() > 1, "it retried: {handed:?}");
    assert!(handed.iter().all(|(_, deferred)| *deferred));
}

/// A WORKER THAT RAN ON ITS ACCOUNT'S VAULT TOKEN COMES BACK THROUGH THE WRAPPER
/// (DESIGN-claude-setup-token S10, §5.4): on a qualified host, its own agent's end is admitted
/// now, so the planned line (which carries the token wrapper ahead of the build) relaunches it,
/// never the native constructor's fixed seed, which executes the build. NEGATIVE CONTROL: on its
/// stored login, this host's own agent is deferred as before.
#[test]
fn a_token_session_of_this_host_is_admitted_for_the_planned_line() {
    use aterm_agent::harness::accounts::{AuthRoute, TokenEntry};
    let c = cold_case(20, None);
    let token = Snapshot {
        route: AuthRoute::VaultToken {
            entry: TokenEntry::parse("aterm-claude-token-ac-fedcba9876543210").expect("an entry"),
        },
        ..c.original.clone()
    };
    let decided = |snap: &Snapshot| {
        account_cold_decided(&c.seat, snap, c.mode, ColdEnd::ThisHost, true, || {
            PLATFORM_SHELL
        })
    };
    assert!(
        matches!(
            decided(&token),
            Ok(relaunch::ColdAdmission::Immediate(Some(_)))
        ),
        "the token session is admitted now"
    );
    assert!(matches!(
        decided(&c.original),
        Ok(relaunch::ColdAdmission::Deferred(_))
    ));
}
