// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::harness::accounts::Mode;
use crate::harness::codex_alab::DefaultRow;

fn auth(email: Option<&str>) -> Vec<u8> {
    let token = crate::harness::codex_app_login::tests::make_jwt(&aterm_json::json!({
        "email": email,
        "https://api.openai.com/auth": {
            "chatgpt_user_id": "fixture-user",
            "chatgpt_account_id": "fixture-account"
        }
    }));
    aterm_json::to_vec(&aterm_json::json!({
        "auth_mode": "chatgpt",
        "tokens": {"id_token": token, "access_token": token}
    }))
    .unwrap()
}

#[test]
fn passthrough_facts_withhold_a_known_mismatched_default_identity() {
    for (lone, mode) in [(false, Mode::Off), (true, Mode::Early)] {
        let pool = DoctorPool::new(true, Some(mode));
        if lone {
            pool.main_alone(Some("main@example.com"));
        }
        pool.state(|state| state.active = Some("main".into()));
        let files = [
            pool.store.state_file(),
            pool.store.logins_file(),
            pool.store.ledger_path(),
            pool.store.wake_path(),
        ];
        let before = files.each_ref().map(|path| std::fs::read(path).ok());
        let path = pool.b.home.join(".codex/auth.json");
        let replaced = auth(Some("other@example.com"));
        std::fs::write(&path, &replaced).unwrap();
        let facts = pool.facts();
        let view = facts.pool.as_ref().unwrap();
        assert_eq!(view.serving_label, None);
        assert_eq!(view.serving_login, None);
        assert_eq!(view.serving_login_file, None);
        assert!(view.launch_refused.is_some());
        assert!(matches!(view.default_row, Some(DefaultRow::Other { .. })));
        assert_eq!(view.default_label.as_deref(), Some("main"));
        assert_eq!(
            files.each_ref().map(|path| std::fs::read(path).ok()),
            before
        );
        assert_eq!(std::fs::read(&path).unwrap(), replaced);

        for email in [Some("main@example.com"), None] {
            let accepted = auth(email);
            std::fs::write(&path, &accepted).unwrap();
            let facts = pool.facts();
            let view = facts.pool.as_ref().unwrap();
            assert_eq!(view.serving_label.as_deref(), Some("main"));
            assert_eq!(view.serving_login, Some(("CODEX_AUTH_PATH", None)));
            assert!(view.serving_login_file.is_some());
            assert_eq!(view.launch_refused, None);
            assert!(matches!(view.default_row, Some(DefaultRow::Serves(_))));
            assert_eq!(
                files.each_ref().map(|path| std::fs::read(path).ok()),
                before
            );
            assert_eq!(std::fs::read(&path).unwrap(), accepted);
        }
    }
}

#[test]
fn a_refused_launch_does_not_preserve_mismatched_default_attribution() {
    let pool = DoctorPool::new(false, Some(Mode::Early));
    pool.wall_main();
    let before = pool.facts();
    let view = before.pool.as_ref().unwrap();
    assert!(view.launch_refused.is_some());
    assert_eq!(view.serving_label.as_deref(), Some("main"));
    let path = pool.b.home.join(".codex/auth.json");
    let replaced = auth(Some("other@example.com"));
    std::fs::write(&path, &replaced).unwrap();
    let after = pool.facts();
    let view = after.pool.as_ref().unwrap();
    assert!(view.launch_refused.is_some());
    assert_eq!(view.serving_label, None);
    assert_eq!(view.serving_login, None);
    assert_eq!(view.serving_login_file, None);
    assert_eq!(std::fs::read(path).unwrap(), replaced);
}

#[test]
fn default_drift_leaves_an_independent_serving_login_attributed() {
    let pool = DoctorPool::new(true, Some(Mode::Off));
    pool.pin("work");
    let default_path = pool.b.home.join(".codex/auth.json");
    let work_path = pool.b.home.join(".aterm/logins/codex/work/auth.json");
    let replaced = auth(Some("other@example.com"));
    let independent = auth(Some("work@example.com"));
    std::fs::write(&default_path, &replaced).unwrap();
    std::fs::write(&work_path, &independent).unwrap();
    let facts = pool.facts();
    let view = facts.pool.as_ref().unwrap();
    assert_eq!(view.serving_label.as_deref(), Some("work"));
    assert!(
        view.serving_login
            .as_ref()
            .is_some_and(|(_, path)| path.is_some())
    );
    assert_eq!(view.launch_refused, None);
    assert!(matches!(view.default_row, Some(DefaultRow::Other { .. })));
    assert_eq!(std::fs::read(&default_path).unwrap(), replaced);
    assert_eq!(std::fs::read(&work_path).unwrap(), independent);

    let displaced = auth(Some("another@example.com"));
    std::fs::write(&work_path, &displaced).unwrap();
    let facts = pool.facts();
    let view = facts.pool.as_ref().unwrap();
    assert_eq!(view.serving_label, None);
    assert_eq!(view.serving_login, None);
    assert_eq!(view.serving_login_file, None);
    assert!(view.launch_refused.is_some());
    assert_eq!(std::fs::read(default_path).unwrap(), replaced);
    assert_eq!(std::fs::read(work_path).unwrap(), displaced);
}

#[test]
fn facts_choose_the_valid_alternative_a_managed_launch_uses() {
    use crate::harness::accounts::Cli;
    let pool = DoctorPool::new(true, Some(Mode::Early));
    aterm_uds::atomic_file::write(
        &pool.config.join("aterm.toml"),
        b"[harness]\naccounts='early'\n",
        0o600,
    )
    .unwrap();
    pool.state(|state| {
        state.active = Some("work".into());
        state.pinned = false;
    });
    let default_auth = pool.b.home.join(".codex/auth.json");
    let work_auth = pool.b.home.join(".aterm/logins/codex/work/auth.json");
    let valid = auth(Some("main@example.com"));
    let displaced = auth(Some("other@example.com"));
    std::fs::write(&default_auth, &valid).unwrap();
    std::fs::write(&work_auth, &displaced).unwrap();
    let files = [
        pool.store.state_file(),
        pool.store.logins_file(),
        pool.store.ledger_path(),
        pool.store.wake_path(),
    ];
    let before = files.each_ref().map(|path| std::fs::read(path).ok());
    let facts = pool.facts();
    let view = facts.pool.as_ref().unwrap();
    assert_eq!(view.serving_label.as_deref(), Some("main"));
    assert_eq!(view.serving_login, Some(("CODEX_AUTH_PATH", None)));
    assert_eq!(view.launch_refused, None);
    assert_eq!(
        files.each_ref().map(|path| std::fs::read(path).ok()),
        before
    );
    let seat = crate::harness::accounts_pool::Seat {
        config_dir: &pool.config,
        state: &pool.b.state,
        home: &pool.b.home,
        now: DoctorPool::NOW,
    };
    let agents = pool.b.pkg.agents_dir();
    let program = crate::harness::worker::codex_launch_program_in(Some(&agents));
    let no_network = |_: Cli, _: &Path, _: Option<&str>, _: i64| None;
    let managed = crate::harness::accounts_pool::launch_env_for(
        &seat,
        Cli::Codex,
        &no_network,
        Some(&program),
    )
    .unwrap()
    .unwrap();
    assert_eq!(managed.label, "main");
    assert_eq!(std::fs::read(default_auth).unwrap(), valid);
    assert_eq!(std::fs::read(work_auth).unwrap(), displaced);
}

#[test]
fn default_only_wall_facts_keep_identity_refusal_ahead_of_reset_recovery() {
    use crate::harness::{
        accounts::{self, Cli, Roster},
        accounts_pool,
    };

    let pool = DoctorPool::new(false, Some(Mode::Early));
    pool.wall_main();
    let auth_path = pool.b.home.join(".codex/auth.json");
    let files = [
        pool.roster.clone(),
        pool.store.state_file(),
        pool.store.logins_file(),
        pool.store.ledger_path(),
        pool.store.wake_path(),
        auth_path.clone(),
        pool.b.home.join(".aterm/logins/codex/work/auth.json"),
    ];
    for email in ["other@example.com", "main@example.com"] {
        std::fs::write(&auth_path, auth(Some(email))).unwrap();
        let before = files.each_ref().map(|path| std::fs::read(path).ok());
        let facts = pool.facts();
        let view = facts.pool.as_ref().unwrap();
        assert!(view.default_only);
        let roster = Roster::load_for(Cli::Codex, &pool.roster).unwrap();
        let state = pool.store.load(DoctorPool::NOW).unwrap().0;
        let enrolled = accounts::enrolled_in(Cli::Codex, &roster, &pool.b.home);
        let policy = (Cli::Codex, false, Mode::Early);
        let program = view.program.as_deref();
        let wall =
            accounts_pool::launch_now(policy, &roster, &state, &enrolled, program, DoctorPool::NOW)
                .unwrap_err();
        let wall_words = wall.strip_suffix(accounts_pool::NO_WORKER).unwrap_or(&wall);
        let calls = std::cell::Cell::new(0);
        let observed = accounts_pool::launch_observed_with(
            policy,
            &roster,
            &state,
            &enrolled,
            program,
            DoctorPool::NOW,
            |picked| {
                calls.set(calls.get() + 1);
                assert_eq!(picked.map(|row| row.label.as_str()), Some("main"));
                accounts_pool::check_launch_login_identity(
                    Cli::Codex,
                    &roster,
                    picked,
                    &pool.b.home,
                    program,
                    None,
                )
            },
        )
        .unwrap_err()
        .to_string();
        assert_eq!(calls.get(), 1);
        let row = crate::ai::state::assess(&facts, true, DoctorPool::NOW);
        if email == "other@example.com" {
            let identity = accounts_pool::check_default_login_identity(
                Cli::Codex,
                &roster,
                &pool.b.home,
                program,
                None,
            )
            .unwrap_err()
            .to_string();
            assert_eq!(observed, identity);
            assert_eq!(view.launch_refused.as_deref(), Some(identity.as_str()));
            assert!(
                matches!(&view.default_row, Some(DefaultRow::Other { label, why }) if label == "main" && why == &identity)
            );
            assert_eq!(view.serving_label, None);
            assert!(row.fact.contains(&identity));
            assert!(!row.fact.contains(wall_words));
        } else {
            assert_eq!(observed, wall);
            assert_eq!(view.launch_refused.as_deref(), Some(wall.as_str()));
            assert!(
                matches!(&view.default_row, Some(DefaultRow::Walled { label, .. }) if label == "main")
            );
            assert!(row.fact.contains(wall_words));
        }
        assert_eq!(
            files.each_ref().map(|path| std::fs::read(path).ok()),
            before
        );
    }
}

#[test]
fn non_file_codex_facts_do_not_judge_a_contradictory_stale_auth_file() {
    for backend in ["keyring", "auto", "ephemeral", "unknown"] {
        let pool = DoctorPool::new(true, Some(Mode::Off));
        let directory = pool.b.home.join(".codex");
        std::fs::write(
            directory.join("config.toml"),
            format!("cli_auth_credentials_store = \"{backend}\"\n"),
        )
        .unwrap();
        let path = directory.join("auth.json");
        let stale = auth(Some("other@example.com"));
        std::fs::write(&path, &stale).unwrap();
        let facts = pool.facts();
        assert!(facts.hidden_store || facts.credential_store_error.is_some());
        let view = facts.pool.as_ref().unwrap();
        assert_eq!(view.serving_label, None);
        assert_eq!(view.serving_login, None);
        assert_eq!(view.launch_refused, None);
        assert_eq!(std::fs::read(path).unwrap(), stale);
    }
}

/// A POOL WHOSE LAUNCH IS REFUSED FOR WHAT ITS DEFAULT LOGIN NAMES COUNTS IN `health:`, by the CLI's
/// name, and asks a person for the sign-in that ends it. CONTROL: the login its row names is OK.
#[test]
fn a_launch_refused_for_a_moved_default_counts_in_health() {
    let pool = DoctorPool::new(true, Some(Mode::Early));
    pool.main_alone(Some("main@example.com"));
    pool.state(|state| state.active = Some("main".into()));
    let path = pool.b.home.join(".codex/auth.json");
    std::fs::write(&path, auth(Some("main@example.com"))).unwrap();
    let health_of = || {
        let row = crate::ai::state::assess(&pool.facts(), true, DoctorPool::NOW);
        crate::ai::state::health(&[row])
    };
    let control = health_of();
    assert!(!control.warning.contains(&"codex"), "CONTROL: {control:?}");
    std::fs::write(&path, auth(Some("other@example.com"))).unwrap();
    let health = health_of();
    assert_eq!(health.word, "WARN", "{health:?}");
    assert_eq!(health.warning, ["codex"], "{health:?}");
    assert_eq!(health.mends.len(), 1, "{health:?}");
}

/// A SYNC ASKS NO PERSON FOR ANYTHING: where a sync moves the row off a moved default login, the
/// doctor's row is a note that names no person and mends nothing, and aterm's upkeep runs that sync.
/// CONTROL: where no row holds that account, a person's sign-in ends it, and the row asks for it.
#[test]
fn a_moved_default_a_sync_ends_asks_no_person() {
    use crate::ai::state::{Actor, Mark, assess};
    let pool = DoctorPool::new(true, Some(Mode::Early));
    let default_login = pool.b.home.join(".codex/auth.json");
    std::fs::write(&default_login, auth(Some("other@example.com"))).unwrap();
    pool.main_alone(Some("main@example.com"));
    pool.state(|state| {
        state.active = Some("main".into());
        state.pinned = true;
    });
    let control = assess(&pool.facts(), true, DoctorPool::NOW);
    assert_eq!(control.mark, Mark::Warn, "CONTROL: {control:?}");
    assert_eq!(control.actor, Some(Actor::Person), "CONTROL: {control:?}");
    assert!(control.mend.is_some(), "CONTROL: {control:?}");

    let work = pool.b.home.join(".aterm/logins/codex/work");
    std::fs::write(work.join("auth.json"), auth(Some("other@example.com"))).unwrap();
    std::fs::write(
        &pool.roster,
        format!(
            "[[account]]\nlabel = \"main\"\nclass = \"dedicated\"\nemail = \"main@example.com\"\n\n\
             [[account]]\nlabel = \"work\"\nclass = \"dedicated\"\nemail = \"other@example.com\"\n\
             dir = \"{}\"\n",
            work.display()
        ),
    )
    .unwrap();
    let row = assess(&pool.facts(), true, DoctorPool::NOW);
    assert_eq!(row.mend, None, "{row:?}");
    assert_ne!(row.actor, Some(Actor::Person), "{row:?}");
    assert!(row.mark < Mark::Warn, "{row:?}");
}
