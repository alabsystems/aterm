// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::resources::{RequestKey, Resources};

fn key() -> RequestKey {
    RequestKey::new([51; 32], [52; 16]).unwrap()
}

struct Monitor {
    entered: Cell<i64>,
    durable: Cell<i64>,
    exact: Cell<i64>,
}
impl Monitor {
    fn new() -> Self {
        Self {
            entered: Cell::new(0),
            durable: Cell::new(1),
            exact: Cell::new(1),
        }
    }
    fn enter(
        &self,
        request: &BoundRequest,
        credential: &[u8],
        resources: &Resources,
    ) -> Option<Observation> {
        self.entered.set(self.entered.get() + 1);
        if resources.attempt(request.id()).unwrap().is_none() {
            self.durable.set(0);
        }
        if request.payload() != b"private request" || credential != b"synthetic-private-credential"
        {
            self.exact.set(0);
        }
        None
    }
}

fn call(
    source: &Billing,
    monitor: &Monitor,
    probe: (&Resources, &Allocation),
    adapter: &str,
    bypass: bool,
    before: impl FnOnce(),
) -> Result<bool> {
    let (resources, allocation) = probe;
    let observe = |request: &BoundRequest, credential: &[u8]| {
        let claim = RequestRegistry::at(source.paths.clone())
            .lookup_until(key(), deadline())
            .unwrap();
        if claim.is_none_or(|claim| claim.operation != request.operation(allocation).unwrap()) {
            monitor.durable.set(0);
        }
        monitor.enter(request, credential, resources)
    };
    if bypass {
        let prepared = source.prepare_until(deadline())?;
        return resources
            .dispatch_bound(
                prepared.request(key().ledger_id(), b"private request")?,
                |request| {
                    observe(
                        request,
                        prepared
                            .preparation()
                            .credential()
                            .unwrap()
                            .value()
                            .as_slice(),
                    )
                },
            )
            .map(|_| true);
    }
    source
        .dispatch_with_until(
            key(),
            adapter,
            b"private request",
            deadline(),
            before,
            observe,
        )
        .map(|result| result.refusal.is_none())
}

fn stop(f: &Fixture, decision: Digest) {
    let db = Activations::at(f.lab.paths[0].clone());
    db.issue(
        &client::admin_open(&f.lab.paths[0]).unwrap(),
        f.id(),
        db.stop_request([31; 32], vec![decision]).unwrap(),
    )
    .unwrap();
}

pub(super) fn succeed(f: &Fixture, source: &Billing, decision: Digest) -> Resources {
    f.repair();
    let actor = f.lab.bootstrap(1).request().unwrap().binding().actor();
    let config = f.lab.paths[0].home.join("harness.toml");
    aterm_uds::atomic_file::write(&config, b"", 0o600).unwrap();
    crate::test_complete_writer_successor(
        f.lab._home.path(),
        &f.lab.paths[0],
        &f.lab.paths[1],
        &config,
        deadline(),
    )
    .unwrap();
    let db = Activations::at(f.lab.paths[0].clone());
    let admin = client::admin_open(&f.lab.paths[0]).unwrap();
    let request = db
        .configuration(&admin, [31; 32], "account-31", actor, vec![decision])
        .unwrap();
    db.issue(&admin, f.id(), request).unwrap();
    let resources = source.prepare_until(deadline()).unwrap().resources();
    resources.initialize().unwrap();
    resources
}

pub(crate) fn dispatch_transition(
    before: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    transition_with(before, action, false)
}

fn transition_with(
    before: &aterm_spec::interp::State,
    action: &str,
    bypass: bool,
) -> aterm_spec::interp::State {
    let f = Fixture::new(if action == "Succession" { 2 } else { 1 });
    let policy = f.policy("example-v1");
    let reference = policy.reference([41; 32]).unwrap();
    let decision = f.issue(31, 0, reference, vec![]);
    let source = f.source(31, 0);
    let prepared = source.prepare_until(deadline()).unwrap();
    let allocation = prepared.allocation().clone();
    let resources = prepared.resources();
    let probe = (&resources, &allocation);
    resources.initialize().unwrap();
    let monitor = Monitor::new();
    let scenario = match action {
        "Fresh" => 1,
        "Retry" => 2,
        "Window" => 3,
        "Account" => 4,
        "Adapter" => 5,
        "StoppedBefore" => 6,
        "CredentialAfter" => 7,
        "StoppedAfter" => 8,
        "Panic" => 9,
        "Exhausted" => 10,
        "Succession" => 11,
        "LostRegistry" => 12,
        "Expired" => 13,
        "PendingEnrollment" => 14,
        _ => unreachable!(),
    };
    if matches!(
        action,
        "Retry" | "Window" | "Account" | "Succession" | "LostRegistry"
    ) {
        assert!(call(&source, &monitor, probe, "example-v1", false, || {}).unwrap());
    }
    let accepted = match action {
        "Fresh" | "Retry" => call(&source, &monitor, probe, "example-v1", false, || {}),
        "PendingEnrollment" => {
            RequestRegistry::at(f.lab.paths[0].clone())
                .test_pending_enrollment(deadline())
                .unwrap();
            call(&source, &monitor, probe, "example-v1", false, || {})
        }
        "Window" | "LostRegistry" => {
            if action == "LostRegistry" {
                let bootstrap = f.lab.bootstrap(0);
                let session = crate::table::Session::open(&bootstrap).unwrap();
                let scope = crate::catalog::Descriptor::application(
                    bootstrap.trust(),
                    "resources",
                    "requests",
                    1,
                )
                .unwrap()
                .scope;
                let directory = crate::catalog::Directory::new(&f.lab.paths[0], bootstrap.trust());
                let data = directory.path(scope).unwrap();
                let claim = crate::table::store::claim_dir(&session)
                    .unwrap()
                    .join(crate::table::store::claim_name(scope));
                drop(session);
                std::fs::rename(&data, data.with_extension("preserved")).unwrap();
                std::fs::rename(&claim, claim.with_extension("preserved")).unwrap();
            }
            let mut terms = policy
                .resources()
                .next()
                .unwrap()
                .allocation
                .terms()
                .clone();
            terms.window = [99; 32];
            let replacement = Policy::new(vec![Resource {
                allocation: Proposal::prepare(f.lab.bootstrap(0).trust(), terms).unwrap(),
                provider: "example".into(),
                adapter: "example-v1".into(),
            }])
            .unwrap();
            f.publish(|doc| {
                doc.set_entry(&Audience::Everyone, ENTRY, replacement.text().as_bytes())
                    .unwrap();
            });
            f.issue(
                31,
                0,
                replacement.reference([41; 32]).unwrap(),
                vec![decision],
            );
            let prepared = source.prepare_until(deadline()).unwrap();
            let resources = prepared.resources();
            resources.initialize().unwrap();
            call(
                &source,
                &monitor,
                (&resources, prepared.allocation()),
                "example-v1",
                bypass,
                || {},
            )
        }
        "Account" => {
            f.issue(32, 0, reference, vec![]);
            call(
                &f.source(32, 0),
                &monitor,
                probe,
                "example-v1",
                false,
                || {},
            )
        }
        "Adapter" => call(&source, &monitor, probe, "different-v1", false, || {}),
        "StoppedBefore" => {
            stop(&f, decision);
            call(&source, &monitor, probe, "example-v1", false, || {})
        }
        "CredentialAfter" => call(&source, &monitor, probe, "example-v1", false, || {
            f.publish(|doc| {
                doc.set_entry(&Audience::Everyone, "key", b"changed-credential")
                    .unwrap();
            });
        }),
        "StoppedAfter" => call(&source, &monitor, probe, "example-v1", false, || {
            stop(&f, decision)
        }),
        "Panic" => {
            assert!(
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    source
                        .dispatch_until(
                            key(),
                            "example-v1",
                            b"private request",
                            deadline(),
                            |request, credential| {
                                monitor.enter(request, credential, &resources);
                                panic!("synthetic adapter failure");
                            },
                        )
                        .unwrap();
                }))
                .is_err()
            );
            call(&source, &monitor, probe, "example-v1", false, || {})
        }
        "Exhausted" => {
            resources.reserve([88; 16], 2).unwrap();
            call(&source, &monitor, probe, "example-v1", false, || {})
        }
        "Succession" => {
            let resources = succeed(&f, &source, decision);
            call(
                &source,
                &monitor,
                (&resources, &allocation),
                "example-v1",
                false,
                || {},
            )
        }
        "Expired" => source
            .dispatch_until(
                key(),
                "example-v1",
                b"private request",
                Instant::now(),
                |request, credential| monitor.enter(request, credential, &resources),
            )
            .map(|d| d.refusal.is_none()),
        _ => unreachable!(),
    }
    .unwrap_or(false);
    let mut after = before.clone();
    after.insert("scenario", scenario);
    after.insert("accepted", i64::from(accepted));
    after.insert("entered", monitor.entered.get());
    after.insert("durable", monitor.durable.get());
    after.insert("exact", monitor.exact.get());
    after
}

#[test]
fn metered_dispatch_conforms_and_catches_cross_window_replay() {
    let model = aterm_models_db::metered_dispatch_model();
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    let initial = model.init_state();
    aterm_spec::fs_lane::each(&model.actions, |action| {
        let next = super::dispatch_transition(&initial, action.name);
        assert!(
            model.successors(action.name, &initial).contains(&next),
            "{}: {next:?}",
            action.name
        );
        next
    });
    let unsafe_next = transition_with(&initial, "Window", true);
    assert!(!model.check_invariant("NoReplay", &unsafe_next));
}

#[test]
fn registry_claim_can_resume_after_missing_resource_without_double_dispatch() {
    let f = Fixture::new(1);
    f.issue(
        31,
        0,
        f.policy("example-v1").reference([41; 32]).unwrap(),
        vec![],
    );
    let source = f.source(31, 0);
    assert!(
        source
            .dispatch_until(
                key(),
                "example-v1",
                b"private request",
                deadline(),
                |_, _| panic!("uninitialized")
            )
            .is_err()
    );
    let registry = RequestRegistry::at(f.lab.paths[0].clone());
    let claim = registry.lookup_until(key(), deadline()).unwrap().unwrap();
    let resources = source.prepare_until(deadline()).unwrap().resources();
    resources.initialize().unwrap();
    let first = source
        .dispatch_until(
            key(),
            "example-v1",
            b"private request",
            deadline(),
            |_, _| None,
        )
        .unwrap();
    assert!(first.entered);
    assert_eq!(first.claim.receipt, claim.receipt);
    let retry = source
        .dispatch_until(
            key(),
            "example-v1",
            b"private request",
            deadline(),
            |_, _| panic!("replay"),
        )
        .unwrap();
    assert!(!retry.entered);
    assert_eq!(first.attempt.receipt, retry.attempt.receipt);
}

#[test]
fn failed_refresh_stays_charged_and_never_replays_after_credential_restore() {
    let f = Fixture::new(1);
    f.issue(
        31,
        0,
        f.policy("example-v1").reference([41; 32]).unwrap(),
        vec![],
    );
    let source = f.source(31, 0);
    let resources = source.prepare_until(deadline()).unwrap().resources();
    resources.initialize().unwrap();
    let first = source
        .dispatch_with_until(
            key(),
            "example-v1",
            b"private request",
            deadline(),
            || {
                f.publish(|doc| {
                    doc.set_entry(&Audience::Everyone, "key", b"changed")
                        .unwrap();
                });
            },
            |_, _| panic!("stale credential"),
        )
        .unwrap();
    assert!(!first.entered);
    assert!(first.refusal.is_some());
    assert!(first.attempt.observation.confirmed().is_none());
    assert_eq!(resources.snapshot().unwrap().used.values().sum::<u64>(), 1);
    f.publish(|doc| {
        doc.set_entry(&Audience::Everyone, "key", b"synthetic-private-credential")
            .unwrap();
    });
    let retry = source
        .dispatch_until(
            key(),
            "example-v1",
            b"private request",
            deadline(),
            |_, _| panic!("replay"),
        )
        .unwrap();
    assert!(!retry.entered);
    assert_eq!(first.attempt.receipt, retry.attempt.receipt);
}

#[test]
fn billing_dispatch_keeps_offline_alias_shares_and_request_retries_through_repair() {
    let f = Fixture::new(2);
    let reference = f.policy("example-v1").reference([41; 32]).unwrap();
    f.issue(31, 0, reference, vec![]);
    f.issue(32, 1, reference, vec![]);
    f.repair();
    for (n, account) in [(0, 31), (1, 32)] {
        let source = f.source(account, n);
        source
            .prepare_until(deadline())
            .unwrap()
            .resources()
            .initialize()
            .unwrap();
        for logical in [1, 2] {
            let key = RequestKey::new([51; 32], [logical; 16]).unwrap();
            let result = source
                .dispatch_until(
                    key,
                    "example-v1",
                    b"offline",
                    deadline(),
                    |request, credential| {
                        assert_eq!(request.payload(), b"offline");
                        assert_eq!(credential, b"synthetic-private-credential");
                        None
                    },
                )
                .unwrap();
            assert!(result.entered);
        }
        let third = RequestKey::new([51; 32], [3; 16]).unwrap();
        assert!(
            source
                .dispatch_until(third, "example-v1", b"offline", deadline(), |_, _| panic!(
                    "overdraw"
                ))
                .is_err()
        );
    }
    f.repair();
    for (n, account) in [(0, 31), (1, 32)] {
        let source = f.source(account, n);
        let resources = source.prepare_until(deadline()).unwrap().resources();
        assert_eq!(resources.snapshot().unwrap().used.values().sum::<u64>(), 4);
        let first = RequestKey::new([51; 32], [1; 16]).unwrap();
        assert!(
            !source
                .dispatch_until(first, "example-v1", b"offline", deadline(), |_, _| panic!(
                    "replay"
                ))
                .unwrap()
                .entered
        );
    }
}

#[test]
fn composed_dispatch_cannot_take_over_an_ancestor_request_after_writer_succession() {
    let f = Fixture::new(2);
    let reference = f.policy("example-v1").reference([41; 32]).unwrap();
    let decision = f.issue(31, 0, reference, vec![]);
    let source = f.source(31, 0);
    source
        .prepare_until(deadline())
        .unwrap()
        .resources()
        .initialize()
        .unwrap();
    source
        .dispatch_until(
            key(),
            "example-v1",
            b"private request",
            deadline(),
            |_, _| None,
        )
        .unwrap();
    let resources = succeed(&f, &source, decision);
    assert!(
        source
            .dispatch_until(
                key(),
                "example-v1",
                b"private request",
                deadline(),
                |_, _| panic!("ancestor replay")
            )
            .is_err()
    );
    let next = RequestKey::new([51; 32], [53; 16]).unwrap();
    let started = Instant::now();
    let fresh = source
        .dispatch_until(next, "example-v1", b"new request", deadline(), |_, _| None)
        .unwrap();
    // A refusal after the full minute is the hang detector (ATERM-KI-1105-13).
    assert!(
        fresh.entered,
        "a fresh request was withheld after {:?}: {:?}",
        started.elapsed(),
        fresh.refusal
    );
    assert_eq!(resources.snapshot().unwrap().used.values().sum::<u64>(), 2);
}
