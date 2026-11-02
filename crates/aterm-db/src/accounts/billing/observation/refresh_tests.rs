// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::accounts::broker::{Broker, Grant, Policy as BrokerPolicy, command};
use aterm_codec::hex;

fn publish(f: &Fixture, grants: Vec<Grant>) {
    super::broker_tests::publish(f, grants);
}

fn broker(f: &Fixture) -> Broker {
    Broker::at(
        f.lab.paths[0].clone(),
        client::config(&f.lab.paths[0]).unwrap().unwrap(),
    )
    .unwrap()
}

pub(crate) fn refresh_transition(
    before: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    transition_with(before, action, false)
}

fn transition_with(
    before: &aterm_spec::interp::State,
    action: &str,
    reset_producer: bool,
) -> aterm_spec::interp::State {
    let (f, b, _, old) = super::broker_tests::setup();
    let other = Grant {
        id: [71; 16],
        producer: [72; 32],
        binding: crate::resources::RequestBinding {
            account: [73; 32],
            ..old.binding.clone()
        },
        ..old.clone()
    };
    let original = BrokerPolicy::new(vec![old.clone(), other.clone()])
        .unwrap()
        .text();
    publish(&f, vec![old.clone(), other.clone()]);
    let entered = Cell::new(0);
    if action == "Retry" {
        let token = b
            .issue_until(old.id, Duration::from_secs(60), deadline())
            .unwrap();
        assert!(
            b.dispatch_until(
                "example-v1",
                token.bytes(),
                [96; 16],
                b"private request",
                deadline(),
                |_, _| {
                    entered.set(entered.get() + 1);
                    None
                }
            )
            .unwrap()
            .entered
        );
    }
    match action {
        "Changed" | "Retry" => {
            f.issue(
                31,
                0,
                f.policy("example-v1").reference([41; 32]).unwrap(),
                vec![old.binding.revision],
            );
        }
        "Absent" => publish(&f, vec![other.clone()]),
        "ForeignActor" => {
            let mut foreign = old.clone();
            foreign.actor = [74; 48];
            publish(&f, vec![foreign, other.clone()]);
        }
        "AdapterChanged" => {
            let policy = f.policy("other-v1");
            f.publish(|doc| {
                doc.set_entry(&Audience::Everyone, ENTRY, policy.text().as_bytes())
                    .unwrap();
            });
            f.issue(
                31,
                0,
                policy.reference([41; 32]).unwrap(),
                vec![old.binding.revision],
            );
        }
        "Unchanged" | "PolicyChanged" => {}
        _ => panic!("unknown refresh scenario"),
    }
    let result = b.test_refresh_after_billing(old.id, deadline(), || {
        if action == "PolicyChanged" {
            publish(&f, vec![old.clone()]);
        }
    });
    let drafted = i64::from(result.is_ok());
    let mut identity = true;
    let mut retained = true;
    if let Ok(refreshed) = result {
        assert_eq!(refreshed.before(), &old);
        assert_eq!(
            refreshed.base(),
            aterm_digest::Sha256::digest(original.as_bytes())
        );
        let mut after = refreshed.after().clone();
        // Negative control: manually reconstructing an otherwise valid policy
        // with a new producer demonstrates why refresh must retain this value.
        if reset_producer {
            after.producer = [99; 32];
        }
        identity = after.id == old.id
            && after.producer == old.producer
            && after.actor == old.actor
            && after.binding.account == old.binding.account
            && after.binding.adapter == old.binding.adapter
            && after.lifetime == old.lifetime;
        retained = refreshed.policy().grants().find(|g| g.id == other.id) == Some(&other);
        assert_eq!(
            after.binding,
            *f.source(31, 0).prepare_until(deadline()).unwrap().binding()
        );
        if action == "Unchanged" {
            assert_eq!(refreshed.policy().text(), original);
        }
        if action == "Retry" {
            publish(&f, vec![after.clone(), other]);
            let next = broker(&f);
            let token = next
                .issue_until(after.id, Duration::from_secs(60), deadline())
                .unwrap();
            let retry = next.dispatch_until(
                "example-v1",
                token.bytes(),
                [96; 16],
                b"private request",
                deadline(),
                |_, _| {
                    entered.set(entered.get() + 1);
                    None
                },
            );
            if reset_producer {
                assert!(retry.unwrap().entered);
            } else {
                assert!(retry.is_err());
            }
        }
    }
    let used = f
        .source(31, 0)
        .prepare_until(deadline())
        .unwrap()
        .resources()
        .snapshot()
        .unwrap()
        .used
        .values()
        .sum::<u64>();
    let scenario = [
        "Changed",
        "Unchanged",
        "Absent",
        "ForeignActor",
        "AdapterChanged",
        "PolicyChanged",
        "Retry",
    ]
    .iter()
    .position(|name| *name == action)
    .unwrap()
        + 1;
    let mut next = before.clone();
    next.insert("scenario", scenario as i64);
    next.insert("drafted", drafted);
    next.insert("identity", i64::from(identity));
    next.insert("retained", i64::from(retained));
    next.insert("used", used as i64);
    next.insert("entered", entered.get());
    next
}

#[test]
fn grant_refresh_conforms_and_resetting_producer_replays_a_request() {
    let model = aterm_models_db::broker_refresh_model();
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    let initial = model.init_state();
    aterm_spec::fs_lane::each(&model.actions, |action| {
        let next = super::refresh_transition(&initial, action.name);
        assert!(
            model.successors(action.name, &initial).contains(&next),
            "{}: {next:?}",
            action.name
        );
        next
    });
    let unsafe_next = transition_with(&initial, "Retry", true);
    assert!(!model.check_invariant("IdentityStable", &unsafe_next));
    assert!(!model.check_invariant("NoReplay", &unsafe_next));
}

/// A new broker process must reopen the durable producer/request claim after a
/// published refresh. Its fresh capability and a new request also have to work.
#[cfg(unix)]
#[test]
fn a_published_refresh_refuses_replay_in_a_new_broker_process() {
    use crate::resources::{RequestKey, RequestRegistry};

    let (f, revision, receipt) = {
        let (f, b, token, old) = super::broker_tests::setup();
        let first = b
            .dispatch_until(
                "example-v1",
                token.bytes(),
                [96; 16],
                b"private request",
                deadline(),
                |_, credential| {
                    assert_eq!(credential, b"synthetic-private-credential");
                    None
                },
            )
            .unwrap();
        assert!(first.entered);
        f.issue(
            31,
            0,
            f.policy("example-v1").reference([41; 32]).unwrap(),
            vec![old.binding.revision],
        );
        let refreshed = b.refresh_proposal_until(old.id, deadline()).unwrap();
        assert_eq!(refreshed.after().id, old.id);
        assert_eq!(refreshed.after().producer, old.producer);
        let revision = refreshed.after().binding.revision;
        assert_ne!(revision, old.binding.revision);
        f.publish(|doc| {
            doc.set_entry(
                &Audience::Everyone,
                crate::accounts::broker::ENTRY,
                refreshed.policy().text().as_bytes(),
            )
            .unwrap();
        });
        // Only fixture ownership and public metadata leave this scope. All old
        // brokers, capabilities, preparations and ledger/admin guards retire.
        (f, revision, first.accounting.claim.receipt)
    };

    struct Process(aterm_uds::spawnfd::Child);
    impl Drop for Process {
        fn drop(&mut self) {
            aterm_uds::deadline::kill_within(&mut self.0, Duration::from_secs(60));
        }
    }

    let mut command = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "accounts::billing::observation::tests::refresh_tests::broker_refresh_replay_child",
            "--ignored",
            "--nocapture",
        ])
        .env("ATERM_TEST_BROKER_REFRESH_HOME", &f.lab.paths[0].home)
        .env(
            "ATERM_TEST_BROKER_REFRESH_REVISION",
            hex::encode_lower(&revision),
        )
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(aterm_uds::spawnfd::Stdio::piped())
        .stderr(aterm_uds::spawnfd::Stdio::inherit());
    let mut child = Process(command.spawn().unwrap());
    assert_ne!(child.0.id(), std::process::id());
    aterm_uds::deadline::prints(
        &mut child.0,
        "broker-refresh-process-replay-checked",
        Duration::from_secs(60),
    );
    let until = deadline();
    let mut exit = aterm_uds::deadline::ExitWait::new(&child.0);
    loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            assert!(status.success(), "refresh child failed: {status}");
            break;
        }
        assert!(Instant::now() < until, "refresh child hang detector");
        exit.wait(until.saturating_duration_since(Instant::now()));
    }
    println!("broker refresh replay verified in child {}", child.0.id());
    let retained = RequestRegistry::at(f.lab.paths[0].clone())
        .lookup_until(RequestKey::new([62; 32], [96; 16]).unwrap(), deadline())
        .unwrap()
        .unwrap();
    assert_eq!(retained.receipt, receipt);
    let used = f.source(31, 0).prepare_until(deadline()).unwrap();
    assert_eq!(
        used.resources()
            .snapshot()
            .unwrap()
            .used
            .values()
            .sum::<u64>(),
        2
    );
}

#[cfg(unix)]
#[test]
#[ignore = "child process fixture, invoked by a_published_refresh_refuses_replay_in_a_new_broker_process"]
fn broker_refresh_replay_child() {
    use crate::resources::{RequestKey, RequestRegistry};
    use std::io::Write as _;

    let home = std::env::var_os("ATERM_TEST_BROKER_REFRESH_HOME").unwrap();
    let paths = aterm_secrets::client::Paths::for_home(std::path::Path::new(&home));
    let expected = client::config(&paths).unwrap().unwrap();
    let revision: [u8; 32] = hex::decode_vec(
        std::env::var("ATERM_TEST_BROKER_REFRESH_REVISION")
            .unwrap()
            .as_bytes(),
    )
    .unwrap()
    .try_into()
    .unwrap();
    let prepared = Billing::at(paths.clone(), expected.clone(), [31; 32])
        .prepare_until(deadline())
        .unwrap();
    assert_eq!(prepared.binding().revision, revision);
    let resources = prepared.resources();
    let used = || resources.snapshot().unwrap().used.values().sum::<u64>();
    assert_eq!(used(), 1);
    let registry = RequestRegistry::at(paths.clone());
    let key = RequestKey::new([62; 32], [96; 16]).unwrap();
    let prior = registry.lookup_until(key, deadline()).unwrap().unwrap();
    let b = Broker::at(paths, expected).unwrap();
    // The parent's token never crosses this boundary. Failure must concern the
    // durable request, not a process-local capability invalidated by restart.
    let token = b
        .issue_until([61; 16], Duration::from_secs(60), deadline())
        .unwrap();
    assert!(
        b.dispatch_until(
            "example-v1",
            token.bytes(),
            [96; 16],
            b"private request",
            deadline(),
            |_, _| panic!("refreshed process entered a replay"),
        )
        .is_err()
    );
    assert_eq!(used(), 1);
    let retained = registry.lookup_until(key, deadline()).unwrap().unwrap();
    assert_eq!(retained.receipt, prior.receipt);
    assert_eq!(retained.operation, prior.operation);
    let entered = Cell::new(0);
    let fresh = b
        .dispatch_until(
            "example-v1",
            token.bytes(),
            [97; 16],
            b"private request",
            deadline(),
            |_, credential| {
                assert_eq!(credential, b"synthetic-private-credential");
                entered.set(entered.get() + 1);
                None
            },
        )
        .unwrap();
    assert!(fresh.entered);
    assert_eq!(fresh.accounting.claim.key.producer(), [62; 32]);
    assert_eq!(entered.get(), 1);
    assert_eq!(used(), 2);
    println!("\nbroker-refresh-process-replay-checked");
    std::io::stdout().flush().unwrap();
}

#[test]
fn refresh_cli_explains_changes_without_publishing_or_initializing_a_ledger() {
    let f = Fixture::new(1);
    f.issue(
        31,
        0,
        f.policy("example-v1").reference([41; 32]).unwrap(),
        vec![],
    );
    let old = command::propose_at(
        f.lab.paths[0].clone(),
        [31; 32],
        [81; 16],
        [82; 32],
        Duration::from_secs(60),
        deadline(),
    )
    .unwrap();
    let original = BrokerPolicy::new(vec![old.clone()]).unwrap().text();
    publish(&f, vec![old.clone()]);
    f.issue(
        31,
        0,
        f.policy("example-v1").reference([41; 32]).unwrap(),
        vec![old.binding.revision],
    );
    let id = aterm_codec::hex::encode_lower(&old.id);
    let paths = f.lab.paths[0].clone();
    let output = command::test_command_with_paths(
        "accounts-broker-refresh",
        BTreeMap::from([("--grant", id.as_str())]),
        paths.clone(),
    )
    .unwrap();
    assert_eq!(output["authority"], json!("none"));
    assert_eq!(
        output["base_policy_sha256"],
        json!(aterm_codec::hex::encode_lower(
            &aterm_digest::Sha256::digest(original.as_bytes())
        ))
    );
    assert_eq!(
        output["changes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["field"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["revision", "credential"]
    );
    let proposed = BrokerPolicy::parse(output["policy"].as_str().unwrap().as_bytes()).unwrap();
    let after = proposed.grants().next().unwrap();
    assert_eq!(after.producer, old.producer);
    assert_ne!(after.binding.revision, old.binding.revision);
    let config = client::config(&paths).unwrap().unwrap();
    assert_eq!(
        client::observe_verified_entry(&paths, &config, crate::accounts::broker::ENTRY)
            .unwrap()
            .unwrap()
            .entry
            .unwrap()
            .value()
            .as_slice(),
        original.as_bytes()
    );
    assert!(
        f.source(31, 0)
            .prepare_until(deadline())
            .unwrap()
            .resources()
            .snapshot()
            .is_err()
    );
    assert!(
        broker(&f)
            .issue_until(old.id, Duration::from_secs(60), deadline())
            .is_err(),
        "unpublished draft became authority"
    );
}
