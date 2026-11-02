// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::{
    control::{Endpoint, Repair},
    table::tests::{Lab, repair_commons},
};
use aterm_net::lan::{Connection, Scope as LinkScope};
use aterm_secrets::{SecretBytes, commons::roster::Role, fsx, text::Audience};
use std::time::Duration;

const SENTINEL: &[u8] = b"scratch-mailbox-contribution-65a709";

fn fixture() -> Lab {
    let lab = Lab::new(&[Role::Writer, Role::Writer, Role::Writer]);
    for paths in &lab.paths {
        need::test_alab_policy(paths);
    }
    need::raise(
        &lab.paths[2],
        Want::Mailbox("fixture".into()),
        Purpose::ByHand,
    )
    .unwrap();
    pass(&lab.paths[2]).unwrap();
    repair_commons(&lab, 3);
    lab
}

fn round(lab: &Lab, permits: bool) -> (Repair, Repair, [crate::control::Report; 2]) {
    let endpoint = |n| {
        let endpoint = Endpoint::from_bootstrap(&lab.bootstrap(n))
            .unwrap()
            .unwrap()
            .with_commons(lab.paths[n].clone(), lab.paths[n].home.join("config.toml"))
            .unwrap();
        if permits {
            endpoint.with_credential_owner(
                CredentialOwner::for_test(&lab.paths[n]),
                lab.paths[n].home.join("config.toml"),
            )
        } else {
            endpoint
        }
    };
    let custodian = endpoint(0);
    let holder = endpoint(1);
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let mut left = Repair::default();
    let mut right = Repair::default();
    let controls = std::thread::scope(|tasks| {
        let receiving = tasks.spawn(|| {
            holder.exchange(
                Connection::admit(listener.accept().unwrap().0, LinkScope::Loopback).unwrap(),
                Side::Responder,
                &mut right,
            )
        });
        let left = custodian
            .exchange(
                Connection::connect(address, LinkScope::Loopback, Duration::from_secs(60)).unwrap(),
                Side::Initiator,
                &mut left,
            )
            .unwrap();
        let right = receiving.join().unwrap().unwrap();
        [left, right]
    });
    assert!(
        left.application_error().is_none(),
        "{:?}",
        left.application_error()
    );
    assert!(
        right.application_error().is_none(),
        "{:?}",
        right.application_error()
    );
    (left, right, controls)
}

#[test]
fn a_private_give_crosses_native_authenticated_runtime_transport_and_never_enters_the_log() {
    use std::os::unix::fs::PermissionsExt as _;
    let lab = fixture();
    let given = give_from(&lab.paths[1], "mailbox:fixture", || {
        Ok(SecretBytes::from(SENTINEL))
    })
    .unwrap();
    assert_eq!((given.retained, given.published), (1, 0));
    let files: Vec<_> = std::fs::read_dir(&lab.paths[1].dir)
        .unwrap()
        .filter_map(|e| {
            let entry = e.unwrap();
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("credential-give-")
                .then_some(entry.path())
        })
        .collect();
    assert_eq!(files.len(), 1);
    assert_eq!(
        std::fs::metadata(&files[0]).unwrap().permissions().mode() & 0o777,
        0o600
    );
    repair_commons(&lab, 3);
    let (left, right, _) = round(&lab, false);
    assert!(
        left.handoff().is_none() && right.handoff().is_none(),
        "converge has no transport-owner permit"
    );
    assert!(files[0].exists());
    let (left, right, controls) = round(&lab, true);
    assert_eq!(left.handoff(), Some(&Ok(1)));
    assert_eq!(right.handoff(), Some(&Ok(1)));
    for (repair, control) in [(&left, controls[0]), (&right, controls[1])] {
        assert_eq!(control.sent + control.received, 0);
        assert!(
            repair
                .membership()
                .is_none_or(|report| report.sent + report.received == 0)
        );
        let app = repair.application().unwrap();
        assert_eq!(
            app.sent
                + app.received
                + app.keys_sent
                + app.keys_received
                + app.conflicts_sent
                + app.conflicts_received,
            0
        );
        assert_eq!(
            crate::runtime::Outcome::of(true, repair, Some(control)),
            crate::runtime::Outcome::Repaired { transferred: true },
            "a durable credential handoff must qualify as progress"
        );
    }
    assert!(
        !files[0].exists(),
        "the custodian's durable publication acknowledged the give"
    );
    let admin = client::admin_cached(&lab.paths[0]).unwrap();
    let requester = Audience::Hosts(vec!["m3".into()]);
    assert_eq!(
        admin
            .doc
            .entry(&requester, "fixture_password")
            .unwrap()
            .value
            .as_slice(),
        SENTINEL
    );
    for host in ["m1", "m2"] {
        assert!(
            admin
                .doc
                .entry(&Audience::Hosts(vec![host.into()]), "fixture_password")
                .is_none()
        );
    }
    repair_commons(&lab, 3);
    for paths in &lab.paths {
        with_store(paths, |session, store, scope| {
            let log = Log::read_with(session, store, scope, decode)?;
            for (_, command) in log.eligible() {
                assert!(
                    !command
                        .encode()?
                        .windows(SENTINEL.len())
                        .any(|bytes| bytes == SENTINEL)
                );
            }
            Ok(())
        })
        .unwrap();
    }
    let (left, right, controls) = round(&lab, true);
    for (repair, control) in [(&left, controls[0]), (&right, controls[1])] {
        assert_eq!(repair.handoff(), Some(&Ok(0)));
        assert_eq!(
            crate::runtime::Outcome::of(true, repair, Some(control)),
            crate::runtime::Outcome::Repaired { transferred: false },
            "an empty credential handoff must yield to ordinary discovery"
        );
    }
    client::sync(&lab.paths[2], None).unwrap();
    assert_eq!(
        fsx::read_private(&lab.paths[2].home.join(".secrets/fixture_password"), 1024)
            .unwrap()
            .unwrap()
            .as_slice(),
        SENTINEL
    );
}

#[test]
fn excluded_requesters_do_not_open_a_persons_input() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    need::test_alab_policy(&lab.paths[1]);
    need::raise(
        &lab.paths[1],
        Want::Mailbox("fixture".into()),
        Purpose::ByHand,
    )
    .unwrap();
    pass(&lab.paths[1]).unwrap();
    repair_commons(&lab, 3);
    // The first Mac's unrelated fixture vault is not ALab; its compiled policy permits none.
    assert!(
        give_from(&lab.paths[0], "mailbox:fixture", || panic!(
            "policy must be decided before input is read"
        ))
        .is_err()
    );
}

#[test]
fn the_compiled_mailbox_source_sends_only_that_labels_two_entries() {
    let lab = fixture();
    let holder = &lab.paths[1];
    fsx::ensure_private_dir(&holder.home.join(".secrets")).unwrap();
    fsx::write_atomic(
        &holder.home.join(".secrets/fixture_password"),
        SENTINEL,
        0o600,
    )
    .unwrap();
    fsx::write_atomic(
        &holder.home.join(".secrets/fixture_totp"),
        b"FIXTURESEED",
        0o600,
    )
    .unwrap();
    fsx::write_atomic(
        &holder.home.join(".secrets/other_password"),
        b"never-send",
        0o600,
    )
    .unwrap();
    pass(holder).unwrap();
    repair_commons(&lab, 3);
    let (left, right, _) = round(&lab, true);
    assert_eq!(left.handoff(), Some(&Ok(1)));
    assert_eq!(right.handoff(), Some(&Ok(1)));
    let admin = client::admin_cached(&lab.paths[0]).unwrap();
    let requester = Audience::Hosts(vec!["m3".into()]);
    assert_eq!(
        admin
            .doc
            .entry(&requester, "fixture_totp")
            .unwrap()
            .value
            .as_slice(),
        b"FIXTURESEED"
    );
    assert!(
        admin
            .doc
            .entries()
            .iter()
            .all(|entry| entry.name != "other_password")
    );
}

#[test]
fn custody_and_current_policy_are_checked_before_the_source_is_read() {
    let lab = fixture();
    give_from(&lab.paths[1], "mailbox:fixture", || {
        Ok(SecretBytes::from(SENTINEL))
    })
    .unwrap();
    repair_commons(&lab, 3);
    let custodian_owner = CredentialOwner::for_test(&lab.paths[0]);
    let holder_owner = CredentialOwner::for_test(&lab.paths[1]);
    let config = lab.paths[1].home.join("config.toml");
    let network = lab.bootstrap(0).trust().network;
    let actor = |n| lab.bootstrap(n).request().unwrap().binding().actor();
    let custodian = Round {
        paths: &lab.paths[0],
        config: &config,
        owner: &custodian_owner,
        network,
        local: actor(0),
        peer: actor(1),
    };
    let holder = Round {
        paths: &lab.paths[1],
        config: &config,
        owner: &holder_owner,
        network,
        local: actor(1),
        peer: actor(0),
    };
    let admin = client::admin_cached(&lab.paths[0]).unwrap();
    let (request, offer) = custodian.candidate(&admin).unwrap().unwrap();
    let admission = holder.admit(request, offer, actor(1)).unwrap();
    let mut exchange = Exchange {
        vault: admission.vault,
        network,
        request,
        offer,
        custodian_nonce: [1; 32],
        holder_nonce: [2; 32],
        custodian: actor(0),
        holder: actor(1),
        recipient: admission.recipient,
    };
    let wrong_signer = exchange.prove(&[98; 32]).unwrap();
    assert!(
        holder_delivery(&holder, &exchange, &wrong_signer, |_| panic!(
            "a noncustodian cannot read a source"
        ))
        .is_err()
    );
    let admin = client::admin_cached(&lab.paths[0]).unwrap();
    exchange.recipient = age::Identity::from_secret_bytes([99; 32]).recipient();
    let redirected = admin.prove_credential_custody(&exchange).unwrap();
    assert!(
        holder_delivery(&holder, &exchange, &redirected, |_| panic!(
            "a signed wrong recipient cannot read a source"
        ))
        .is_err()
    );
    exchange.recipient = admission.recipient;
    let proof = admin.prove_credential_custody(&exchange).unwrap();
    let delivered = holder_delivery(&holder, &exchange, &proof, |_| {
        Ok(Values(vec![(
            "fixture_password".into(),
            SecretBytes::from(SENTINEL),
        )]))
    })
    .unwrap();
    assert!(
        !delivered
            .windows(SENTINEL.len())
            .any(|bytes| bytes == SENTINEL)
    );
    fsx::write_atomic(&config, b"[commons]\nenabled = false\n", 0o600).unwrap();
    assert!(
        holder_delivery(&holder, &exchange, &proof, |_| panic!(
            "withdrawn policy cannot read a source"
        ))
        .is_err()
    );
}

#[test]
fn a_give_is_idempotent_and_expires_even_after_the_request_disappears() {
    let lab = fixture();
    give_from(&lab.paths[1], "mailbox:fixture", || {
        Ok(SecretBytes::from(SENTINEL))
    })
    .unwrap();
    give_from(&lab.paths[1], "mailbox:fixture", || {
        Ok(SecretBytes::from(SENTINEL))
    })
    .unwrap();
    assert!(
        give_from(&lab.paths[1], "mailbox:fixture", || Ok(SecretBytes::from(
            b"different".as_slice()
        )))
        .is_err()
    );
    let file_count = || {
        std::fs::read_dir(&lab.paths[1].dir)
            .unwrap()
            .filter(|file| {
                file.as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with("credential-give-")
            })
            .count()
    };
    assert_eq!(file_count(), 1);
    source::cleanup(
        &lab.paths[1],
        lab.bootstrap(1).trust().network,
        &Book::default(),
        now_ms().saturating_add(need::WANT_WINDOW_SECS * MS + 1_000),
    )
    .unwrap();
    assert_eq!(file_count(), 0);
}

#[test]
fn an_undelivered_offer_waits_for_its_window_then_uses_the_holder_away_fallback() {
    let lab = fixture();
    give_from(&lab.paths[1], "mailbox:fixture", || {
        Ok(SecretBytes::from(SENTINEL))
    })
    .unwrap();
    repair_commons(&lab, 3);
    // All members have swept, but a concrete holder still has the whole delivery window.
    pass(&lab.paths[0]).unwrap();
    let now = now_ms();
    let immediate = custodian_pass_at(&lab.paths[0], now).unwrap().unwrap();
    assert!(immediate.asked.is_empty());
    assert_eq!(immediate.waiting.len(), 1);
    let expired = custodian_pass_at(&lab.paths[0], now + need::WANT_WINDOW_SECS * MS + 1_000)
        .unwrap()
        .unwrap();
    assert_eq!(expired.asked.len(), 1);
    with_store(&lab.paths[0], |session, store, scope| {
        let log = Log::read_with(session, store, scope, decode)?;
        assert!(log.eligible().iter().any(|(_, command)| {
            record_of(command).is_ok_and(|record| {
                record.kind == "needs-person"
                    && record.body.get("template").and_then(Value::as_str) == Some("holder-away")
            })
        }));
        Ok(())
    })
    .unwrap();
}

#[test]
fn disabled_local_sharing_refuses_give_before_reading_its_input() {
    let lab = fixture();
    let config = lab.paths[1].dir.parent().unwrap().join("aterm.toml");
    fsx::write_atomic(&config, b"[commons]\nenabled = false\n", 0o600).unwrap();
    assert!(
        give_from(&lab.paths[1], "mailbox:fixture", || panic!(
            "disabled sharing must not open input"
        ))
        .is_err()
    );
}

#[test]
fn admin_give_fills_each_requesters_own_section_and_refuses_a_different_existing_value() {
    let lab = fixture();
    let first = give_from(&lab.paths[0], "mailbox:fixture", || {
        Ok(SecretBytes::from(SENTINEL))
    })
    .unwrap();
    assert_eq!((first.published, first.retained), (1, 0));
    let seq = client::admin_cached(&lab.paths[0]).unwrap().seq();
    need::raise(
        &lab.paths[1],
        Want::Mailbox("fixture".into()),
        Purpose::ByHand,
    )
    .unwrap();
    pass(&lab.paths[1]).unwrap();
    repair_commons(&lab, 3);
    assert!(
        give_from(&lab.paths[0], "mailbox:fixture", || Ok(SecretBytes::from(
            b"different-scratch-password".as_slice()
        )))
        .is_err()
    );
    assert_eq!(client::admin_cached(&lab.paths[0]).unwrap().seq(), seq);
    let second = give_from(&lab.paths[0], "mailbox:fixture", || {
        Ok(SecretBytes::from(SENTINEL))
    })
    .unwrap();
    assert_eq!(second.published, 1);
    let admin = client::admin_cached(&lab.paths[0]).unwrap();
    for host in ["m2", "m3"] {
        assert_eq!(
            admin
                .doc
                .entry(&Audience::Hosts(vec![host.into()]), "fixture_password")
                .unwrap()
                .value
                .as_slice(),
            SENTINEL
        );
    }
    assert!(
        admin
            .doc
            .entry(&Audience::Hosts(vec!["m1".into()]), "fixture_password")
            .is_none()
    );
    aterm_secrets::text::Document::parse(admin.doc.render().as_str().unwrap()).unwrap();
}
