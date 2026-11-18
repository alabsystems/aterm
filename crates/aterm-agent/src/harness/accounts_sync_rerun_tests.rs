// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What a sync of the default login answers for: each occupant of the directory a displaced row
//! takes, what its receipts and the setup's say, what upkeep reads of it, and a renewal's row.

use super::*;

/// A sync's refusal for each occupant of a displaced row's directory, by its reason. `was` lists
/// that directory where `left`, spelled apart from it, since a location is what it names.
#[test]
fn each_occupant_of_a_displaced_rows_directory_answers_for_itself() {
    // This directory-only judgment needs no native vault or account-source fixture.
    let scratch = aterm_tempfile::tempdir().unwrap();
    let dir = scratch.path().join("login");
    let spelled = dir.display().to_string();
    let verdict = |left: bool| {
        fs::create_dir_all(&dir).unwrap();
        let was = if left {
            vec![Some(format!("{spelled}/"))]
        } else {
            Vec::new()
        };
        displaced_dir_refusal(&spelled, &was, A).unwrap()
    };
    let clear = |dir: &Path| {
        let _ = fs::remove_dir_all(dir);
        fs::create_dir_all(dir).unwrap();
    };

    clear(&dir);
    assert!(verdict(false).is_none(), "empty");
    fs::write(
        dir.join(crate::harness::codex_app_login::AUTH_LOCK_FILE),
        "",
    )
    .unwrap();
    assert!(verdict(false).is_none(), "a lock is no login");
    fs::write(dir.join(accounts::RENEWAL_CHECKPOINT_FILE), "{}").unwrap();
    assert!(verdict(false).is_none(), "nor is a checkpoint");

    clear(&dir);
    fs::write(dir.join("notes"), "x").unwrap();
    assert!(matches!(verdict(false), Some(DisplacedDir::OtherFiles)));

    clear(&dir);
    fs::write(dir.join("auth.json"), "{}").unwrap();
    assert!(matches!(verdict(false), Some(DisplacedDir::Unreadable)));

    clear(&dir);
    write_login(&dir, "z@example.com");
    assert!(
        matches!(verdict(true), Some(DisplacedDir::NamesAnother(named)) if named == "z@example.com"),
        "another account's, even where the row left it"
    );

    clear(&dir);
    write_login(&dir, A);
    assert!(matches!(verdict(false), Some(DisplacedDir::OwnNeverLeft)));
    assert!(verdict(true).is_none(), "its own, in a directory it left");
    fs::write(
        dir.join("config.toml"),
        "cli_auth_credentials_store='keyring'\n",
    )
    .unwrap();
    assert!(
        matches!(verdict(true), Some(DisplacedDir::KeyringStore)),
        "a file says nothing where the logins are in the keyring"
    );
}

/// A SYNC THAT PUBLISHED ROWS AND REFUSED THE MOVE SAYS NO ROW MOVED, never that nothing was
/// written: the new row is in the file. CONTROL: the row is there.
#[cfg(unix)]
#[test]
fn a_sync_that_published_rows_and_refused_the_move_says_no_row_moved() {
    let world = World::new("published-then-refused-");
    let store = world.store();
    crate::vault_entry::seed(
        &world.home,
        &[(
            &cli_fleet_file(Cli::Codex),
            format!(
                "[[account]]\nlabel='a'\nemail='{A}'\nmachines=['m16']\n\n\
                 [[account]]\nlabel='b'\nemail='{B}'\nmachines=['m16']\n\n\
                 [[account]]\nlabel='c'\nemail='c@example.com'\nmachines=['m16']\n"
            )
            .as_bytes(),
        )],
    );
    let dir_a = store.login_dir("a").unwrap();
    fs::create_dir_all(dir_a.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink("/nonexistent-rerun-target", &dir_a).unwrap();
    world.login(B);
    let (code, out, err) = sync_words(&world);
    assert_eq!(code, ExitCode::FAILURE, "{out}{err}");
    assert!(world.rows().row("c").is_some(), "CONTROL: {out}{err}");
    assert!(err.contains("no row moved"), "{err}");
    assert!(!err.contains("nothing was written"), "{err}");
}

/// THE SETUP'S RESYNC COUNTS A MOVE IT MADE WITH NOTHING TO READ: it says it synced, and changed.
#[test]
fn the_setup_resync_counts_a_move_it_made_with_nothing_to_read() {
    let world = World::new("setup-resync-");
    world.login(B);
    let claude = world.config.with_file_name(accounts::ACCOUNTS_FILE);
    let fetch =
        |_: &Env, source: &str| -> Result<String, String> { Err(format!("no fetch: {source}")) };
    let spawn = |_work: Box<dyn FnOnce() + Send>| Ok(());
    let (cooldown, rediscovery) = (
        Watched::new("accounts_sync/sync_cooldown", None),
        Watched::new("accounts_sync/rediscovery", Rediscovery::default()),
    );
    let guard = AutoRenewGuard::default().shared();
    let fx =
        super::super::login_tests::upkeep_effects(&fetch, &spawn, &cooldown, &rediscovery, &guard);
    let resync = resync_with(&world.home, &world.state, (&claude, NOW, false), &fx);
    assert_eq!(world.rows().row_for_login(None).unwrap().label, "b");
    assert!(resync.synced, "{:?}", resync.failed);
    assert!(resync.changed, "{:?}", resync.failed);
}

/// UPKEEP CARRIES A REFUSED MOVE AS A FAILURE, AND A MOVE WITH NOTHING TO READ WAKES THE POOL.
#[test]
fn upkeep_carries_a_refused_move_and_a_move_with_nothing_to_read_wakes_the_pool() {
    let fetch =
        |_: &Env, source: &str| -> Result<String, String> { Err(format!("no fetch: {source}")) };
    let codex = |ran: Vec<(Cli, Result<CliSync, String>)>| {
        ran.into_iter()
            .find(|(cli, _)| *cli == Cli::Codex)
            .map(|(_, synced)| synced)
            .unwrap()
    };

    let world = World::new("upkeep-refused-");
    write_login(&world.store().login_dir("a").unwrap(), "z@example.com");
    world.login(B);
    let synced = codex(sync_cli_accounts_with(
        &world.home,
        &world.state,
        &world.roster,
        NOW,
        &fetch,
    ));
    assert!(synced.is_err(), "{synced:?}");

    let world = World::new("upkeep-moved-");
    let wake = world.store().wake_path();
    let _ = fs::remove_file(&wake);
    world.login(B);
    let synced = codex(sync_cli_accounts_with(
        &world.home,
        &world.state,
        &world.roster,
        NOW,
        &fetch,
    ));
    assert_eq!(world.rows().row_for_login(None).unwrap().label, "b");
    assert!(wake.exists(), "the pool woke: {synced:?}");
}

/// A SESSION ON THE DIRECTORY THE NEW DEFAULT ROW LEFT: the bound row carries what it left, and
/// `--json` names the row that took the login, with a source to read and with nothing to read.
#[test]
fn a_move_leaves_the_row_what_it_left_and_the_receipt_names_it() {
    for source in [
        #[cfg(unix)]
        true,
        false,
    ] {
        let world = if source {
            #[cfg(unix)]
            {
                World::with_source("bound-was-")
            }
            #[cfg(not(unix))]
            {
                panic!("the real vault fixture requires Unix")
            }
        } else {
            World::new("bound-was-")
        };
        let store = world.store();
        let dir_b = store.login_dir("b").unwrap();
        write_login(&dir_b, B);
        world.login(B);
        let fetch = |_: &Env, _: &str| -> Result<String, String> { panic!("no source is fetched") };
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let _ = run_cli_accounts_sync_with(
            Cli::Codex,
            (&world.env(), None),
            (None, None, true),
            (&mut out, &mut err),
            &fetch,
        );
        let doc: aterm_json::Value = aterm_json::from_str(String::from_utf8_lossy(&out).trim())
            .unwrap_or_else(|_| panic!("{}", String::from_utf8_lossy(&err)));
        // The row is named by its account's key, never by its label.
        let b = crate::harness::account_names::names_of_login(Cli::Codex, None, B, None)
            .expect("b's account")
            .0;
        assert_eq!(doc["own_row"]["ac"], aterm_json::Value::from(b), "{doc}");
        assert!(doc["own_row"].get("label").is_none(), "{doc}");

        let logins = store.load_logins(NOW).unwrap().0;
        let bound = accounts::bound(&world.rows(), &logins);
        let left = dir_b.display().to_string();
        let was = &bound.row("b").unwrap().was;
        assert!(
            was.iter().flatten().any(|dir| *dir == left),
            "what `b` left: {was:?}"
        );
    }
}

/// A ROSTER THAT DOES NOT PARSE IS THE SYNC'S ERROR, EVEN WITH NOTHING TO READ: no sync of it reads
/// as a Mac that keeps nothing, so upkeep says why instead of passing it over.
#[test]
fn a_broken_roster_is_the_syncs_error_with_nothing_to_read() {
    let world = World::new("broken-roster-");
    world.login(B);
    fs::write(&world.roster, "[[account\nlabel=").unwrap();
    let synced = world.sync();
    assert!(synced.is_err(), "{:?}", synced.is_ok());
}

/// A RENEWAL FROM A SESSION WHOSE DIRECTORY ANOTHER ROW NOW HOLDS IS THAT ROW'S, not the row's that
/// left it: `a` owns it, so nothing follows, where `b`'s `was` would have made two candidates.
#[test]
fn a_renewal_on_a_directory_another_row_now_holds_is_that_rows() {
    let world = World::new("renewal-current-owner-");
    let store = world.store();
    let dir = store.login_dir("a").unwrap();
    write_login(&dir, A);
    let spelled = dir.display().to_string();
    let mut rows = world.rows();
    for row in &mut rows.rows {
        row.dir = (row.label == "a").then(|| spelled.clone());
    }
    world.save_rows(&rows);
    store
        .transact_logins(NOW, &mut None, |_, logins| {
            logins.labels.insert(
                "b".into(),
                accounts::Binding {
                    was: vec![Some(spelled.clone())],
                    ..accounts::Binding::default()
                },
            );
            Ok(Vec::new())
        })
        .unwrap();
    let identity = crate::harness::codex_renewal::Identity::from_native("account", "user");
    let followed = crate::harness::codex_renewal::with_successor(
        &world.env(),
        &dir.join("auth.json"),
        &identity,
        |_| Ok(()),
    );
    assert_eq!(followed, Ok(None));
}
