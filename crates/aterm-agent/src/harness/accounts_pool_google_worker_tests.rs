// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Synthetic published grants exercise worker admission, never live authentication.

use super::super::{agy_signin, worker};
use super::*;

const EMAIL: &str = "person@example.com";
const NOW: i64 = 1_790_000_000;

struct World {
    _root: aterm_tempfile::TempDir,
    config: std::path::PathBuf,
    state: std::path::PathBuf,
    home: std::path::PathBuf,
    profile: std::path::PathBuf,
}

impl World {
    fn new(principal: Option<[u8; 32]>) -> Self {
        let root = aterm_tempfile::tempdir().unwrap();
        let config = root.path().join("config");
        let state = root.path().join("state");
        let home = root.path().join("home");
        for path in [&config, &state, &home] {
            aterm_secrets::fsx::ensure_private_dir(path).unwrap();
        }
        let profile = home.join(".aterm/logins/agy/published");
        aterm_secrets::fsx::ensure_private_dir(&profile).unwrap();
        let row = Row {
            label: "one".into(),
            email: Some(EMAIL.into()),
            dir: Some(profile.to_str().unwrap().into()),
            ..Default::default()
        };
        std::fs::write(
            config.join(Cli::Agy.accounts().roster_file),
            accounts::append_rows_for(Cli::Agy, "", std::slice::from_ref(&row)).unwrap(),
        )
        .unwrap();
        aterm_secrets::fsx::write_atomic(&state.join("google-identities.toml"), b"[[identities]]\nid='one'\nemail='person@example.com'\n[[bindings]]\nprovider='agy'\naccount='one'\nidentity='one'\nexpected_email='person@example.com'\n", 0o600).unwrap();
        let world = Self {
            _root: root,
            config,
            state,
            home,
            profile,
        };
        world
            .seat()
            .store(Cli::Agy)
            .transact_logins(NOW, &mut None, |state, logins| {
                let events = accounts::note_renewed(
                    state,
                    logins,
                    "one",
                    row.dir.as_deref(),
                    world.profile.to_str().unwrap(),
                    None,
                    NOW,
                );
                if let Some(principal) = principal {
                    accounts::note_google_identity(logins, "one", EMAIL, principal);
                }
                Ok(events)
            })
            .unwrap();
        world
    }

    fn seat(&self) -> Seat<'_> {
        Seat {
            config_dir: &self.config,
            state: &self.state,
            home: &self.home,
            now: NOW,
        }
    }

    fn starter(&self) -> worker::Starter {
        worker::Starter {
            exe: "/inherited/agy".into(),
            env: vec![
                ("HOME".into(), self.profile.to_str().unwrap().into()),
                ("GEMINI_API_KEY".into(), "synthetic-key".into()),
            ],
            args: Ok(vec!["--gemini_dir=/inherited/profile".into()]),
            unset: Vec::new(),
        }
    }

    /// What the store says of row `one`'s login as the roster binds it now.
    fn word(&self) -> accounts::Auth {
        let roster = self.seat().bound_roster(Cli::Agy).unwrap();
        let row = roster.row("one").unwrap();
        let (state, _) = self.seat().store(Cli::Agy).load(NOW + 2).unwrap();
        state.account("one").current_auth(row)
    }

    /// The ledger's `auth signed-out` rows of row `one`.
    fn signed_out_rows(&self) -> usize {
        self.seat()
            .store(Cli::Agy)
            .tail(64)
            .unwrap()
            .into_iter()
            .filter(|event| {
                event.kind == accounts::Kind::Auth
                    && event.account == "one"
                    && event.why == "signed-out"
            })
            .count()
    }

    fn prepare(&self) -> Result<Option<GooglePoolWorker>, String> {
        prepare_google_worker_with(&self.seat(), self.starter(), |row, profile| {
            Ok(agy_signin::published_fixture(
                &self.state,
                profile,
                &row.label,
                EMAIL,
            ))
        })
    }
}

#[test]
fn a_cancelled_app_preparation_never_reaches_native_google_account_work() {
    let world = World::new(Some([1; 32]));
    let before = world.seat().store(Cli::Agy).load(NOW).unwrap().0;
    let lifetime = super::super::account_work::Lifetime::default();
    let scope = lifetime.enter().unwrap();
    let pause = lifetime.pause().unwrap();
    assert!(
        prepare_google_worker_scoped(
            scope,
            &world.seat(),
            Path::new("/not-an-executable"),
            world.starter()
        )
        .is_err()
    );
    assert_eq!(world.seat().store(Cli::Agy).load(NOW).unwrap().0, before);
    assert!(
        pause
            .try_wait()
            .expect("all actual work has retired")
            .resume()
            .is_ok()
    );
}

#[test]
fn native_google_worker_rechecks_selection_after_its_local_check() {
    for change in 0..6 {
        let world = World::new(Some([1; 32]));
        let config = world.config.join("aterm.toml");
        aterm_uds::atomic_file::write(&config, ("[harness]\naccounts='wall'\n").as_ref(), 0o600)
            .unwrap();
        let result = prepare_google_worker_with(&world.seat(), world.starter(), |row, profile| {
            let grant = agy_signin::published_fixture(&world.state, profile, &row.label, EMAIL);
            match change {
                0 => {
                    aterm_uds::atomic_file::write(
                        &config,
                        ("[harness]\naccounts='off'\n").as_ref(),
                        0o600,
                    )
                    .unwrap();
                }
                1 | 2 | 5 => {
                    world
                        .seat()
                        .store(Cli::Agy)
                        .transact(NOW + 1, &mut None, |state| {
                            match change {
                                1 => {
                                    state
                                        .accounts
                                        .insert("other".into(), accounts::Account::default());
                                    state.active = Some("other".into());
                                    state.pinned = true;
                                }
                                2 => {
                                    state.prepared_accounts.insert("one".into());
                                }
                                5 => {
                                    state.active = Some("one".into());
                                }
                                _ => unreachable!(),
                            }
                            Ok(Vec::new())
                        })
                        .unwrap();
                }
                3 => {
                    aterm_uds::atomic_file::write(&config, ("[harness").as_ref(), 0o600).unwrap();
                }
                4 => {
                    world
                        .seat()
                        .store(Cli::Agy)
                        .request_pin("one", NOW + 1)
                        .unwrap();
                }
                _ => unreachable!(),
            }
            Ok(grant)
        });
        if change == 5 {
            assert!(
                result.unwrap().is_some(),
                "an automatic active-row observation is not a new pin"
            );
        } else {
            assert!(result.is_err(), "stale selection accepted change {change}");
        }
    }
}

#[test]
fn native_google_worker_checks_execution_authority_before_its_grant_check() {
    for preparation_only in [false, true] {
        let world = World::new(Some([1; 32]));
        world
            .seat()
            .store(Cli::Agy)
            .transact(NOW + 1, &mut None, |state| {
                if preparation_only {
                    state.prepared_accounts.insert("one".into());
                } else {
                    state
                        .accounts
                        .insert("other".into(), accounts::Account::default());
                    state.active = Some("other".into());
                    state.pinned = true;
                }
                Ok(Vec::new())
            })
            .unwrap();
        assert!(
            prepare_google_worker_with(&world.seat(), world.starter(), |_, _| panic!(
                "refused account reached its grant check"
            ))
            .is_err()
        );
    }
}

#[test]
fn native_google_worker_keeps_exact_profile_and_rechecks_before_admission() {
    for mutation in 0..2 {
        let world = World::new(Some([1; 32]));
        let guard = world.prepare().unwrap().unwrap();
        let starter = guard.starter().unwrap().clone();
        assert_eq!(starter.exe, Path::new("/synthetic/agy"));
        assert!(!starter.env.iter().any(|(name, _)| name == "GEMINI_API_KEY"));
        assert!(starter.unset.contains(&"GEMINI_API_KEY"));
        let mut argv = vec![starter.exe.to_str().unwrap().to_owned()];
        argv.extend(starter.args.as_ref().unwrap().iter().cloned());
        assert_eq!(
            super::super::upgrade_agy::config_dir(&argv, None, &world.home),
            Some(world.profile.join(".gemini/antigravity-cli"))
        );
        assert!(
            starter.env.iter().any(
                |(name, value)| name == "SSH_CONNECTION" && value == "127.0.0.1 1 127.0.0.1 22"
            )
        );
        let admitted = worker::Admitted::assumed(starter.clone());
        guard.check_admitted(&admitted).unwrap();
        let config = world.config.join("native-policy.toml");
        crate::test_config::write(&config, "").unwrap();
        super::super::native_keys::with_test_launcher(
            |_, _| panic!("verified profile reached shared-key selection"),
            || {
                let cli = worker::worker_cli(aterm_phase::Program::Agy).unwrap();
                let line = worker::line(
                    cli,
                    super::super::upgrade::Dialect::Zsh,
                    &admitted,
                    &worker::Fresh {
                        config: Some(&config),
                        ..Default::default()
                    },
                )
                .unwrap();
                assert!(!line.contains("native-run"));
                assert!(line.contains("/synthetic/agy"));
            },
        );
        let mut changed = starter;
        changed.env.push(("HOME".into(), "/different".into()));
        assert!(
            guard
                .check_admitted(&worker::Admitted::assumed(changed))
                .is_err()
        );
        if mutation == 0 {
            world
                .seat()
                .store(Cli::Agy)
                .transact_logins(NOW + 1, &mut None, |_, logins| {
                    accounts::note_proof(logins, "one", accounts::Proof::Unchecked);
                    Ok(Vec::new())
                })
                .unwrap();
        } else {
            // A grant agy rewrote is still its login; one that is gone signs the row out here.
            let grant = super::super::upgrade_agy::profile_grant(&world.profile);
            aterm_secrets::fsx::write_atomic(&grant, b"renewed by agy", 0o600).unwrap();
            assert!(guard.starter().is_ok());
            std::fs::remove_file(&grant).unwrap();
        }
        assert!(guard.starter().is_err());
    }
}

#[test]
fn native_google_worker_refuses_an_unproved_identity_or_ambiguous_home() {
    assert!(World::new(None).prepare().is_err());
    let world = World::new(Some([1; 32]));
    let mut starter = world.starter();
    starter
        .env
        .push(("HOME".into(), world.home.to_str().unwrap().into()));
    assert!(
        prepare_google_worker_with(&world.seat(), starter, |_, _| panic!(
            "ambiguous home reached its grant check"
        ))
        .is_err()
    );
    let mut orphan = world.starter();
    orphan.env[0].1 = world
        .home
        .join(".aterm/logins/agy/orphan")
        .to_str()
        .unwrap()
        .into();
    assert!(
        prepare_google_worker_with(&world.seat(), orphan, |_, _| panic!(
            "orphan reached its grant check"
        ))
        .is_err()
    );
}

#[test]
fn native_google_worker_renews_a_signed_out_row_once_in_new_home() {
    for scenario in 0..4 {
        let world = World::new(Some([1; 32]));
        let next = world.home.join(".aterm/logins/agy/replacement");
        aterm_secrets::fsx::ensure_private_dir(&next).unwrap();
        let checks = std::cell::Cell::new(0);
        let renewals = std::cell::Cell::new(0);
        let result = recover_google_worker_with(
            &world.seat(),
            world.starter(),
            |row, profile| {
                checks.set(checks.get() + 1);
                if checks.get() == 1 {
                    assert_eq!(profile, world.profile);
                    return Err(agy_signin::PublishedRefusal::SignedOut);
                }
                assert_eq!(profile, next);
                if scenario == 1 {
                    return Err(agy_signin::PublishedRefusal::SignedOut);
                }
                if scenario == 3 {
                    aterm_uds::atomic_file::write(
                        &(world.config.join("aterm.toml")),
                        ("[harness]\naccounts='off'\n").as_ref(),
                        0o600,
                    )
                    .unwrap();
                }
                Ok(agy_signin::published_fixture(
                    &world.state,
                    profile,
                    &row.label,
                    EMAIL,
                ))
            },
            |row, _| {
                renewals.set(renewals.get() + 1);
                world
                    .seat()
                    .store(Cli::Agy)
                    .transact_logins(NOW + 1, &mut None, |state, logins| {
                        let events = accounts::note_renewed(
                            state,
                            logins,
                            &row.label,
                            row.dir.as_deref(),
                            next.to_str().unwrap(),
                            None,
                            NOW + 1,
                        );
                        accounts::note_google_identity(
                            logins,
                            &row.label,
                            EMAIL,
                            [if scenario == 2 { 2 } else { 1 }; 32],
                        );
                        Ok(events)
                    })
                    .map_err(|error| error.to_string())?;
                Ok(())
            },
        );
        assert_eq!(checks.get(), if scenario == 2 { 1 } else { 2 });
        assert_eq!(renewals.get(), 1);
        // Only the renewed login a second check found signed out is on the row's record so.
        assert_eq!(
            world.word(),
            if scenario == 1 {
                accounts::Auth::SignedOut
            } else {
                accounts::Auth::Ok
            },
            "scenario {scenario}"
        );
        assert_eq!(world.signed_out_rows(), usize::from(scenario == 1));
        if scenario == 1 {
            assert!(
                matches!(result, Err(super::super::worker::Refused::SignedOut(_))),
                "a renewed row still signed out is a sign-out"
            );
        } else if scenario != 0 {
            assert!(
                matches!(result, Err(super::super::worker::Refused::Preparation(_))),
                "CONTROL: an account that changed under the renewal is no sign-out"
            );
        } else {
            let guard = result.unwrap().unwrap();
            let starter = guard.starter().unwrap();
            assert!(
                starter
                    .env
                    .iter()
                    .any(|(key, value)| key == "HOME" && Path::new(value) == next)
            );
            assert!(!starter.env.iter().any(|(key, _)| key == "GEMINI_API_KEY"));
        }
    }
}

#[test]
fn native_google_worker_renewal_is_not_a_general_failure_retry() {
    for change in 0..4 {
        let world = World::new(Some([1; 32]));
        let result = recover_google_worker_with(
            &world.seat(),
            world.starter(),
            |_, _| {
                if change == 0 {
                    return Err(agy_signin::PublishedRefusal::Refused("profile is held"));
                }
                if change == 1 {
                    aterm_uds::atomic_file::write(
                        &(world.config.join("aterm.toml")),
                        ("[harness]\naccounts='off'\n").as_ref(),
                        0o600,
                    )
                    .unwrap();
                } else if change == 2 {
                    world
                        .seat()
                        .store(Cli::Agy)
                        .transact_logins(NOW + 1, &mut None, |state, _| {
                            state.pinned = true;
                            state.active = Some("another".into());
                            Ok(Vec::new())
                        })
                        .unwrap();
                } else {
                    world
                        .seat()
                        .store(Cli::Agy)
                        .transact_logins(NOW + 1, &mut None, |_, logins| {
                            accounts::note_proof(logins, "one", accounts::Proof::Unchecked);
                            Ok(Vec::new())
                        })
                        .unwrap();
                }
                Err(agy_signin::PublishedRefusal::SignedOut)
            },
            |_, _| panic!("failure or changed selection reached authentication"),
        );
        assert!(result.is_err());
    }
}

#[test]
fn native_google_worker_failed_renewal_never_rechecks_or_launches() {
    let world = World::new(Some([1; 32]));
    let checks = std::cell::Cell::new(0);
    let result = recover_google_worker_with(
        &world.seat(),
        world.starter(),
        |_, _| {
            checks.set(checks.get() + 1);
            Err(agy_signin::PublishedRefusal::SignedOut)
        },
        |_, _| Err("staged login refused".into()),
    );
    assert!(result.is_err());
    assert_eq!(checks.get(), 1);
}

/// A LAUNCH THAT ENDS REFUSED ON A MISSING GRANT LEAVES ITS ROW SIGNED OUT ON THE RECORD, ONCE:
/// whatever the renewal it started came to (refused, held, deferred, a bot check), the grant the
/// check found missing is still missing, so the row's word, its ledger and the journal say so,
/// and a second launch writes nothing more.
/// CONTROL: a launch refused for a reason that is no sign-out (its profile is held) leaves the
/// store as it was; a launch whose renewal took is the test above.
#[test]
fn a_launch_refused_on_a_missing_grant_records_its_row_signed_out_once() {
    let world = World::new(Some([1; 32]));
    let before = world.seat().store(Cli::Agy).load(NOW).unwrap().0;
    let held = recover_google_worker_with(
        &world.seat(),
        world.starter(),
        |_, _| Err(agy_signin::PublishedRefusal::Refused("profile is held")),
        |_, _| panic!("a refusal that is no sign-out reached authentication"),
    );
    assert!(held.is_err());
    assert_eq!(
        world.seat().store(Cli::Agy).load(NOW).unwrap().0,
        before,
        "CONTROL: no sign-out, nothing written"
    );
    assert_eq!(world.word(), accounts::Auth::Ok);
    for launch in 0..2 {
        let result = recover_google_worker_with(
            &world.seat(),
            world.starter(),
            |_, _| Err(agy_signin::PublishedRefusal::SignedOut),
            |_, _| Err("staged login refused".into()),
        );
        assert!(result.is_err());
        assert_eq!(world.word(), accounts::Auth::SignedOut, "launch {launch}");
        assert_eq!(world.signed_out_rows(), 1, "launch {launch}");
    }
}

/// A launch on a row whose published grant is gone runs the real local check: it answers signed
/// out with no provider read on record, and the launch's one staged sign-in follows.
#[test]
fn native_google_worker_launch_on_a_missing_grant_signs_in_without_a_provider_read() {
    let world = World::new(Some([1; 32]));
    let program = world.home.join("agy");
    aterm_secrets::fsx::write_atomic(&program, b"never run", 0o600).unwrap();
    let renewals = std::cell::Cell::new(0);
    let (result, probes) = super::super::provider_read::captured(|| {
        recover_google_worker_with(
            &world.seat(),
            world.starter(),
            |row, profile| {
                agy_signin::check_google_published(
                    &program,
                    &world.state,
                    &row.label,
                    EMAIL,
                    profile,
                )
            },
            |_, _| {
                renewals.set(renewals.get() + 1);
                Err("staged login refused".into())
            },
        )
    });
    assert!(result.is_err());
    assert_eq!(
        renewals.get(),
        1,
        "a signed-out row is the launch's sign-in"
    );
    assert!(probes.is_empty());
}

/// ONLY A RENEWAL THAT LEAVES THE LOGIN SIGNED OUT READS AS A SIGN-OUT. A sign-in that failed or
/// wants a person, one onto the wrong account and one that renewed another account are a person's
/// to mend. One that could not run leaves it signed out too, but a later launch tries the renewal
/// again, so a dispatcher is not sent to ask a person to sign in. CONTROL: a renewal that took
/// refuses nothing.
#[test]
fn only_a_renewal_that_leaves_the_login_signed_out_reads_as_a_sign_out() {
    use super::super::cli::Renewal;
    use super::super::worker::Refused;
    assert_eq!(renewal_refused(Renewal::Renewed), Ok(()), "CONTROL");
    for result in [
        Renewal::AuthFailed,
        Renewal::WrongAccount,
        Renewal::OtherRenewed,
        Renewal::NeedsPerson,
    ] {
        let refused = renewal_refused(result);
        assert!(
            matches!(&refused, Err(Refused::SignedOut(why)) if why.contains(&format!("{result:?}"))),
            "{result:?}: {refused:?}"
        );
    }
    for result in [
        Renewal::Failed,
        Renewal::SigninHeld,
        Renewal::Deferred,
        Renewal::BotCheck,
    ] {
        let refused = renewal_refused(result);
        assert!(
            matches!(&refused, Err(Refused::Preparation(why)) if why.contains(&format!("{result:?}"))),
            "{result:?}: {refused:?}"
        );
    }
}
