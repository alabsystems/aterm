// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::{
    accounts::proposals::Proposals,
    database::Change,
    resources::{Proposal, Terms},
    table::tests::{Lab, repair_commons},
};
use aterm_secrets::{commons::roster::Role, text::Audience};
use std::{cell::Cell, collections::BTreeMap, time::Duration};

pub(crate) struct Fixture {
    lab: Lab,
    serial: Cell<u8>,
}

/// A fixture built once for a walk whose steps each take a copy ([`Self::fork`]): its lab and
/// its next serial, which a walk's threads can share.
pub(crate) struct Built {
    lab: Lab,
    serial: u8,
}

impl Built {
    /// A fixture of its own, on a copy of this one's lab, that goes on from this one's serial.
    fn fork(&self) -> Fixture {
        Fixture {
            lab: self.lab.fork(),
            serial: Cell::new(self.serial),
        }
    }
}

impl Fixture {
    fn built(self) -> Built {
        Built {
            lab: self.lab,
            serial: self.serial.get(),
        }
    }

    fn new(machines: usize) -> Self {
        let lab = Lab::new(&vec![Role::Writer; machines]);
        let f = Self {
            lab,
            serial: Cell::new(1),
        };
        let policy = f.policy("example-v1");
        f.publish(|doc| {
            doc.set_entry(&Audience::Everyone, "key", b"synthetic-private-credential")
                .unwrap();
            doc.set_entry(&Audience::Everyone, ENTRY, policy.text().as_bytes())
                .unwrap();
        });
        f
    }

    fn policy(&self, adapter: &str) -> Policy {
        self.policy_in(adapter, self.lab.bootstrap(0).trust())
    }

    fn policy_in(&self, adapter: &str, trust: aterm_secrets::commons::journal::Trust) -> Policy {
        Policy::new(vec![Resource {
            allocation: Proposal::prepare(
                trust,
                Terms {
                    resource: [41; 32],
                    window: [42; 32],
                    unit: "requests".into(),
                    limit: (self.lab.paths.len() * 2) as u64,
                    shares: self
                        .lab
                        .paths
                        .iter()
                        .enumerate()
                        .map(|(n, _)| {
                            (
                                self.lab.bootstrap(n).request().unwrap().binding().actor(),
                                2,
                            )
                        })
                        .collect::<BTreeMap<_, _>>(),
                },
            )
            .unwrap(),
            provider: "example".into(),
            adapter: adapter.into(),
        }])
        .unwrap()
    }

    fn id(&self) -> [u8; 16] {
        let next = self.serial.get();
        self.serial.set(next + 1);
        [next; 16]
    }

    fn publish(&self, update: impl FnOnce(&mut aterm_secrets::text::Document)) {
        let admin = client::admin_open(&self.lab.paths[0]).unwrap();
        let mut doc = admin.doc.clone();
        update(&mut doc);
        client::admin_publish(&self.lab.paths[0], &admin, &doc).unwrap();
        for paths in &self.lab.paths[1..] {
            client::sync(paths, None).unwrap();
        }
    }

    fn issue(
        &self,
        account: u8,
        machine: usize,
        reference: Reference,
        parents: Vec<Digest>,
    ) -> Digest {
        self.issue_optional(account, machine, Some(reference), parents)
    }

    fn issue_optional(
        &self,
        account: u8,
        machine: usize,
        reference: Option<Reference>,
        parents: Vec<Digest>,
    ) -> Digest {
        self.issue_provider(account, machine, reference, parents, "example")
    }

    fn issue_provider(
        &self,
        account: u8,
        machine: usize,
        reference: Option<Reference>,
        parents: Vec<Digest>,
        provider: &str,
    ) -> Digest {
        let paths = &self.lab.paths[0];
        let config = client::config(paths).unwrap().unwrap();
        let key = format!("account-{account}");
        Proposals::at(paths.clone()).write(self.id(), vec![Change {
            collection: "accounts".into(), key: key.clone(),
            value: Some(json!({"provider":provider, "method":"api-key", "billing":reference.map(Reference::value),
                "secret_ref":{"vault":config.name, "entry":"key"}})),
        }]).unwrap();
        let admin = client::admin_open(paths).unwrap();
        let db = Activations::at(paths.clone());
        let actor = self
            .lab
            .bootstrap(machine)
            .request()
            .unwrap()
            .binding()
            .actor();
        let request = db
            .configuration(&admin, [account; 32], &key, actor, parents)
            .unwrap();
        db.issue(&admin, self.id(), request).unwrap().0.id()
    }

    fn source(&self, account: u8, machine: usize) -> Billing {
        let paths = self.lab.paths[machine].clone();
        let expected = client::config(&paths).unwrap().unwrap();
        Billing::at(paths, expected, [account; 32])
    }

    fn repair(&self) {
        // One exchange transfers one common table; discover declarations, then
        // visit the complete catalog lap rather than relying on digest ordering.
        let bootstrap = self.lab.bootstrap(0);
        let session = crate::table::Session::observe(&bootstrap).unwrap();
        let directory = crate::catalog::Directory::new(&self.lab.paths[0], bootstrap.trust());
        let rounds = directory.snapshot_existing(&session).unwrap().entries.len() + 3;
        drop(session);
        repair_commons(&self.lab, rounds);
    }
}

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(60)
}

#[path = "dispatch_tests.rs"]
mod dispatch_tests;
pub(crate) use dispatch_tests::dispatch_transition;

#[path = "broker_tests.rs"]
mod broker_tests;
pub(crate) use broker_tests::broker_transition;
#[path = "transport_tests.rs"]
mod transport_tests;
pub(crate) use transport_tests::transport_transition;
#[path = "host_tests.rs"]
mod host_tests;
pub(crate) use host_tests::host_transition;
#[path = "preparation_tests.rs"]
mod preparation_tests;
pub(crate) use preparation_tests::preparation_transition;
#[path = "selection_tests.rs"]
mod selection_tests;
pub(crate) use selection_tests::selection_transition;
#[path = "refresh_tests.rs"]
mod refresh_tests;
pub(crate) use refresh_tests::refresh_transition;
#[path = "publication_tests.rs"]
mod publication_tests;
pub(crate) use publication_tests::publication_transition;
#[path = "google_tests.rs"]
mod google_tests;

#[test]
fn root_pinned_billing_prepares_exact_credentials_and_one_request_charge() {
    let f = Fixture::new(1);
    let reference = f.policy("example-v1").reference([41; 32]).unwrap();
    let decision = f.issue(31, 0, reference, vec![]);
    let prepared = f.source(31, 0).prepare_until(deadline()).unwrap();
    assert_eq!(prepared.binding().credential, decision);
    assert_eq!(prepared.binding().revision, decision);
    assert_eq!(prepared.binding().policy, reference.policy);
    assert_eq!(
        prepared
            .preparation()
            .credential()
            .unwrap()
            .value()
            .as_slice(),
        b"synthetic-private-credential"
    );
    let resources = prepared.resources();
    assert!(
        resources.snapshot().is_err(),
        "observing does not initialize or spend"
    );
    resources.initialize().unwrap();
    let request = prepared.request([71; 16], b"owned request").unwrap();
    assert_eq!(request.cost(), 1);
    assert!(resources.dispatch_bound(request, |_| None).unwrap().invoked);
    assert_eq!(resources.snapshot().unwrap().used.values().sum::<u64>(), 1);
}

#[test]
fn billing_command_observes_only_public_binding_metadata_and_does_not_enroll_or_spend() {
    let f = Fixture::new(1);
    let reference = f.policy("example-v1").reference([41; 32]).unwrap();
    let decision = f.issue(31, 0, reference, vec![]);
    let command =
        || super::super::command::prepare_at([31; 32], f.lab.paths[0].clone(), deadline());
    let output = command().unwrap();
    assert_eq!(output["kind"], json!("billing-preparation-observation"));
    assert_eq!(
        output["decision"],
        json!(aterm_codec::hex::encode_lower(&decision))
    );
    assert_eq!(output["billing"], reference.value());
    assert_eq!(output["credential_bound"], json!(true));
    assert_eq!(output["ledger"], json!("not-observed"));
    assert_eq!(output["request_cost"], json!(1));
    assert_eq!(output["provider_request"], json!("not-attempted"));
    assert!(
        !aterm_json::to_string(&output)
            .unwrap()
            .contains("synthetic-private-credential")
    );
    let resources = f
        .source(31, 0)
        .prepare_until(deadline())
        .unwrap()
        .resources();
    assert!(
        resources.snapshot().is_err(),
        "command did not enroll a ledger"
    );
    resources.initialize().unwrap();
    let before = resources.snapshot().unwrap();
    assert_eq!(command().unwrap(), output);
    let after = resources.snapshot().unwrap();
    assert_eq!(before.used, after.used);
    assert_eq!(before.retained, after.retained);
    let db = Activations::at(f.lab.paths[0].clone());
    db.issue(
        &client::admin_open(&f.lab.paths[0]).unwrap(),
        f.id(),
        db.stop_request([31; 32], vec![decision]).unwrap(),
    )
    .unwrap();
    assert!(command().is_err());
}

#[test]
#[cfg(unix)]
fn native_billing_preparation_waits_for_unchanged_custody() {
    let f = Fixture::new(1);
    let reference = f.policy("example-v1").reference([41; 32]).unwrap();
    f.issue(31, 0, reference, vec![]);
    let expected =
        super::super::command::prepare_at([31; 32], f.lab.paths[0].clone(), deadline()).unwrap();
    let held = aterm_uds::private_file::PrivateLock::take_with(
        &f.lab.paths[0].dir,
        "commons.lock",
        Duration::ZERO,
        "private custody held",
        || {},
    )
    .unwrap();
    let mut held = Some(held);
    let result = aterm_uds::file_lock::with_lock_wait_observer(
        move |_| {
            if let Some(held) = held.take() {
                drop(held);
            }
        },
        || {
            super::super::command::prepare_started(
                [31; 32],
                f.lab.paths[0].clone(),
                Instant::now() - Duration::from_secs(3),
            )
        },
    );
    assert_eq!(result.unwrap(), expected);
    let held = aterm_uds::private_file::PrivateLock::take_with(
        &f.lab.paths[0].dir,
        "commons.lock",
        Duration::ZERO,
        "private custody held",
        || {},
    )
    .unwrap();
    assert!(
        super::super::command::prepare_started(
            [31; 32],
            f.lab.paths[0].clone(),
            Instant::now() - Duration::from_secs(61)
        )
        .is_err()
    );
    drop(held);
    assert!(
        f.source(31, 0)
            .prepare_until(deadline())
            .unwrap()
            .resources()
            .snapshot()
            .is_err()
    );
}

#[test]
fn root_pinned_aliases_share_fixed_offline_capacity_and_sync_later() {
    let f = Fixture::new(2);
    let reference = f.policy("example-v1").reference([41; 32]).unwrap();
    f.issue(31, 0, reference, vec![]);
    f.issue(32, 1, reference, vec![]);
    f.repair();
    let a = f.source(31, 0).prepare_until(deadline()).unwrap();
    let b = f.source(32, 1).prepare_until(deadline()).unwrap();
    assert_eq!(a.allocation().id(), b.allocation().id());
    let left = a.resources();
    let right = b.resources();
    left.initialize().unwrap();
    right.initialize().unwrap();
    for (resources, prepared) in [(&left, &a), (&right, &b)] {
        for id in [71, 72] {
            assert!(
                resources
                    .dispatch_bound(prepared.request([id; 16], b"offline").unwrap(), |_| None)
                    .unwrap()
                    .invoked
            );
        }
        assert!(
            resources
                .dispatch_bound(
                    prepared.request([73; 16], b"over share").unwrap(),
                    |_| panic!("overdraw")
                )
                .is_err()
        );
    }
    f.repair();
    assert_eq!(
        left.snapshot().unwrap().used,
        right.snapshot().unwrap().used
    );
    assert_eq!(left.snapshot().unwrap().used.values().sum::<u64>(), 4);
}

#[test]
fn policy_changes_require_a_new_root_account_decision() {
    let f = Fixture::new(1);
    let reference = f.policy("example-v1").reference([41; 32]).unwrap();
    let old = f.issue(31, 0, reference, vec![]);
    let source = f.source(31, 0);
    let prepared = source.prepare_until(deadline()).unwrap();
    let replacement = f.policy("example-v2");
    f.publish(|doc| {
        doc.set_entry(&Audience::Everyone, ENTRY, replacement.text().as_bytes())
            .unwrap();
    });
    assert!(source.prepare_until(deadline()).is_err());
    assert!(prepared.refresh_until(deadline()).is_err());
    f.issue(31, 0, replacement.reference([41; 32]).unwrap(), vec![old]);
    assert_eq!(
        source.prepare_until(deadline()).unwrap().binding().adapter,
        "example-v2"
    );
}

#[test]
fn local_edits_changed_credentials_and_root_stops_never_prepare_billing() {
    let f = Fixture::new(1);
    let policy = f.policy("example-v1");
    let decision = f.issue(31, 0, policy.reference([41; 32]).unwrap(), vec![]);
    let source = f.source(31, 0);
    source.prepare_until(deadline()).unwrap();
    let file = f.lab.paths[0].home.join(".secrets").join(ENTRY);
    std::fs::write(&file, f.policy("example-v2").text()).unwrap();
    assert!(source.prepare_until(deadline()).is_err());
    std::fs::write(&file, policy.text()).unwrap();
    source.prepare_until(deadline()).unwrap();
    f.publish(|doc| {
        doc.set_entry(&Audience::Everyone, "key", b"changed-key")
            .unwrap();
    });
    assert!(source.prepare_until(deadline()).is_err());
    f.publish(|doc| {
        doc.set_entry(&Audience::Everyone, "key", b"synthetic-private-credential")
            .unwrap();
    });
    source.prepare_until(deadline()).unwrap();
    let db = Activations::at(f.lab.paths[0].clone());
    let stop = db.stop_request([31; 32], vec![decision]).unwrap();
    db.issue(&client::admin_open(&f.lab.paths[0]).unwrap(), f.id(), stop)
        .unwrap();
    assert!(source.prepare_until(deadline()).is_err());
}

#[test]
fn refresh_advances_vault_floor_and_rejects_rollback_and_expiry() {
    let f = Fixture::new(1);
    f.issue(
        31,
        0,
        f.policy("example-v1").reference([41; 32]).unwrap(),
        vec![],
    );
    let source = f.source(31, 0);
    assert!(source.prepare_until(Instant::now()).is_err());
    let files: Vec<_> = ["state.toml", "vault"]
        .iter()
        .map(|name| {
            let path = f.lab.paths[0].dir.join(name);
            let bytes = std::fs::read(&path).unwrap();
            (path, bytes)
        })
        .collect();
    let prepared = source.prepare_until(deadline()).unwrap();
    f.publish(|doc| {
        doc.set_entry(&Audience::Everyone, "unrelated", b"changed")
            .unwrap();
    });
    let prepared = prepared.refresh_until(deadline()).unwrap();
    for (path, bytes) in files {
        std::fs::write(path, bytes).unwrap();
    }
    assert!(prepared.refresh_until(deadline()).is_err());
}

pub(crate) fn transition(
    before: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    transition_with(before, action, false)
}

fn transition_with(
    before: &aterm_spec::interp::State,
    action: &str,
    accept_self_asserted_pin: bool,
) -> aterm_spec::interp::State {
    let names = [
        "Valid",
        "MissingPin",
        "ChangedPolicy",
        "ChangedCredential",
        "Stopped",
        "LocalEdit",
        "Rollback",
        "Expired",
        "Unrelated",
        "ForeignRoot",
    ];
    let scenario = names.iter().position(|name| *name == action).unwrap() + 1;
    let f = Fixture::new(1);
    let policy = if action == "ForeignRoot" {
        let mut trust = f.lab.bootstrap(0).trust();
        trust.network = [18; 16];
        let policy = f.policy_in("example-v1", trust);
        f.publish(|doc| {
            doc.set_entry(&Audience::Everyone, ENTRY, policy.text().as_bytes())
                .unwrap();
        });
        policy
    } else {
        f.policy("example-v1")
    };
    let reference = (action != "MissingPin").then(|| policy.reference([41; 32]).unwrap());
    let decision = f.issue_optional(31, 0, reference, vec![]);
    let source = f.source(31, 0);
    let accepted = match action {
        "Valid" | "MissingPin" | "ForeignRoot" => source.prepare_until(deadline()).is_ok(),
        "ChangedPolicy" => {
            let replacement = f.policy("example-v2");
            f.publish(|doc| {
                doc.set_entry(&Audience::Everyone, ENTRY, replacement.text().as_bytes())
                    .unwrap();
            });
            if accept_self_asserted_pin {
                // Executable negative control: the current file supplies its own
                // fingerprint instead of the independently root-pinned reference.
                replacement
                    .select(
                        replacement.reference([41; 32]).unwrap(),
                        f.lab.bootstrap(0).trust(),
                        "example",
                    )
                    .is_ok()
            } else {
                source.prepare_until(deadline()).is_ok()
            }
        }
        "ChangedCredential" => {
            f.publish(|doc| {
                doc.set_entry(&Audience::Everyone, "key", b"different credential")
                    .unwrap();
            });
            source.prepare_until(deadline()).is_ok()
        }
        "Stopped" => {
            let db = Activations::at(f.lab.paths[0].clone());
            let request = db.stop_request([31; 32], vec![decision]).unwrap();
            db.issue(
                &client::admin_open(&f.lab.paths[0]).unwrap(),
                f.id(),
                request,
            )
            .unwrap();
            source.prepare_until(deadline()).is_ok()
        }
        "LocalEdit" => {
            std::fs::write(
                f.lab.paths[0].home.join(".secrets").join(ENTRY),
                f.policy("example-v2").text(),
            )
            .unwrap();
            source.prepare_until(deadline()).is_ok()
        }
        "Unrelated" | "Rollback" => {
            let files: Vec<_> = ["state.toml", "vault"]
                .iter()
                .map(|name| {
                    let path = f.lab.paths[0].dir.join(name);
                    let bytes = std::fs::read(&path).unwrap();
                    (path, bytes)
                })
                .collect();
            let old = source.prepare_until(deadline()).unwrap();
            f.publish(|doc| {
                doc.set_entry(&Audience::Everyone, "other", b"unrelated update")
                    .unwrap();
            });
            let next = old.refresh_until(deadline());
            if action == "Unrelated" {
                next.is_ok()
            } else {
                let next = next.unwrap();
                for (path, bytes) in files {
                    std::fs::write(path, bytes).unwrap();
                }
                next.refresh_until(deadline()).is_ok()
            }
        }
        "Expired" => source.prepare_until(Instant::now()).is_ok(),
        _ => unreachable!(),
    };
    let mut next = before.clone();
    next.insert("scenario", scenario as i64);
    next.insert("accepted", i64::from(accepted));
    next
}

#[test]
fn billing_model_conforms_and_detects_a_self_asserted_policy_pin() {
    let model = aterm_models_db::billing_model();
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    let initial = model.init_state();
    aterm_spec::fs_lane::each(&model.actions, |action| {
        let next = transition(&initial, action.name);
        assert!(
            model.successors(action.name, &initial).contains(&next),
            "{}: {next:?}",
            action.name
        );
        next
    });
    let unsafe_next = transition_with(&initial, "ChangedPolicy", true);
    assert!(!model.check_invariant("NoUnboundPreparation", &unsafe_next));
}

#[test]
fn fleet_intent_billing_follows_current_children_and_parent_stop() {
    use aterm_secrets::commons::accounts::IntentRef;
    let f = Fixture::new(2);
    let paths = &f.lab.paths[0];
    let expected = client::config(paths).unwrap().unwrap();
    let reference = f.policy("example-v1").reference([41; 32]).unwrap();
    Proposals::at(paths.clone())
        .write(
            f.id(),
            vec![Change {
                collection: "accounts".into(),
                key: "intent".into(),
                value: Some(
                    json!({"provider":"example", "method":"api-key", "billing":reference.value(),
            "secret_ref":{"vault":expected.name, "entry":"key"}}),
                ),
            }],
        )
        .unwrap();
    let db = Activations::at(paths.clone());
    let admin = client::admin_open(paths).unwrap();
    let request = db
        .intent_configuration(&admin, [34; 32], "intent", vec![])
        .unwrap();
    let root = db.issue(&admin, f.id(), request).unwrap().0;
    db.materialize_next_intent(&admin).unwrap().unwrap();
    db.materialize_next_intent(&admin).unwrap().unwrap();
    f.repair();
    let intent = IntentRef {
        account: [34; 32],
        decision: root.id(),
    };
    let sources: Vec<_> = (0..2)
        .map(|n| {
            let paths = f.lab.paths[n].clone();
            let expected = client::config(&paths).unwrap().unwrap();
            let account = intent.device_account(f.lab.bootstrap(n).request().unwrap().binding());
            Billing::at(paths, expected, account)
        })
        .collect();
    let revisions: Vec<_> = sources
        .iter()
        .map(|source| source.prepare_until(deadline()).unwrap().binding().revision)
        .collect();
    let request = db
        .intent_configuration(&admin, [34; 32], "intent", vec![root.id()])
        .unwrap();
    let replacement = db.issue(&admin, f.id(), request).unwrap().0;
    assert!(sources[0].prepare_until(deadline()).is_err());
    db.materialize_next_intent(&admin).unwrap().unwrap();
    db.materialize_next_intent(&admin).unwrap().unwrap();
    f.repair();
    for (n, source) in sources.iter().enumerate() {
        let current = source.prepare_until(deadline()).unwrap();
        assert_ne!(current.binding().revision, revisions[n]);
        let next = IntentRef {
            account: [34; 32],
            decision: replacement.id(),
        };
        assert_eq!(
            next.device_account(f.lab.bootstrap(n).request().unwrap().binding()),
            source.account
        );
    }
    let stop = db.stop_request([34; 32], vec![replacement.id()]).unwrap();
    db.issue(&admin, f.id(), stop).unwrap();
    f.repair();
    for source in sources {
        assert!(source.prepare_until(deadline()).is_err());
    }
}

#[test]
fn billing_uses_selected_successor_custody_without_inheriting_spent_share() {
    let f = Fixture::new(2);
    let reference = f.policy("example-v1").reference([41; 32]).unwrap();
    let old = f.issue(31, 0, reference, vec![]);
    let source = f.source(31, 0);
    let prepared = source.prepare_until(deadline()).unwrap();
    let resources = prepared.resources();
    resources.initialize().unwrap();
    resources.reserve([85; 16], 2).unwrap();
    f.repair();
    let actor = f.lab.bootstrap(1).request().unwrap().binding().actor();
    let config = f.lab.paths[0].home.join("harness.toml");
    aterm_uds::atomic_file::write(&config, b"", 0o600).unwrap();
    let selected = crate::test_complete_writer_successor(
        f.lab._home.path(),
        &f.lab.paths[0],
        &f.lab.paths[1],
        &config,
        deadline(),
    )
    .unwrap();
    assert!(prepared.refresh_until(deadline()).is_err());
    let db = Activations::at(f.lab.paths[0].clone());
    let admin = client::admin_open(&f.lab.paths[0]).unwrap();
    let request = db
        .configuration(&admin, [31; 32], "account-31", actor, vec![old])
        .unwrap();
    let new = db.issue(&admin, f.id(), request).unwrap().0;
    std::fs::remove_file(f.lab.paths[0].home.join(".secrets").join(ENTRY)).unwrap();
    let next = source.prepare_until(deadline()).unwrap();
    assert_eq!(next.active_paths, selected);
    assert_eq!(next.binding().credential, new.id());
    let resources = next.resources();
    resources.initialize().unwrap();
    assert_eq!(resources.snapshot().unwrap().used.values().sum::<u64>(), 2);
    resources.reserve([86; 16], 2).unwrap();
    assert!(resources.reserve([87; 16], 1).is_err());
    assert_eq!(resources.snapshot().unwrap().used.values().sum::<u64>(), 4);
}
