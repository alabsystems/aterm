// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn selected(settings: &[Setting]) -> Plan {
    let mut selection = Selection::none();
    for setting in settings {
        selection.set(*setting, true);
    }
    Plan::from(selection)
}

fn observe(root: &Path) -> Result<InitialSetupObservation, String> {
    observation_at(root).map_err(|error| error.to_string())
}

fn attempt(
    ticket: &mut InitialSetupAuthorization,
    root: &Path,
    plan: &Plan,
    write: impl FnMut(&AuthorizationWrite, &Path) -> Result<(), String>,
    read: impl FnOnce(&Path) -> Result<InitialSetupObservation, String>,
) -> InitialSetupObserved<Option<u64>> {
    observe_mutation_at(
        root,
        observation_at(root).ok(),
        Err("busy".into()),
        |root| {
            assert!(HeldLock::try_take(root)?.is_none());
            ticket.persist_with(root, plan, Admission::Person, write, read)
        },
    )
}

fn vault(root: &Path, name: &str) -> ConfiguredVault {
    ConfiguredVault {
        config: aterm_secrets::client::Config {
            source: aterm_secrets::transport::Source::Url(format!(
                "https://{name}.example/v1/{name}"
            )),
            mirrors: Vec::new(),
            name: name.into(),
            signer: Some([7; 32]),
            followed: Vec::new(),
            host: "fixture".into(),
        },
        home: root.to_path_buf(),
    }
}

#[test]
fn an_explicit_startup_join_binds_the_vault_read_at_admission() {
    for join in [false, true] {
        let root = aterm_tempfile::tempdir().unwrap();
        let offered_vault = vault(root.path(), "offered");
        let joined_since = vault(root.path(), "joined");
        let after_admission = vault(root.path(), "later");
        let plan = offered_vault.present(selected(&[Setting::Secrets]));
        save_plan_as(root.path(), &plan, true).unwrap();
        let mut ticket = InitialSetupAuthorization::capture_at(root.path()).unwrap();
        assert_eq!(
            ticket
                .offer_reading(root.path(), || Ok(Some(offered_vault.clone())))
                .unwrap(),
            plan
        );
        let result = observe_mutation_at(root.path(), None, Err("busy".into()), |root| {
            assert!(HeldLock::try_take(root)?.is_none());
            if join {
                ticket.persist_join_with(
                    root,
                    &plan,
                    || Ok(Some(joined_since.clone())),
                    AuthorizationWrite::persist,
                    observe,
                )
            } else {
                ticket.persist_with(
                    root,
                    &plan,
                    Admission::Automatic,
                    AuthorizationWrite::persist,
                    observe,
                )
            }
        });
        assert_eq!(result.result.unwrap(), Some(1));
        let admitted = progress(root.path()).unwrap().unwrap().admitted_vault;
        assert_eq!(
            admitted,
            AdmittedVault::Configured(if join {
                joined_since.identity()
            } else {
                offered_vault.identity()
            })
        );
        assert_eq!(admitted.stands_in(&plan, &joined_since), !join);
        assert!(admitted.stands_in(&plan, &after_admission));
        assert_eq!(saved_plan_provisional(root.path()).unwrap(), !join);
    }
}

#[test]
fn a_startup_join_keeps_its_authority_across_admission_write_retries() {
    let root = aterm_tempfile::tempdir().unwrap();
    let offered = vault(root.path(), "offered");
    let joined = vault(root.path(), "joined");
    let plan = offered.present(selected(&[Setting::Secrets]));
    save_plan_as(root.path(), &plan, true).unwrap();
    let mut ticket = InitialSetupAuthorization::capture_at(root.path()).unwrap();
    ticket
        .offer_reading(root.path(), || Ok(Some(offered)))
        .unwrap();
    let mut writes = 0;
    let failed = observe_mutation_at(root.path(), None, Err("busy".into()), |root| {
        ticket.persist_join_with(
            root,
            &plan,
            || Ok(Some(joined.clone())),
            |step, root| {
                step.persist(root)?;
                writes += 1;
                if writes == 2 {
                    Err("injected staged admission flush failure".into())
                } else {
                    Ok(())
                }
            },
            observe,
        )
    });
    assert!(failed.result.is_err());
    assert!(ticket.pending());
    let recovered = observe_mutation_at(root.path(), None, Err("busy".into()), |root| {
        ticket.persist_join_with(
            root,
            &plan,
            || panic!("an owned retry cannot bind a later vault"),
            AuthorizationWrite::persist,
            observe,
        )
    });
    assert_eq!(recovered.result.unwrap(), Some(1));
    let authority = progress(root.path()).unwrap().unwrap().admitted_vault;
    assert_eq!(authority, AdmittedVault::Configured(joined.identity()));
    assert!(!authority.stands_in(&plan, &joined));
    assert!(authority.stands_in(&plan, &vault(root.path(), "later")));
}

#[test]
fn a_startup_join_reopens_vault_stages_of_the_same_staged_plan() {
    for join in [false, true] {
        let root = aterm_tempfile::tempdir().unwrap();
        let current = vault(root.path(), "offered");
        let plan = current.present(selected(&[
            Setting::Desktop,
            Setting::Secrets,
            Setting::Repositories,
        ]));
        let revision = authorize_at(root.path(), &plan).unwrap().unwrap();
        let mut staged = progress(root.path()).unwrap().unwrap();
        staged.admitted = false;
        staged.done = Stage::Workstation.bit() | Stage::Secrets.bit() | Stage::Repositories.bit();
        save_progress(root.path(), staged).unwrap();
        save_workstation_decision(root.path(), true).unwrap();
        let mut ticket = InitialSetupAuthorization::capture_at(root.path()).unwrap();
        ticket
            .offer_reading(root.path(), || Ok(Some(current.clone())))
            .unwrap();
        let result = observe_mutation_at(root.path(), None, Err("busy".into()), |root| {
            if join {
                ticket.persist_join_with(
                    root,
                    &plan,
                    || Ok(Some(current)),
                    AuthorizationWrite::persist,
                    observe,
                )
            } else {
                ticket.persist_with(
                    root,
                    &plan,
                    Admission::Automatic,
                    AuthorizationWrite::persist,
                    observe,
                )
            }
        });
        assert_eq!(result.result.unwrap(), Some(revision + 1));
        let after = progress(root.path()).unwrap().unwrap();
        assert!(after.admitted && !after.completed);
        assert_eq!(
            after.done,
            if join {
                Stage::Workstation.bit()
            } else {
                staged.done
            }
        );
    }
}

#[test]
fn a_startup_join_reads_no_vault_for_a_retired_doctor_review() {
    for stopped in [false, true] {
        let root = aterm_tempfile::tempdir().unwrap();
        let offered = vault(root.path(), "offered");
        let plan = offered.present(selected(&[Setting::Secrets]));
        save_plan_as(root.path(), &plan, true).unwrap();
        let mut ticket = InitialSetupAuthorization::capture_at(root.path()).unwrap();
        ticket
            .offer_reading(root.path(), || Ok(Some(offered)))
            .unwrap();
        if stopped {
            write_json(
                &root.path().join("first-run.json"),
                &json!({"profile":PROFILE,"state":"restored"}),
            )
            .unwrap();
        } else {
            restart_at(root.path(), &plan).unwrap();
        }
        let before = observation_at(root.path()).unwrap();
        let result = observe_mutation_at(root.path(), None, Err("busy".into()), |root| {
            ticket.persist_join_with(
                root,
                &plan,
                || panic!("a stale review cannot read or bind the current vault"),
                |_, _| panic!("a stale review cannot write"),
                observe,
            )
        });
        assert_eq!(result.result.unwrap(), None);
        assert!(!ticket.pending());
        assert_eq!(observation_at(root.path()).unwrap(), before);
    }
}

/// The first-launch offer answered with a typed Join while this Mac's vault configuration
/// cannot be read (a root-owned `source.toml`): the Join's own read fails, and holding the
/// answer back for it would hold aterm's own permissions back too, trying the same answer
/// again and again. It is admitted at once as the person's choice of groups, with no vault
/// recorded and nothing joined: Permissions and every other group run, the Secrets step
/// waits with its calm note, and a vault the configuration names once it reads is left in
/// place.
#[test]
fn a_startup_join_over_an_unreadable_vault_admits_the_answer_without_the_join() {
    let root = aterm_tempfile::tempdir().unwrap();
    let mut ticket = InitialSetupAuthorization::capture_at(root.path()).unwrap();
    let offered = ticket
        .offer_reading(root.path(), || Err("source.toml: Permission denied".into()))
        .unwrap();
    assert!(offered.selection.contains(Setting::Permissions));
    let mut joined = offered.clone();
    joined.selection.set(Setting::Secrets, true);
    joined.vault_url = "https://typed.example/v1/typed".into();
    let tried = observe_mutation_at(root.path(), None, Err("busy".into()), |root| {
        ticket.persist_join_with(
            root,
            &joined,
            || Err("source.toml: Permission denied".into()),
            AuthorizationWrite::persist,
            observe,
        )
    });
    assert!(matches!(tried.result, Ok(Some(_))), "{:?}", tried.result);
    assert!(!ticket.pending(), "the startup tries nothing again");
    assert!(
        ticket.join_unread(),
        "System State says the vault was not joined"
    );
    let admitted = progress(root.path()).unwrap().unwrap();
    assert!(admitted.admitted, "Full Disk Access and the debugger run");
    assert_eq!(admitted.admitted_vault, AdmittedVault::None);
    assert_eq!(saved_plan(root.path()).unwrap(), Some(joined.clone()));
    assert!(
        admitted
            .admitted_vault
            .stands_in(&joined, &vault(root.path(), "configured")),
        "the vault this Mac is in stays as it is"
    );
}

#[test]
fn every_owned_write_boundary_recovers_the_same_revision_including_mismatched_prefixes() {
    for fault in 0..4 {
        for published in [false, true] {
            let root = aterm_tempfile::tempdir().unwrap();
            let previous = selected(&[Setting::Widgets, Setting::Secrets]);
            let original = authorize_at(root.path(), &previous).unwrap().unwrap();
            let mut chosen = selected(&[Setting::Desktop, Setting::Secrets]);
            chosen.vault_url = "https://vault.example/v1/reviewed".into();
            let mut ticket = InitialSetupAuthorization::capture_at(root.path()).unwrap();
            assert_eq!(ticket.revision(), original);
            let mut index = 0;
            let failed = attempt(
                &mut ticket,
                root.path(),
                &chosen,
                |step, root| {
                    let current = index;
                    index += 1;
                    if current == fault {
                        if published {
                            step.persist(root)?;
                        }
                        return Err("injected namespace flush failure".into());
                    }
                    step.persist(root)
                },
                observe,
            );
            assert!(failed.result.is_err());
            assert!(ticket.pending());
            let after_error = observation_at(root.path()).unwrap();
            let writes = std::cell::Cell::new(0);
            let recovered = attempt(
                &mut ticket,
                root.path(),
                &chosen,
                |step, root| {
                    writes.set(writes.get() + 1);
                    step.persist(root)
                },
                observe,
            );
            assert_eq!(recovered.result.unwrap(), Some(original + 1));
            assert!(!ticket.pending());
            assert_eq!(revision_at(root.path()).unwrap(), original + 1);
            assert_eq!(saved_plan(root.path()).unwrap(), Some(chosen));
            assert!(progress(root.path()).unwrap().unwrap().admitted);
            if fault == 3 && published {
                assert_eq!(writes.get(), 0);
                assert_eq!(observation_at(root.path()).unwrap(), after_error);
            }
        }
    }
}

#[test]
fn repeated_failures_keep_the_bound_choice_and_same_plan_stage_history() {
    let root = aterm_tempfile::tempdir().unwrap();
    let plan = selected(&[Setting::Desktop, Setting::Spotlight, Setting::Secrets]);
    let original = authorize_at(root.path(), &plan).unwrap().unwrap();
    let mut history = progress(root.path()).unwrap().unwrap();
    history.done = Stage::Spotlight.bit();
    save_progress(root.path(), history).unwrap();
    let mut ticket = InitialSetupAuthorization::capture_at(root.path()).unwrap();
    for published in [false, true] {
        let mut index = 0;
        assert!(
            attempt(
                &mut ticket,
                root.path(),
                &plan,
                |step, root| {
                    if index == 1 {
                        if published {
                            step.persist(root)?;
                        }
                        return Err("injected repeated staged save failure".into());
                    }
                    index += 1;
                    step.persist(root)
                },
                observe
            )
            .result
            .is_err()
        );
        assert!(ticket.pending());
    }
    let owned = observation_at(root.path()).unwrap();
    assert!(
        attempt(
            &mut ticket,
            root.path(),
            &plan,
            |_, _| panic!("read failure cannot write"),
            |_| Err("injected owned read failure".into())
        )
        .result
        .is_err()
    );
    assert!(ticket.pending());
    assert_eq!(observation_at(root.path()).unwrap(), owned);
    assert_eq!(
        ticket.persist_at(root.path(), &plan).result.unwrap(),
        Some(original + 1)
    );
    assert_eq!(
        progress(root.path()).unwrap().unwrap().done,
        Stage::Spotlight.bit()
    );
}

#[test]
fn same_plan_admission_keeps_an_explicit_preference_scope_through_a_failed_write() {
    for published in [false, true] {
        let root = aterm_tempfile::tempdir().unwrap();
        let plan = selected(&[Setting::Desktop, Setting::Dock]);
        let first = authorize_at(root.path(), &plan).unwrap().unwrap();
        let mut previous = progress(root.path()).unwrap().unwrap();
        let mut only = Selection::none();
        only.set(Setting::Desktop, true);
        previous.only = Some(only);
        save_progress(root.path(), previous).unwrap();
        let mut ticket = InitialSetupAuthorization::capture_at(root.path()).unwrap();
        let mut index = 0;
        let failed = attempt(
            &mut ticket,
            root.path(),
            &plan,
            |step, root| {
                let current = index;
                index += 1;
                if current == 1 {
                    if published {
                        step.persist(root)?;
                    }
                    return Err("injected progress publication failure".into());
                }
                step.persist(root)
            },
            observe,
        );
        assert!(failed.result.is_err());
        assert_eq!(
            ticket.persist_at(root.path(), &plan).result.unwrap(),
            Some(first + 1)
        );
        assert_eq!(progress(root.path()).unwrap().unwrap().only, Some(only));
    }
}

#[test]
fn a_restored_startup_keeps_ownership_across_its_workstation_write_error() {
    for (restored, different, published) in [
        (false, false, false),
        (false, false, true),
        (true, false, false),
        (true, false, true),
        (true, true, false),
        (true, true, true),
    ] {
        let (root, mut plan) = if restored {
            super::super::tests::staged_restored_choice()
        } else {
            let root = aterm_tempfile::tempdir().unwrap();
            let plan = Plan::default();
            authorize_at(root.path(), &plan).unwrap();
            (root, plan)
        };
        if different {
            plan.selection.set(Setting::Spotlight, false);
        }
        // A different plan admitted over a Restore's record keeps it, measured from that
        // plan ([`admitted_keeps`]).
        let kept = decision_record(root.path())
            .unwrap()
            .1
            .map(|restored| Restored {
                selection: Some(plan.selection),
                ..restored
            });
        let original = revision_at(root.path()).unwrap();
        let mut ticket = InitialSetupAuthorization::capture_at(root.path()).unwrap();
        let failed = attempt(
            &mut ticket,
            root.path(),
            &plan,
            |step, root| {
                if matches!(step, AuthorizationWrite::Workstation(..)) {
                    if published {
                        step.persist(root)?;
                    }
                    return Err("injected workstation decision failure".into());
                }
                step.persist(root)
            },
            observe,
        );
        assert!(failed.result.is_err());
        assert!(ticket.pending());
        let admitted = ticket.persist_at(root.path(), &plan).result.unwrap();
        assert_eq!(
            admitted,
            Some(original + 1),
            "restored={restored}, different={different}, published={published}: this is still the ticket's own publication"
        );
        assert_eq!(decision_record(root.path()).unwrap().1, kept);
    }
}

#[test]
fn empty_admission_is_confirmed_only_from_this_tickets_final_publication() {
    for replace in [false, true] {
        let root = aterm_tempfile::tempdir().unwrap();
        let plan = selected(&[]);
        let mut ticket = InitialSetupAuthorization::capture_at(root.path()).unwrap();
        let mut index = 0;
        assert!(
            attempt(
                &mut ticket,
                root.path(),
                &plan,
                |step, root| {
                    step.persist(root)?;
                    index += 1;
                    if index == 4 {
                        Err("injected final publication flush failure".into())
                    } else {
                        Ok(())
                    }
                },
                observe
            )
            .result
            .is_err()
        );
        assert!(ticket.pending());
        let owned = observation_at(root.path()).unwrap();
        if replace {
            restart_at(root.path(), &plan).unwrap();
        }
        let recovered = attempt(
            &mut ticket,
            root.path(),
            &plan,
            |_, _| panic!("confirmation cannot rewrite admission"),
            observe,
        );
        assert_eq!(
            recovered.result.unwrap(),
            if replace { None } else { Some(1) }
        );
        assert!(!ticket.pending());
        if !replace {
            assert_eq!(observation_at(root.path()).unwrap(), owned);
            assert!(admission_at(root.path()).unwrap().unwrap().completed);
        }
    }
}

#[test]
fn stopped_or_replaced_publications_end_bound_and_partially_written_tickets() {
    for started in [false, true] {
        for replacement in 0..5 {
            let root = aterm_tempfile::tempdir().unwrap();
            let plan = selected(&[Setting::Secrets]);
            authorize_at(root.path(), &plan).unwrap();
            let mut ticket = InitialSetupAuthorization::capture_at(root.path()).unwrap();
            if started {
                assert!(
                    attempt(
                        &mut ticket,
                        root.path(),
                        &plan,
                        |step, root| {
                            step.persist(root)?;
                            Err("injected own write failure".into())
                        },
                        observe
                    )
                    .result
                    .is_err()
                );
            }
            {
                let _writer = HeldLock::take(root.path()).unwrap();
                match replacement {
                    0 | 1 => {
                        write_json(&root.path().join("first-run.json"), &json!({"profile":PROFILE,"state":if replacement == 0 {"cancelled"} else {"restored"}})).unwrap();
                        let path = root.path().join("initial-setup.json");
                        std::fs::remove_file(&path).unwrap();
                        std::fs::create_dir(path).unwrap();
                    }
                    2 => {
                        restart_at(root.path(), &plan).unwrap();
                    }
                    3 => {
                        save_plan(root.path(), &plan).unwrap();
                    }
                    _ => {
                        aterm_machines::port::write_atomic(
                            &root.path().join("first-run.json"),
                            b"invalid decision",
                            0o600,
                        )
                        .unwrap();
                    }
                }
            }
            let plan_bytes = std::fs::read(root.path().join("automatic-plan.json")).unwrap();
            let decision = std::fs::read(root.path().join("first-run.json")).unwrap();
            let result = attempt(
                &mut ticket,
                root.path(),
                &plan,
                |_, _| panic!("a stale ticket cannot write"),
                observe,
            );
            assert_eq!(result.result.unwrap(), None);
            assert!(!ticket.pending());
            assert_eq!(
                std::fs::read(root.path().join("automatic-plan.json")).unwrap(),
                plan_bytes
            );
            assert_eq!(
                std::fs::read(root.path().join("first-run.json")).unwrap(),
                decision
            );
        }
    }
}

#[test]
fn lost_raw_publication_binding_never_adopts_a_later_readable_run() {
    let root = aterm_tempfile::tempdir().unwrap();
    let plan = selected(&[Setting::Secrets]);
    let mut ticket = InitialSetupAuthorization::capture_at(root.path()).unwrap();
    assert!(
        attempt(
            &mut ticket,
            root.path(),
            &plan,
            |step, root| {
                step.persist(root)?;
                let path = root.join("first-run.json");
                std::fs::create_dir(path).unwrap();
                Err("injected unpinnable publication".into())
            },
            observe
        )
        .result
        .is_err()
    );
    assert!(!ticket.pending());
    std::fs::remove_dir(root.path().join("first-run.json")).unwrap();
    restart_at(root.path(), &plan).unwrap();
    let current = observation_at(root.path()).unwrap();
    assert_eq!(ticket.persist_at(root.path(), &plan).result.unwrap(), None);
    assert_eq!(observation_at(root.path()).unwrap(), current);
}

#[test]
fn capture_uses_one_coherent_revision_and_leaves_absent_state_uncreated() {
    let root = aterm_tempfile::tempdir().unwrap();
    let absent = root.path().join("absent");
    let ticket = InitialSetupAuthorization::capture_at(&absent).unwrap();
    assert_eq!(ticket.revision(), 0);
    assert!(!absent.exists());
    let plan = selected(&[Setting::Secrets]);
    let revision = authorize_at(root.path(), &plan).unwrap().unwrap();
    let before = observation_at(root.path()).unwrap();
    let ticket = InitialSetupAuthorization::capture_at(root.path()).unwrap();
    assert_eq!(ticket.revision(), revision);
    assert_eq!(observation_at(root.path()).unwrap(), before);
    let mut replacement = plan;
    replacement.vault_url = "https://vault.example/v1/other".into();
    save_plan(root.path(), &replacement).unwrap();
    // A plan ahead of its run, as a choice cut short leaves it, still binds the run's
    // revision so the next launch can offer it; no pass runs on it meanwhile.
    assert_eq!(
        InitialSetupAuthorization::capture_at(root.path())
            .unwrap()
            .revision(),
        revision
    );
    assert!(revision_at(root.path()).is_err());
    std::fs::remove_file(root.path().join("automatic-plan.json")).unwrap();
    assert!(InitialSetupAuthorization::capture_at(root.path()).is_err());
}

fn admit(root: &Path, plan: &Plan, admission: Admission) -> Option<u64> {
    let mut ticket = InitialSetupAuthorization::capture_at(root).unwrap();
    observe_mutation_at(
        root,
        observation_at(root).ok(),
        Err("busy".into()),
        |root| ticket.persist_with(root, plan, admission, AuthorizationWrite::persist, observe),
    )
    .result
    .unwrap()
}

#[test]
fn fleet_expansion_widens_finished_automatic_plans_and_preserves_personal_plans() {
    let root = aterm_tempfile::tempdir().unwrap();
    let narrow = selected(&[Setting::Permissions]);
    let first = admit(root.path(), &narrow, Admission::Automatic).unwrap();
    assert!(saved_plan_provisional(root.path()).unwrap());
    let mut finished = progress(root.path()).unwrap().unwrap();
    finished.done |= Stage::Permissions.bit();
    finished.completed = true;
    save_progress(root.path(), finished).unwrap();
    assert!(records_finished(&Records::read(root.path()).unwrap()));

    let full = Plan::from(Selection::all());
    assert_eq!(admit(root.path(), &full, Admission::Automatic), None);
    let widened = admit(root.path(), &full, Admission::Upgrade).unwrap();
    assert_eq!(widened, first + 1);
    let after = progress(root.path()).unwrap().unwrap();
    assert!(after.admitted && !after.completed);
    assert_eq!(after.done, Stage::Permissions.bit());
    assert_eq!(saved_plan(root.path()).unwrap(), Some(full.clone()));
    assert!(saved_plan_provisional(root.path()).unwrap());

    // Fleet admission preserves a partial plan the person chose.
    let other = aterm_tempfile::tempdir().unwrap();
    admit(other.path(), &narrow, Admission::Person).unwrap();
    assert!(!saved_plan_provisional(other.path()).unwrap());
    let before = observation_at(other.path()).unwrap();
    assert_eq!(admit(other.path(), &full, Admission::Upgrade), None);
    assert_eq!(saved_plan(other.path()).unwrap(), Some(narrow));
    assert_eq!(observation_at(other.path()).unwrap(), before);
}

#[test]
fn a_relaunch_can_recover_an_upgrade_after_only_its_plan_was_published() {
    let root = aterm_tempfile::tempdir().unwrap();
    let narrow = selected(&[Setting::Permissions]);
    let first = admit(root.path(), &narrow, Admission::Automatic).unwrap();
    let mut finished = progress(root.path()).unwrap().unwrap();
    finished.done = Stage::Permissions.bit();
    finished.completed = true;
    save_progress(root.path(), finished).unwrap();
    let full = Plan::default();
    let mut ticket = InitialSetupAuthorization::capture_at(root.path()).unwrap();
    let mut writes = 0;
    let failed = observe_mutation_at(root.path(), None, Err("busy".into()), |root| {
        ticket.persist_with(
            root,
            &full,
            Admission::Upgrade,
            |step, root| {
                writes += 1;
                if writes == 2 {
                    return Err("injected interruption before progress publication".into());
                }
                step.persist(root)
            },
            observe,
        )
    });
    assert!(failed.result.is_err());
    assert_eq!(saved_plan(root.path()).unwrap(), Some(full.clone()));
    drop(ticket);

    let mut relaunched = InitialSetupAuthorization::capture_at(root.path())
        .expect("an upgrade's atomic plan publication must leave a recoverable staged run");
    assert_eq!(relaunched.revision(), first + 1);
    let staged = progress(root.path()).unwrap().unwrap();
    assert!(!staged.admitted && !staged.completed);
    assert_eq!(staged.done, Stage::Permissions.bit());
    relaunched.offer_reading(root.path(), || Ok(None)).unwrap();
    assert_eq!(
        relaunched
            .persist_at_as(root.path(), &full, Admission::Automatic)
            .result
            .unwrap(),
        Some(first + 2)
    );
    assert_eq!(
        progress(root.path()).unwrap().unwrap().done,
        Stage::Permissions.bit()
    );
}

fn provisional_permissions(root: &Path, completed: bool) -> u64 {
    let narrow = selected(&[Setting::Permissions]);
    let revision = admit(root, &narrow, Admission::Automatic).unwrap();
    let mut prior = progress(root).unwrap().unwrap();
    prior.completed = completed;
    if completed {
        prior.done = Stage::Permissions.bit();
    }
    save_progress(root, prior).unwrap();
    revision
}

fn interrupt_upgrade(root: &Path, fault: usize, published: bool) -> InitialSetupAuthorization {
    let mut ticket = InitialSetupAuthorization::capture_at(root).unwrap();
    let mut index = 0;
    let failed = observe_mutation_at(root, None, Err("busy".into()), |root| {
        ticket.persist_with(
            root,
            &Plan::default(),
            Admission::Upgrade,
            |step, root| {
                let current = index;
                index += 1;
                if current == fault {
                    if published {
                        step.persist(root)?;
                    }
                    return Err("injected upgrade publication failure".into());
                }
                step.persist(root)
            },
            observe,
        )
    });
    assert!(failed.result.is_err());
    assert!(ticket.pending());
    ticket
}

#[test]
fn every_upgrade_prefix_recovers_with_its_owner_or_after_process_exit() {
    let full = Plan::default();
    for fault in 0..4 {
        for published in [false, true] {
            for relaunch in [false, true] {
                let root = aterm_tempfile::tempdir().unwrap();
                let first = provisional_permissions(root.path(), true);
                let mut ticket = interrupt_upgrade(root.path(), fault, published);
                let revision = if relaunch {
                    drop(ticket);
                    let mut fresh = InitialSetupAuthorization::capture_at(root.path()).unwrap();
                    if let Some(admission) = admission_at(root.path()).unwrap()
                        && admission.plan == full
                    {
                        assert!(!admission.completed);
                        admission.revision
                    } else {
                        let staged = saved_plan(root.path()).unwrap() == Some(full.clone());
                        assert_eq!(fresh.revision(), first + u64::from(staged));
                        fresh.offer_reading(root.path(), || Ok(None)).unwrap();
                        fresh
                            .persist_at_as(
                                root.path(),
                                &full,
                                if staged {
                                    Admission::Automatic
                                } else {
                                    Admission::Upgrade
                                },
                            )
                            .result
                            .unwrap()
                            .unwrap()
                    }
                } else {
                    ticket
                        .upgrade_at(root.path(), &full)
                        .result
                        .unwrap()
                        .unwrap()
                };
                let recovered = progress(root.path()).unwrap().unwrap();
                assert_eq!(recovered.revision, revision);
                let readmitted =
                    relaunch && (fault != 0 || published) && !(fault == 3 && published);
                assert_eq!(revision, first + 1 + u64::from(readmitted));
                assert!(recovered.admitted && !recovered.completed && !recovered.stopped);
                assert_eq!(recovered.done, Stage::Permissions.bit());
                assert_eq!(recovered.access_revision(), first);
                assert_eq!(decision_at(root.path()).unwrap(), Decision::Pending);
                assert_eq!(saved_plan(root.path()).unwrap(), Some(full.clone()));
                assert!(saved_plan_provisional(root.path()).unwrap());
            }
        }
    }
}

#[test]
fn stops_survive_every_upgrade_prefix_and_are_durable_before_the_request_is_removed() {
    for fault in 0..4 {
        for published in [false, true] {
            for target in [false, true] {
                let root = aterm_tempfile::tempdir().unwrap();
                let first = provisional_permissions(root.path(), false);
                let mut owner = interrupt_upgrade(root.path(), fault, published);
                let widened = saved_plan(root.path()).unwrap() == Some(Plan::default());
                if target && !widened {
                    continue;
                }
                request_stop_at(root.path(), first + u64::from(target)).unwrap();
                assert!(finished_at(root.path()).unwrap());
                let snapshot = snapshot_at(root.path(), Plan::default()).unwrap();
                assert!(snapshot.finished && !snapshot.permissions_allowed);
                assert!(snapshot.rows.iter().all(|row| row.status == "stopped"));
                let mut fresh = InitialSetupAuthorization::capture_at(root.path()).unwrap();
                assert_eq!(
                    fresh
                        .persist_at_as(root.path(), &Plan::default(), Admission::Automatic)
                        .result
                        .unwrap(),
                    None
                );
                assert_eq!(
                    owner
                        .upgrade_at(root.path(), &Plan::default())
                        .result
                        .unwrap(),
                    None
                );

                observe_mutation_at(root.path(), None, Err("busy".into()), |root| {
                    fold_stop_request(root)
                })
                .result
                .unwrap();
                assert_eq!(stop_request_at(root.path()), None);
                assert!(progress_file(root.path()).unwrap().unwrap().stopped);
                assert!(
                    finished_at(root.path()).unwrap(),
                    "Stop cannot disappear with its request"
                );
                assert!(snapshot_at(root.path(), Plan::default()).unwrap().finished);
            }
        }
    }
}

#[test]
fn an_old_stop_of_a_completed_predecessor_is_spent_and_a_later_run_is_independent() {
    let root = aterm_tempfile::tempdir().unwrap();
    let first = provisional_permissions(root.path(), true);
    let mut owner = interrupt_upgrade(root.path(), 1, false);
    request_stop_at(root.path(), first).unwrap();
    assert!(!finished_at(root.path()).unwrap());
    assert!(!progress(root.path()).unwrap().unwrap().stopped);
    assert_eq!(
        owner
            .upgrade_at(root.path(), &Plan::default())
            .result
            .unwrap(),
        Some(first + 1),
        "a completed predecessor's delayed Stop must not strand its owned upgrade"
    );
    restart_at(root.path(), &Plan::default()).unwrap();
    let next = progress(root.path()).unwrap().unwrap();
    assert!(next.admitted && !next.stopped);
    request_stop_at(root.path(), first + 1).unwrap();
    assert_eq!(
        progress(root.path()).unwrap().unwrap(),
        next,
        "a newer deliberate revision is not stopped by the upgrade marker"
    );
}

#[test]
fn irrelevant_stops_keep_the_owned_upgrade_across_every_publication_boundary() {
    for completed in [false, true] {
        for predecessor in [false, true] {
            if predecessor && !completed {
                continue;
            }
            for fault in 0..4 {
                for published in [false, true] {
                    let root = aterm_tempfile::tempdir().unwrap();
                    provisional_permissions(root.path(), completed);
                    let mut prior = progress(root.path()).unwrap().unwrap();
                    prior.revision = 7;
                    save_progress(root.path(), prior).unwrap();
                    let stopped = prior.revision - u64::from(!predecessor);
                    let mut owner = interrupt_upgrade(root.path(), fault, published);
                    request_stop_at(root.path(), stopped).unwrap();
                    let request = std::fs::read(root.path().join(STOP_REQUEST)).unwrap();
                    assert_eq!(
                        owner
                            .upgrade_at(root.path(), &Plan::default())
                            .result
                            .unwrap(),
                        Some(prior.revision + 1),
                        "completed={completed}, predecessor={predecessor}, fault={fault}, published={published}"
                    );
                    let recovered = progress(root.path()).unwrap().unwrap();
                    assert!(recovered.admitted && !recovered.stopped && !recovered.completed);
                    assert_eq!(recovered.revision, prior.revision + 1);
                    assert_eq!(recovered.done, prior.done);
                    assert_eq!(recovered.access_revision(), prior.access_revision());
                    assert_eq!(
                        std::fs::read(root.path().join(STOP_REQUEST)).unwrap(),
                        request
                    );
                    observe_mutation_at(root.path(), None, Err("busy".into()), fold_stop_request)
                        .result
                        .unwrap();
                    assert_eq!(stop_request_at(root.path()), None);
                    assert!(!progress(root.path()).unwrap().unwrap().stopped);
                }
            }
        }
    }
}

#[test]
fn current_future_and_unreadable_stops_still_retire_upgrade_recovery() {
    for fault in 0..4 {
        for published in [false, true] {
            for offset in [Some(1), Some(2), None] {
                let root = aterm_tempfile::tempdir().unwrap();
                let first = provisional_permissions(root.path(), true);
                let mut owner = interrupt_upgrade(root.path(), fault, published);
                if let Some(offset) = offset {
                    request_stop_at(root.path(), first + offset).unwrap();
                } else {
                    std::fs::write(root.path().join(STOP_REQUEST), b"unreadable").unwrap();
                }
                let request = std::fs::read(root.path().join(STOP_REQUEST)).unwrap();
                assert_eq!(
                    owner
                        .upgrade_at(root.path(), &Plan::default())
                        .result
                        .unwrap(),
                    None,
                );
                assert!(!owner.pending());
                assert_eq!(
                    std::fs::read(root.path().join(STOP_REQUEST)).unwrap(),
                    request
                );
            }
        }
    }
}

#[test]
fn a_stop_saved_during_or_after_the_upgrade_commit_holds_before_any_stage() {
    for completed in [false, true] {
        for after in [0, 3] {
            let root = aterm_tempfile::tempdir().unwrap();
            let first = provisional_permissions(root.path(), completed);
            let mut owner = InitialSetupAuthorization::capture_at(root.path()).unwrap();
            let mut index = 0;
            let admitted = observe_mutation_at(root.path(), None, Err("busy".into()), |root| {
                owner.persist_with(
                    root,
                    &Plan::default(),
                    Admission::Upgrade,
                    |step, root| {
                        step.persist(root)?;
                        if index == after {
                            request_stop_at(root, first)?;
                        }
                        index += 1;
                        Ok(())
                    },
                    observe,
                )
            })
            .result
            .unwrap();
            assert_eq!(admitted, Some(first + 1));
            assert_eq!(finished_at(root.path()).unwrap(), !completed);
            assert_eq!(progress(root.path()).unwrap().unwrap().stopped, !completed);
            // The real pass folds a request before consulting finished_at,
            // so verify that fold cannot erase the inherited Stop.
            observe_mutation_at(root.path(), None, Err("busy".into()), fold_stop_request)
                .result
                .unwrap();
            assert_eq!(stop_request_at(root.path()), None);
            assert_eq!(finished_at(root.path()).unwrap(), !completed);
        }
    }
}

#[test]
fn an_upgrade_with_no_previous_progress_is_readable_but_never_infers_admission() {
    let root = aterm_tempfile::tempdir().unwrap();
    let narrow = selected(&[Setting::Permissions]);
    save_plan_as(root.path(), &narrow, true).unwrap();
    save_workstation_authorization(root.path(), &narrow).unwrap();
    drop(interrupt_upgrade(root.path(), 1, false));
    assert_eq!(progress_file(root.path()).unwrap(), None);
    let pending = progress(root.path()).unwrap().unwrap();
    assert_eq!(pending.revision, 1);
    assert!(!pending.admitted && !pending.completed);
    assert_eq!(admission_at(root.path()).unwrap(), None);
    assert!(!finished_at(root.path()).unwrap());
    let mut fresh = InitialSetupAuthorization::capture_at(root.path()).unwrap();
    assert_eq!(fresh.revision(), 1);
    assert_eq!(
        fresh
            .persist_at_as(root.path(), &Plan::default(), Admission::Automatic)
            .result
            .unwrap(),
        Some(2)
    );
}

#[test]
fn a_fresh_same_plan_choice_keeps_the_bridge_until_its_own_progress_is_published() {
    for admission in [Admission::Automatic, Admission::Person] {
        for fault in 0..4 {
            for published in [false, true] {
                let root = aterm_tempfile::tempdir().unwrap();
                let first = provisional_permissions(root.path(), true);
                drop(interrupt_upgrade(root.path(), 1, false));
                let mut fresh = InitialSetupAuthorization::capture_at(root.path()).unwrap();
                let mut index = 0;
                let failed = observe_mutation_at(root.path(), None, Err("busy".into()), |root| {
                    fresh.persist_with(
                        root,
                        &Plan::default(),
                        admission,
                        |step, root| {
                            let current = index;
                            index += 1;
                            if current == fault {
                                if published {
                                    step.persist(root)?;
                                }
                                return Err("injected fresh choice publication failure".into());
                            }
                            step.persist(root)
                        },
                        observe,
                    )
                });
                assert!(failed.result.is_err());
                assert!(fresh.pending());
                assert!(InitialSetupAuthorization::capture_at(root.path()).is_ok());
                assert_eq!(
                    fresh
                        .persist_at_as(root.path(), &Plan::default(), admission)
                        .result
                        .unwrap(),
                    Some(first + 2)
                );
                assert_eq!(
                    progress(root.path()).unwrap().unwrap().done,
                    Stage::Permissions.bit()
                );
                assert_eq!(
                    saved_plan_provisional(root.path()).unwrap(),
                    admission == Admission::Automatic
                );
            }
        }
    }
}

#[test]
fn restore_cancel_and_replaced_publications_retire_upgrade_recovery() {
    for state in ["restored", "cancelled"] {
        let root = aterm_tempfile::tempdir().unwrap();
        provisional_permissions(root.path(), false);
        let mut owner = interrupt_upgrade(root.path(), 1, false);
        write_json(
            &root.path().join("first-run.json"),
            &json!({"profile":PROFILE,"state":state}),
        )
        .unwrap();
        let before = observation_at(root.path()).unwrap();
        assert!(finished_at(root.path()).unwrap());
        assert_eq!(
            owner
                .upgrade_at(root.path(), &Plan::default())
                .result
                .unwrap(),
            None
        );
        assert_eq!(observation_at(root.path()).unwrap(), before);
    }
    for same in [false, true] {
        let root = aterm_tempfile::tempdir().unwrap();
        provisional_permissions(root.path(), true);
        let mut owner = interrupt_upgrade(root.path(), 1, false);
        if same {
            let path = root.path().join("automatic-plan.json");
            let record: Value = aterm_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            write_json(&path, &record).unwrap();
        } else {
            restart_at(root.path(), &selected(&[Setting::Desktop])).unwrap();
        }
        let before = observation_at(root.path()).unwrap();
        assert_eq!(
            owner
                .upgrade_at(root.path(), &Plan::default())
                .result
                .unwrap(),
            None
        );
        assert_eq!(observation_at(root.path()).unwrap(), before);
    }
}

#[test]
fn malformed_upgrade_markers_and_unknown_predecessors_never_recover_a_mismatch() {
    for malformed in [false, true] {
        let root = aterm_tempfile::tempdir().unwrap();
        provisional_permissions(root.path(), true);
        drop(interrupt_upgrade(root.path(), 1, false));
        if malformed {
            let path = root.path().join("automatic-plan.json");
            let mut record: Value = aterm_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            record
                .as_object_mut()
                .unwrap()
                .get_mut("upgrade")
                .unwrap()
                .as_object_mut()
                .unwrap()
                .get_mut("pending")
                .unwrap()
                .as_object_mut()
                .unwrap()
                .insert("admitted".into(), Value::Bool(true));
            write_json(&path, &record).unwrap();
        } else {
            let mut unrelated = progress_file(root.path()).unwrap().unwrap();
            unrelated.revision += 10;
            save_progress(root.path(), unrelated).unwrap();
        }
        assert!(revision_at(root.path()).is_err());
        assert!(!matches!(admission_at(root.path()), Ok(Some(_))));
        // Independent, physically matching progress remains authoritative
        // even if an old marker is malformed or describes another run.
        let mut newer = Progress::for_plan(&Plan::default());
        newer.revision = 50;
        newer.admitted = true;
        save_progress(root.path(), newer).unwrap();
        assert_eq!(progress(root.path()).unwrap(), Some(newer));
        assert_eq!(
            InitialSetupAuthorization::capture_at(root.path())
                .unwrap()
                .revision(),
            50
        );
    }
}
