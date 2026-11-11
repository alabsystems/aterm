// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

use super::*;
use agh::git::{ControlRefChange, ControlRefTransaction, PUBLISHER_FENCE_REF, RELEASE_LEASE_REF};
use std::sync::{Mutex, OnceLock, Weak};

#[test]
fn interleaved_claim_cas_refuses_a_winner_after_either_listing() {
    for boundary in [1, 2] {
        let world = World::canonical("interleaved-claim");
        common::canonical_repos(&world.fake);
        let (remote, source, main, _) = incremental_source(&world);
        let name = "refs/heads/release-claim";
        let tree = host_git(&remote, &["rev-parse", "main^{tree}"]);
        let rival = host_git(
            &remote,
            &["commit-tree", &tree, "-p", &main, "-m", "rival holder"],
        );
        let origin = remote.clone();
        let winner = rival.clone();
        let slot = Arc::new(OnceLock::<Weak<Fx>>::new());
        let callback_slot = slot.clone();
        let reads = Mutex::new(0);
        let fx = Arc::new(
            Fx::simulated(live_start()).with_git_child_observer_for_tests(move |span| {
                let specs = callback_slot
                    .get()
                    .unwrap()
                    .upgrade()
                    .unwrap()
                    .children_run();
                if specs[span.ordinal].args.iter().any(|a| a == "ls-remote") {
                    let mut count = reads.lock().unwrap();
                    *count += 1;
                    if *count == boundary {
                        // Rival's actual expected-absent CAS wins while our admission is in flight.
                        host_git(&origin, &["update-ref", name, &winner, ABSENT]);
                    }
                }
            }),
        );
        assert!(slot.set(Arc::downgrade(&fx)).is_ok());
        let gh = world
            .client(git_env())
            .fx(fx)
            .git_remote_for_tests("fixture-dev/app", &remote)
            .build();
        let slug = common::slug("fixture-dev/app");
        let batch = push_batch(&[(name, ABSENT, &main)])
            .observing_main(&main)
            .unwrap();
        let error = gh
            .run_git(&GitRun::push_refs(&slug, &source, batch, false))
            .unwrap_err();
        assert_eq!(error.kind, Kind::Conflict, "boundary {boundary}: {error:?}");
        assert!(!matches!(error.sub, Sub::GitInventoryChanged { .. }));
        assert_eq!(host_git(&remote, &["rev-parse", name]), rival);
    }
}

#[test]
fn claim_receive_uncertainty_never_repeats_or_becomes_an_unsent_race() {
    use agh::journal::IntentStore as _;
    for mode in ["before", "sibling", "conflict", "timeout", "after"] {
        let world = World::canonical(&format!("claim-receive-{mode}"));
        common::canonical_repos(&world.fake);
        let (remote, source, main, _) = incremental_source(&world);
        let name = "refs/heads/release-claim";
        let batch = push_batch(&[(name, ABSENT, &main)])
            .observing_main(&main)
            .unwrap();
        let slug = common::slug("fixture-dev/app");
        let probe = world
            .client(git_env())
            .git_remote_for_tests("fixture-dev/app", &remote)
            .build();
        probe
            .run_git(&GitRun::push_refs(&slug, &source, batch.clone(), true))
            .unwrap();
        let spec = probe
            .fx()
            .children_run()
            .into_iter()
            .find(|s| s.args.iter().any(|a| a == "push"))
            .unwrap();
        let args = spec.args.into_iter().filter(|a| a != "--dry-run").collect();
        let origin = remote.clone();
        let sha = main.clone();
        let fx = Fx::simulated(live_start()).with_git_intercept_once_for_tests(
            spec.program,
            args,
            || {},
            move |result| {
                assert_eq!(result.as_ref().unwrap().code, Some(0));
                if mode == "timeout" {
                    *result = Err(agh::Error::could_not_run("receive timed out after writing")
                        .with_side_effect(SideEffect::Unknown));
                    return;
                }
                if mode != "after" {
                    host_git(&origin, &["update-ref", "-d", name]);
                }
                if matches!(mode, "sibling" | "conflict") {
                    let tree = host_git(&origin, &["rev-parse", "main^{tree}"]);
                    let next =
                        host_git(&origin, &["commit-tree", &tree, "-p", &sha, "-m", "rival"]);
                    host_git(
                        &origin,
                        &[
                            "update-ref",
                            if mode == "conflict" {
                                name
                            } else {
                                "refs/heads/main"
                            },
                            &next,
                        ],
                    );
                }
                let out = result.as_mut().unwrap();
                out.code = Some(1);
                out.stdout = format!("!\t{sha}:{name}\t[remote rejected] (failed)\n").into_bytes();
                out.stderr = b"error: failed to push some refs\n".to_vec();
            },
        );
        let gh = world
            .client(git_env())
            .fx(Arc::new(fx))
            .git_remote_for_tests("fixture-dev/app", &remote)
            .build();
        let permit = common::permit(
            &gh,
            &agh::policy::RouteRequest::write(agh::policy::RouteTarget::Repo(slug.clone())),
        );
        let journal = gh.journal(&slug).unwrap();
        let result = gh
            .refs(&slug)
            .push_atomic(&permit, &source, &batch, &journal)
            .unwrap()
            .into_result();
        assert_eq!(
            gh.fx()
                .children_run()
                .iter()
                .filter(|s| s.args.iter().any(|a| a == "push"))
                .count(),
            1,
            "{mode}: repeated receive"
        );
        if mode == "after" {
            result.unwrap();
            assert!(journal.open().unwrap().is_empty());
        } else {
            let error = result.unwrap_err();
            assert_eq!(error.side_effect, SideEffect::Unknown, "{mode}: {error:?}");
            assert!(!matches!(error.sub, Sub::GitInventoryChanged { .. }));
            assert_eq!(journal.open().unwrap().len(), 1);
        }
    }
}

#[test]
fn native_inventory_retry_marker_requires_unchanged_requested_refs_and_forward_main() {
    for mode in ["forward", "conflict", "rewind", "diverge", "sibling"] {
        let world = World::canonical(&format!("claim-inventory-{mode}"));
        common::canonical_repos(&world.fake);
        let (remote, source, owner, _) = incremental_source(&world);
        host_git(
            &source,
            &[
                "remote",
                "set-url",
                "origin",
                "https://github.com/fixture-dev/app.git",
            ],
        );
        let transaction = ControlRefTransaction::new(
            &owner,
            vec![ControlRefChange {
                reference: RELEASE_LEASE_REF.into(),
                before: None,
                after: Some(owner.clone()),
            }],
        )
        .unwrap();
        let slot = Arc::new(OnceLock::<Weak<Fx>>::new());
        let callback_slot = slot.clone();
        let reads = Arc::new(Mutex::new(0_usize));
        let callback_reads = reads.clone();
        let origin = remote.clone();
        let base = owner.clone();
        let fx = Arc::new(
            Fx::simulated(live_start()).with_git_child_observer_for_tests(move |span| {
                let specs = callback_slot
                    .get()
                    .unwrap()
                    .upgrade()
                    .unwrap()
                    .children_run();
                if !specs[span.ordinal]
                    .args
                    .iter()
                    .any(|arg| arg == "ls-remote")
                {
                    return;
                }
                let mut reads = callback_reads.lock().unwrap();
                *reads += 1;
                // Change the real bare fixture after BEFORE has been returned, so
                // the final listing sees it. No wall clock or concurrent thread.
                if *reads != 1 {
                    return;
                }
                let current = host_git(&origin, &["rev-parse", "main"]);
                let tree = host_git(&origin, &["rev-parse", &format!("{current}^{{tree}}")]);
                let next = match mode {
                    "rewind" => host_git(&origin, &["rev-parse", &format!("{base}^")]),
                    "diverge" => host_git(&origin, &["commit-tree", &tree, "-m", "unrelated"]),
                    _ => host_git(
                        &origin,
                        &[
                            "commit-tree",
                            &tree,
                            "-p",
                            &current,
                            "-m",
                            "concurrent land",
                        ],
                    ),
                };
                let reference = match mode {
                    "conflict" => RELEASE_LEASE_REF,
                    "sibling" => PUBLISHER_FENCE_REF,
                    _ => "refs/heads/main",
                };
                host_git(&origin, &["update-ref", reference, &next]);
            }),
        );
        assert!(slot.set(Arc::downgrade(&fx)).is_ok());
        let gh = world
            .client(git_env())
            .fx(fx)
            .git_remote_for_tests("fixture-dev/app", &remote)
            .build();
        let slug = common::slug("fixture-dev/app");
        let permit = common::permit(
            &gh,
            &agh::policy::RouteRequest::write(agh::policy::RouteTarget::Repo(slug.clone())),
        );
        let journal = gh.journal(&slug).unwrap();
        let result = gh
            .refs(&slug)
            .push_control(
                &permit,
                &source,
                &transaction,
                &journal,
                agh::git::GIT_BUDGET,
            )
            .unwrap()
            .into_result();
        {
            let error = result.unwrap_err();
            assert_eq!(
                error.kind,
                if mode == "conflict" {
                    Kind::Conflict
                } else {
                    Kind::CouldNotRun
                },
                "{mode}: {error:?}"
            );
            assert_eq!(error.side_effect, SideEffect::None);
            assert_eq!(*reads.lock().unwrap(), 2);
            assert_eq!(
                matches!(error.sub, Sub::GitInventoryChanged { .. }),
                matches!(mode, "forward" | "sibling")
            );
            assert!(
                !gh.fx()
                    .children_run()
                    .iter()
                    .any(|s| s.args.iter().any(|a| a == "push"))
            );
        }
    }
}
