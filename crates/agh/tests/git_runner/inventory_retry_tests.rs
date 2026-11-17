// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

use super::*;
use agh::git::{ControlRefChange, ControlRefTransaction, PUBLISHER_FENCE_REF, RELEASE_LEASE_REF};
use std::sync::{Mutex, OnceLock, Weak};

/// Git's atomic receiver puts expected-old failures on stderr while every
/// porcelain row says `atomic transaction failed`. The losing create wrote
/// nothing: its journal must close so the same create can later acquire.
#[test]
fn an_explicit_atomic_create_conflict_closes_the_intent_and_allows_later_acquisition() {
    use agh::journal::IntentStore as _;
    let world = World::canonical("atomic-create-conflict");
    common::canonical_repos(&world.fake);
    let (remote, source, main, _) = incremental_source(&world);
    let name = "refs/heads/release-claim";
    let tree = host_git(&remote, &["rev-parse", "main^{tree}"]);
    let rival = host_git(
        &remote,
        &["commit-tree", &tree, "-p", &main, "-m", "another claim"],
    );
    let slug = common::slug("fixture-dev/app");
    let batch = push_batch(&[(name, ABSENT, &main)]);
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
    let winner = rival.clone();
    let desired = main.clone();
    let fx = Fx::simulated(live_start()).with_git_intercept_once_for_tests(
        spec.program,
        args,
        move || {
            host_git(&origin, &["update-ref", name, &winner, ABSENT]);
        },
        move |result| {
            let out = result.as_mut().unwrap();
            assert_ne!(out.code, Some(0), "the real receive must reject our create");
            // Move the rejection from client's stale advertisement to the
            // equivalent server transaction diagnostic, without applying us.
            out.code = Some(1);
            out.stdout =
                format!("!\t{desired}:{name}\t[remote rejected] (atomic transaction failed)\n")
                    .into_bytes();
            out.stderr =
                format!("remote: error: cannot lock ref '{name}': reference already exists\n")
                    .into_bytes();
        },
    );
    let gh = world
        .client(git_env())
        .fx(Arc::new(fx))
        .git_remote_for_tests("fixture-dev/app", &remote)
        .build();
    let run = GitRun::push_refs(&slug, &source, batch, false);
    let error = journaled_push(&gh, &run).unwrap_err();
    assert_eq!(
        (error.kind, error.side_effect),
        (Kind::Conflict, SideEffect::None)
    );
    assert!(gh.journal(&slug).unwrap().open().unwrap().is_empty());
    assert_eq!(host_git(&remote, &["rev-parse", name]), rival);
    host_git(&remote, &["update-ref", "-d", name, &rival]);
    journaled_push(&gh, &run).unwrap();
    assert_eq!(host_git(&remote, &["rev-parse", name]), main);
    assert!(gh.journal(&slug).unwrap().open().unwrap().is_empty());
}

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
        let error =
            journaled_push(&gh, &GitRun::push_refs(&slug, &source, batch, false)).unwrap_err();
        assert_eq!(error.kind, Kind::Conflict, "boundary {boundary}: {error:?}");
        assert!(!matches!(error.sub, Sub::GitInventoryChanged { .. }));
        assert_eq!(host_git(&remote, &["rev-parse", name]), rival);
    }
}

#[test]
fn a_later_known_rejection_cannot_close_an_earlier_uncertain_receive() {
    use agh::journal::IntentStore as _;
    let world = World::canonical("uncertain-before-known-rejection");
    common::canonical_repos(&world.fake);
    let (remote, source, main, _) = incremental_source(&world);
    let name = "refs/heads/release-claim";
    let tree = host_git(&remote, &["rev-parse", "main^{tree}"]);
    let rival = host_git(
        &remote,
        &["commit-tree", &tree, "-p", &main, "-m", "later writer"],
    );
    let slug = common::slug("fixture-dev/app");
    let batch = push_batch(&[(name, ABSENT, &main)]);
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
    let later_origin = remote.clone();
    let later = rival.clone();
    let desired = main.clone();
    let slot = Arc::new(OnceLock::<Weak<Fx>>::new());
    let callback_slot = slot.clone();
    let reads = Mutex::new(0);
    let fx = Fx::simulated(live_start())
        .with_git_intercept_once_for_tests(
            spec.program,
            args,
            || {},
            move |result| {
                let out = result.as_mut().unwrap();
                assert_eq!(out.code, Some(0), "the first receive actually applied us");
                host_git(&origin, &["update-ref", "-d", name, &desired]);
                out.code = Some(1);
                out.stdout =
                    format!("!\t{desired}:{name}\t[remote rejected] (failed)\n").into_bytes();
                out.stderr = b"error: failed to push some refs\n".to_vec();
            },
        )
        .with_git_child_observer_for_tests(move |span| {
            let specs = callback_slot
                .get()
                .unwrap()
                .upgrade()
                .unwrap()
                .children_run();
            if specs[span.ordinal].args.iter().any(|a| a == "ls-remote") {
                let mut reads = reads.lock().unwrap();
                *reads += 1;
                if *reads == 3 {
                    // Recovery has read BEFORE after the first uncertain
                    // receive; a rival wins before our next push advertises.
                    host_git(&later_origin, &["update-ref", name, &later, ABSENT]);
                }
            }
        });
    let fx = Arc::new(fx);
    assert!(slot.set(Arc::downgrade(&fx)).is_ok());
    let gh = world
        .client(git_env())
        .fx(fx)
        .git_remote_for_tests("fixture-dev/app", &remote)
        .build();
    let error = journaled_push(&gh, &GitRun::push_refs(&slug, &source, batch, false)).unwrap_err();
    assert_eq!(
        (error.kind, error.side_effect),
        (Kind::OutcomeUnknown, SideEffect::Unknown)
    );
    assert_eq!(gh.journal(&slug).unwrap().open().unwrap().len(), 1);
    assert_eq!(host_git(&remote, &["rev-parse", name]), rival);
    assert_eq!(
        gh.fx()
            .children_run()
            .iter()
            .filter(|s| s.args.iter().any(|a| a == "push"))
            .count(),
        2
    );
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

/// Simulated effects whose Git observer changes the real bare fixture once,
/// right after the `at`-th remote listing returned: 1 is between the listing a
/// write prepares against and its final listing, 2 between that final listing
/// and receive. No wall clock or concurrent thread. Counts every listing.
fn after_listing(
    at: usize,
    change: impl Fn() + Send + Sync + 'static,
) -> (Arc<Fx>, Arc<Mutex<usize>>) {
    let slot = Arc::new(OnceLock::<Weak<Fx>>::new());
    let callback_slot = slot.clone();
    let reads = Arc::new(Mutex::new(0_usize));
    let callback_reads = reads.clone();
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
            if *reads == at {
                change();
            }
        }),
    );
    assert!(slot.set(Arc::downgrade(&fx)).is_ok());
    (fx, reads)
}

/// A concurrent writer's change to the bare `origin`: main advanced
/// fast-forward (`forward`, `late`), rewound or diverged; an unrelated branch
/// created; or the write's own `requested` ref or a control `sibling` moved.
fn concurrent_change(origin: &Path, mode: &str, requested: &str, sibling: &str) {
    let current = host_git(origin, &["rev-parse", "main"]);
    let tree = host_git(origin, &["rev-parse", &format!("{current}^{{tree}}")]);
    let next = match mode {
        "rewind" => host_git(origin, &["rev-parse", &format!("{current}^")]),
        "diverge" => host_git(origin, &["commit-tree", &tree, "-m", "unrelated"]),
        _ => host_git(
            origin,
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
        "conflict" => requested,
        "sibling" => sibling,
        "unrelated" => "refs/heads/unrelated",
        _ => "refs/heads/main",
    };
    host_git(origin, &["update-ref", reference, &next]);
}

/// A control transaction judges its guarded refs one at a time: main is an
/// object witness, so a fast-forward of it (or an unrelated ref) between the
/// listing it prepared against and receive does not stop the write. A moved
/// requested ref or control sibling conflicts; a rewound or diverged main
/// refuses unsent. None of them is a main-bound caller's retry marker.
/// NEGATIVE CONTROL: the whole-listing comparison refused `forward` with "the
/// remote inventory changed outside the requested refs", as it refused the
/// publisher-fence release of the 0.204.0 cut twice.
#[test]
fn native_control_write_goes_ahead_over_a_forward_main_and_refuses_a_moved_guard() {
    use agh::journal::IntentStore as _;
    for mode in [
        "forward",
        "late",
        "unrelated",
        "conflict",
        "sibling",
        "rewind",
        "diverge",
    ] {
        let world = World::canonical(&format!("control-inventory-{mode}"));
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
        let origin = remote.clone();
        let (fx, reads) = after_listing(usize::from(mode == "late") + 1, move || {
            concurrent_change(&origin, mode, RELEASE_LEASE_REF, PUBLISHER_FENCE_REF);
        });
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
            .and_then(agh::write::Outcome::into_result);
        assert_eq!(*reads.lock().unwrap(), 2, "{mode}");
        let pushes = gh
            .fx()
            .children_run()
            .iter()
            .filter(|s| s.args.iter().any(|a| a == "push"))
            .count();
        if matches!(mode, "forward" | "late" | "unrelated") {
            result.unwrap_or_else(|error| panic!("{mode}: {error:?}"));
            assert_eq!(pushes, 1, "{mode}");
            assert_eq!(host_git(&remote, &["rev-parse", RELEASE_LEASE_REF]), owner);
            assert_eq!(
                host_git(&remote, &["rev-parse", "main"]) == owner,
                mode == "unrelated",
                "{mode}: the concurrent main stands"
            );
            assert!(journal.open().unwrap().is_empty(), "{mode}");
            continue;
        }
        let error = result.unwrap_err();
        let kind = if matches!(mode, "conflict" | "sibling") {
            Kind::Conflict
        } else {
            Kind::CouldNotRun
        };
        assert_eq!(
            (error.kind, error.side_effect),
            (kind, SideEffect::None),
            "{mode}: {error:?}"
        );
        assert!(!matches!(error.sub, Sub::GitInventoryChanged { .. }));
        assert!(
            journal.read().unwrap().0.is_empty(),
            "{mode}: preparation must not begin or settle an intent"
        );
        assert_eq!(pushes, 0, "{mode}");
    }
}

/// An ordinary atomic write judges its guarded refs the same way. A tag create
/// or a branch advance goes ahead when main advances fast-forward, or an
/// unrelated ref moves, between the listing it prepared against and receive.
/// `stale-tag` is the 0.204.0 cut's tag push: the source lacks the remote main
/// tip, so preparation strictly imports the tag's closure before the final
/// listing. A batch whose caller bound its policy to main (`claim`) keeps its
/// unsent retry marker when main advanced before its final listing. A moved
/// requested ref conflicts and a rewound main refuses unsent, in every shape.
/// NEGATIVE CONTROL: the whole-listing comparison refused `forward` in the
/// `tag`, `stale-tag` and `branch` shapes; that refusal ended the 0.204.0 cut.
#[test]
fn native_atomic_write_goes_ahead_over_a_forward_main_and_refuses_a_moved_request() {
    use agh::journal::IntentStore as _;
    for shape in ["tag", "stale-tag", "branch", "claim"] {
        for mode in ["forward", "late", "unrelated", "conflict", "rewind"] {
            let world = World::canonical(&format!("atomic-inventory-{shape}-{mode}"));
            common::canonical_repos(&world.fake);
            let (remote, source, published, unpublished) = incremental_source(&world);
            if shape == "stale-tag" {
                let tree = host_git(&remote, &["rev-parse", "main^{tree}"]);
                let landed = host_git(
                    &remote,
                    &[
                        "commit-tree",
                        &tree,
                        "-p",
                        &published,
                        "-m",
                        "landed elsewhere",
                    ],
                );
                host_git(&remote, &["update-ref", "refs/heads/main", &landed]);
            }
            if shape == "branch" {
                host_git(&remote, &["update-ref", "refs/heads/topic", &published]);
            }
            let (name, before, after) = match shape {
                "branch" => ("refs/heads/topic", published.as_str(), unpublished.as_str()),
                "claim" => ("refs/heads/release-claim", ABSENT, published.as_str()),
                _ => ("refs/tags/v1.0.0", ABSENT, published.as_str()),
            };
            let main = host_git(&remote, &["rev-parse", "main"]);
            let mut batch = push_batch(&[(name, before, after)]);
            if shape == "claim" {
                batch = batch.observing_main(&main).unwrap();
            }
            let origin = remote.clone();
            let requested = name.to_owned();
            let (fx, reads) = after_listing(usize::from(mode == "late") + 1, move || {
                concurrent_change(&origin, mode, &requested, &requested);
            });
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
                .push_atomic(&permit, &source, &batch, &journal)
                .and_then(agh::write::Outcome::into_result);
            let case = format!("{shape}/{mode}");
            assert_eq!(*reads.lock().unwrap(), 2, "{case}");
            let pushes = gh
                .fx()
                .children_run()
                .iter()
                .filter(|s| s.args.iter().any(|a| a == "push"))
                .count();
            let moved = host_git(&remote, &["rev-parse", "main"]);
            let bound = shape == "claim" && mode == "forward";
            if matches!(mode, "forward" | "late" | "unrelated") && !bound {
                result.unwrap_or_else(|error| panic!("{case}: {error:?}"));
                assert_eq!(pushes, 1, "{case}");
                assert_eq!(host_git(&remote, &["rev-parse", name]), after, "{case}");
                assert_eq!(moved == main, mode == "unrelated", "{case}");
                assert!(journal.open().unwrap().is_empty(), "{case}");
                continue;
            }
            let error = result.unwrap_err();
            let kind = if mode == "conflict" {
                Kind::Conflict
            } else {
                Kind::CouldNotRun
            };
            assert_eq!(
                (error.kind, error.side_effect),
                (kind, SideEffect::None),
                "{case}: {error:?}"
            );
            match &error.sub {
                Sub::GitInventoryChanged { main } => {
                    assert!(bound, "{case}: only a main-bound write retries");
                    assert_eq!(main, &moved, "{case}");
                }
                _ => assert!(!bound, "{case}: {error:?}"),
            }
            assert!(
                journal.read().unwrap().0.is_empty(),
                "{case}: preparation must not begin or settle an intent"
            );
            assert_eq!(pushes, 0, "{case}");
        }
    }
}

/// After an uncertain receive, a write not bound to main whose requested ref
/// still holds its BEFORE repeats the same expected-old push even though main
/// advanced fast-forward meanwhile. A requested ref found at neither its BEFORE
/// nor its AFTER may carry our receive under a later write, so its outcome is
/// unknown, the intent stays open and nothing is repeated behind it; it is
/// never a never-sent conflict. NEGATIVE CONTROL: the
/// whole-listing comparison ended `forward` after one receive with "the remote
/// inventory changed outside the requested refs".
#[test]
fn unbound_receive_uncertainty_repeats_over_a_forward_main() {
    use agh::journal::IntentStore as _;
    for mode in ["forward", "conflict", "rewind", "diverge", "gone"] {
        let world = World::canonical(&format!("unbound-receive-{mode}"));
        common::canonical_repos(&world.fake);
        let (remote, source, main, _) = incremental_source(&world);
        let name = "refs/tags/v1.0.0";
        let batch = push_batch(&[(name, ABSENT, &main)]);
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
                // The receive is lost, and a concurrent land advances main.
                host_git(&origin, &["update-ref", "-d", name]);
                if mode == "gone" {
                    host_git(&origin, &["update-ref", "-d", "refs/heads/main"]);
                } else {
                    concurrent_change(&origin, mode, name, name);
                }
                if mode == "conflict" {
                    concurrent_change(&origin, "forward", name, name);
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
            .and_then(agh::write::Outcome::into_result);
        let runs = gh.fx().children_run();
        let count = |verb: &str| {
            runs.iter()
                .filter(|s| s.args.iter().any(|a| a == verb))
                .count()
        };
        if mode == "gone" {
            assert!(
                host_git(
                    &remote,
                    &["for-each-ref", "--format=%(refname)", "refs/heads/main"]
                )
                .is_empty()
            );
        } else {
            assert_ne!(host_git(&remote, &["rev-parse", "main"]), main, "{mode}");
        }
        if mode == "forward" {
            result.unwrap_or_else(|error| panic!("{mode}: {error:?}"));
            assert_eq!(count("push"), 2, "{mode}: the expected-old push repeats");
            assert_eq!(host_git(&remote, &["rev-parse", name]), main);
            assert!(journal.open().unwrap().is_empty());
        } else {
            let error = result.unwrap_err();
            assert_eq!(
                (error.kind, error.side_effect),
                (Kind::OutcomeUnknown, SideEffect::Unknown),
                "{mode}: {error:?}"
            );
            assert_eq!(
                journal.open().unwrap().len(),
                1,
                "{mode}: the uncertain intent stays open"
            );
            assert_eq!(
                count("push"),
                1,
                "{mode}: nothing repeats behind an unknown outcome"
            );
        }
        assert_eq!(
            count("ls-remote"),
            3,
            "{mode}: two preparation reads and one recovery"
        );
    }
}
