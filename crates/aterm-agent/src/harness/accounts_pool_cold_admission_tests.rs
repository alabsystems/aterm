// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn decide(case: i64) -> [i64; 3] {
    let world = World::new("cold-admission");
    let work = world.two(Cli::Codex);
    usage_policy::write_context_login(&work, 1);
    let path = world.cfg.join(Cli::Codex.accounts().roster_file);
    let mut roster = world.seat().roster(Cli::Codex).unwrap();
    roster.rows.retain(|row| row.label == "work");
    roster.rows[0].email = Some(if case == 3 {
        "another@example.test".into()
    } else {
        "fixture@example.test".into()
    });
    if case == 2 {
        roster.rows[0].held_by = Some("another-machine".into());
    }
    std::fs::write(
        &path,
        accounts::append_rows_for(Cli::Codex, "", &roster.rows).unwrap(),
    )
    .unwrap();
    if case == 7 || case == 8 || case == 14 {
        aterm_uds::atomic_file::write(
            &(world.cfg.join("aterm.toml")),
            ("[harness]\naccounts='off'\n").as_ref(),
            0o600,
        )
        .unwrap();
    }
    let seat = world.seat();
    let store = seat.store(Cli::Codex);
    let renewed = world.root.join("renewed");
    if case == 10 {
        usage_policy::write_context_login(&renewed, 2);
        store
            .transact_logins(NOW, &mut None, |state, logins| {
                Ok(accounts::note_renewed(
                    state,
                    logins,
                    "work",
                    work.to_str(),
                    renewed.to_str().unwrap(),
                    None,
                    NOW,
                ))
            })
            .unwrap();
    }
    if case == 11 {
        store
            .transact_logins(NOW, &mut None, |_, logins| {
                logins.labels.insert(
                    "work".into(),
                    accounts::Binding {
                        dir: Some(work.to_str().unwrap().into()),
                        ..accounts::Binding::default()
                    },
                );
                Ok(Vec::new())
            })
            .unwrap();
        std::fs::write(&path, "").unwrap();
    }
    store
        .transact(NOW, &mut None, |state| {
            if case == 1 || case == 7 {
                state.prepared_accounts.insert("work".into());
            }
            if case == 9 {
                accounts::note_wall(state, "work", "weekly", Some(NOW + HOUR), NOW, NOW);
            }
            Ok(Vec::new())
        })
        .unwrap();
    if case == 4 {
        std::fs::remove_file(work.join("auth.json")).unwrap();
    }
    let files = [
        store.state_file(),
        store.logins_file(),
        store.ledger_path(),
        store.wake_path(),
    ];
    let after_read = std::sync::Mutex::new(files.each_ref().map(|file| std::fs::read(file).ok()));
    let read = |_: Cli, _: &Path, _: Option<&str>, _: i64| {
        if case == 5 {
            let mut changed = roster.clone();
            changed.rows[0].browser = Some("changed".into());
            std::fs::write(
                &path,
                accounts::append_rows_for(Cli::Codex, "", &changed.rows).unwrap(),
            )
            .unwrap();
        } else if case == 6 {
            store
                .transact_logins(NOW, &mut None, |state, logins| {
                    Ok(accounts::note_renewed(
                        state,
                        logins,
                        "work",
                        work.to_str(),
                        work.to_str().unwrap(),
                        None,
                        NOW,
                    ))
                })
                .unwrap();
        }
        *after_read.lock().unwrap() = files.each_ref().map(|file| std::fs::read(file).ok());
        None
    };
    let auth = if case == 12 {
        let own = world.root.join("unmanaged");
        usage_policy::write_context_login(&own, 1);
        own.join("auth.json")
    } else {
        work.join("auth.json")
    }
    .display()
    .to_string();
    let old_program = if case == 13 || case == 14 {
        Some(crate::harness::codex_alab::lay_package(
            &world.root.join("old-program"),
            false,
        ))
    } else {
        None
    };
    let result = super::super::cold_launch_env(
        &seat,
        Cli::Codex,
        &[("CODEX_AUTH_PATH", &auth)],
        &read,
        Some(old_program.as_ref().unwrap_or(&world.exe)),
    );
    let admitted = result.is_ok();
    let picked = result.ok().flatten();
    let rebound = picked.is_some();
    if let Some(picked) = picked {
        assert_eq!(case, 10);
        assert_eq!(picked.value.as_deref(), renewed.join("auth.json").to_str());
    }
    [
        i64::from(admitted),
        i64::from(
            *after_read.lock().unwrap() == files.each_ref().map(|file| std::fs::read(file).ok()),
        ),
        i64::from(rebound),
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
        let [admitted, preserved, rebound] = decide(state["case"]);
        next.insert("admitted", admitted);
        next.insert("preserved", preserved);
        next.insert("rebound", rebound);
        next.insert("phase", 1);
    }
    next
}

#[test]
fn cold_launch_admission_conforms_and_catches_unchecked_saved_logins() {
    let model = aterm_models_harness::cold_launch_admission_model();
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
    let mut bad = model.init_state();
    bad.insert("case", 1);
    bad = admission_transition(&bad, "Decide");
    bad.insert("admitted", 1);
    assert!(!model.check_invariant("CurrentAdmission", &bad));
    bad = model.init_state();
    bad.insert("case", 10);
    bad = admission_transition(&bad, "Decide");
    bad.insert("rebound", 0);
    assert!(!model.check_invariant("CurrentBinding", &bad));
}

fn witness_world(tag: &str) -> (World, PathBuf) {
    let world = World::new(tag);
    let work = world.two(Cli::Claude);
    std::fs::create_dir_all(world.home.join(".claude")).unwrap();
    std::fs::write(work.join(".claude.json"), b"{}").unwrap();
    aterm_uds::atomic_file::write(
        &world.cfg.join("aterm.toml"),
        b"[harness]\naccounts='wall'\n",
        0o600,
    )
    .unwrap();
    world
        .seat()
        .store(Cli::Claude)
        .transact(NOW, &mut None, |state| {
            state.home_accounts.insert("work".into());
            Ok(Vec::new())
        })
        .unwrap();
    world
        .seat()
        .store(Cli::Claude)
        .load(NOW)
        .unwrap()
        .0
        .execution_verdict("work")
        .unwrap();
    (world, work)
}

fn admit_witness(world: &World, login: Option<&Path>) -> ColdLaunchAdmission {
    super::super::cold_launch_admitted(
        &world.seat(),
        Cli::Claude,
        accounts::ProcessLogin {
            login: login.map(|path| path.to_str().unwrap().to_owned()),
            default_home: true,
            stored_login: true,
        },
        &NoUsage,
        Some(&world.exe),
    )
    .unwrap()
}

fn validate_intent(world: &World, intent: &ExecutionIntent) -> Result<(), String> {
    super::super::admit_execution(&world.seat(), Cli::Claude, intent).map(drop)
}

fn witness_images(world: &World) -> [Option<Vec<u8>>; 5] {
    let store = world.seat().store(Cli::Claude);
    [
        store.state_file(),
        store.logins_file(),
        store.ledger_path(),
        world.cfg.join(Cli::Claude.accounts().roster_file),
        store.dir.join("sync-commit.json"),
    ]
    .map(|path| std::fs::read(path).ok())
}

#[test]
fn cold_witness_keeps_the_exact_managed_binding_and_validates_without_writes() {
    let (world, work) = witness_world("cold-witness-managed");
    let admitted = admit_witness(&world, Some(&work));
    assert!(matches!(
        &admitted.intent,
        ExecutionIntent::Stored { owner: Some(_), .. }
    ));
    assert!(admitted.pick.is_none());
    let before = witness_images(&world);
    let grant =
        super::super::admit_execution(&world.seat(), Cli::Claude, &admitted.intent).unwrap();
    assert!(
        world
            .seat()
            .store(Cli::Claude)
            .acquire_within(&[])
            .unwrap()
            .is_none()
    );
    drop(grant);
    validate_intent(&world, &admitted.intent).unwrap();
    assert_eq!(witness_images(&world), before);
    // The same path now has a new binding generation, even with identical command bytes.
    world
        .seat()
        .store(Cli::Claude)
        .transact_logins(NOW, &mut None, |state, logins| {
            Ok(accounts::note_renewed(
                state,
                logins,
                "work",
                work.to_str(),
                work.to_str().unwrap(),
                None,
                NOW,
            ))
        })
        .unwrap();
    let before = witness_images(&world);
    assert!(validate_intent(&world, &admitted.intent).is_err());
    assert_eq!(witness_images(&world), before);
}

#[test]
fn cold_witness_rejects_same_directory_row_identity_and_local_profile_replacement() {
    for profile_change in [false, true] {
        let (world, work) = witness_world("cold-witness-identity");
        std::fs::write(
            work.join(".claude.json"),
            b"{\"oauthAccount\":{\"accountUuid\":\"first-principal\"}}",
        )
        .unwrap();
        let admitted = admit_witness(&world, Some(&work));
        validate_intent(&world, &admitted.intent).unwrap();
        if profile_change {
            std::fs::write(
                work.join(".claude.json"),
                b"{\"oauthAccount\":{\"accountUuid\":\"second-principal\"}}",
            )
            .unwrap();
        } else {
            let mut roster = world.seat().roster(Cli::Claude).unwrap();
            roster
                .rows
                .iter_mut()
                .find(|row| row.label == "work")
                .unwrap()
                .identity_email = Some("new-owner@example.test".into());
            std::fs::write(
                world.cfg.join(Cli::Claude.accounts().roster_file),
                accounts::append_rows_for(Cli::Claude, "", &roster.rows).unwrap(),
            )
            .unwrap();
        }
        let before = witness_images(&world);
        assert!(validate_intent(&world, &admitted.intent).is_err());
        assert_eq!(witness_images(&world), before);
    }
}

#[test]
fn cold_witness_missing_unmanaged_profile_is_not_an_account_identity_grant() {
    let world = World::new("cold-witness-unmanaged");
    std::fs::create_dir_all(world.home.join(".claude")).unwrap();
    let admitted = admit_witness(&world, None);
    assert!(matches!(
        &admitted.intent,
        ExecutionIntent::Stored { owner: None, .. }
    ));
    assert_eq!(
        crate::harness::profile::default_identity_evidence(&world.home, "").unwrap(),
        crate::harness::profile::IdentityEvidence::Missing
    );
    let before = witness_images(&world);
    validate_intent(&world, &admitted.intent).unwrap();
    assert_eq!(witness_images(&world), before);
    // A new default row changes the exact previously unmanaged namespace.
    std::fs::write(
        world.cfg.join(Cli::Claude.accounts().roster_file),
        "[[account]]\nlabel='new-owner'\nclass='dedicated'\n",
    )
    .unwrap();
    let before = witness_images(&world);
    assert!(validate_intent(&world, &admitted.intent).is_err());
    assert_eq!(witness_images(&world), before);
}

#[test]
fn cold_witness_unavailable_profiles_never_become_missing_evidence() {
    for kind in ["malformed", "nonfile", "dangling"] {
        if kind == "dangling" && !cfg!(unix) {
            continue;
        }
        let (world, work) = witness_world("cold-witness-unavailable");
        let initial = admit_witness(&world, Some(&work));
        validate_intent(&world, &initial.intent).unwrap();
        let profile = work.join(".claude.json");
        std::fs::remove_file(&profile).unwrap();
        match kind {
            "malformed" => std::fs::write(&profile, b"not JSON").unwrap(),
            "nonfile" => std::fs::create_dir(&profile).unwrap(),
            _ => {
                #[cfg(unix)]
                std::os::unix::fs::symlink(work.join("absent-profile"), &profile).unwrap();
            }
        }
        assert!(crate::harness::profile::identity_evidence_in_home(&work, "").is_err());
        let before = witness_images(&world);
        assert!(
            super::super::cold_launch_admitted(
                &world.seat(),
                Cli::Claude,
                accounts::ProcessLogin {
                    login: Some(work.to_str().unwrap().into()),
                    default_home: true,
                    stored_login: true,
                },
                &NoUsage,
                Some(&world.exe)
            )
            .is_err()
        );
        // Initial admission may perform its established claim bookkeeping; no credentials or
        // roster are repaired from invalid evidence. Validation itself is checked above.
        assert_eq!(
            std::fs::read(world.cfg.join(Cli::Claude.accounts().roster_file)).ok(),
            before[3]
        );
    }
}

#[test]
fn cold_witness_refuses_pending_publication_without_recovering_it() {
    let (world, work) = witness_world("cold-witness-pending");
    let admitted = admit_witness(&world, Some(&work));
    validate_intent(&world, &admitted.intent).unwrap();
    super::recovery::pending_renewal(&world, Cli::Claude, &work, false);
    let before = witness_images(&world);
    assert!(
        before[4].is_some(),
        "the actual interrupted publication must exist"
    );
    assert!(validate_intent(&world, &admitted.intent).is_err());
    assert_eq!(
        witness_images(&world),
        before,
        "validation must not replay the pending publication"
    );
}

#[test]
fn cold_witness_rejects_email_only_replacement_with_no_provider_principal() {
    let (world, work) = witness_world("cold-witness-email-only");
    let path = work.join(".claude.json");
    let write = |email: &str| {
        std::fs::write(
            &path,
            aterm_json::to_vec(&aterm_json::json!({"oauthAccount":{"emailAddress":email}}))
                .unwrap(),
        )
        .unwrap()
    };
    write("first@example.test");
    let admitted = admit_witness(&world, Some(&work));
    validate_intent(&world, &admitted.intent).unwrap();
    write("second@example.test");
    let before = witness_images(&world);
    assert!(validate_intent(&world, &admitted.intent).is_err());
    assert_eq!(witness_images(&world), before);
}
