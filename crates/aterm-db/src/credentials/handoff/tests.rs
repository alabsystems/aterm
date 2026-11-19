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

/// `body`, with `flag` (a mutant or seam) on for this thread while it runs.
fn under<T>(
    flag: Option<&'static std::thread::LocalKey<std::cell::Cell<bool>>>,
    body: impl FnOnce() -> T,
) -> T {
    if let Some(flag) = flag {
        flag.with(|flag| flag.set(true));
    }
    let out = body();
    if let Some(flag) = flag {
        flag.with(|flag| flag.set(false));
    }
    out
}

fn round(lab: &Lab, permits: bool) -> (Repair, Repair, [crate::control::Report; 2]) {
    round_with_owner(lab, permits, None)
}

fn round_with_owner(
    lab: &Lab,
    permits: bool,
    custodian_owner: Option<CredentialOwner>,
) -> (Repair, Repair, [crate::control::Report; 2]) {
    round_on_side(lab, permits, custodian_owner, Side::Initiator)
}

fn round_on_side(
    lab: &Lab,
    permits: bool,
    custodian_owner: Option<CredentialOwner>,
    custodian_side: Side,
) -> (Repair, Repair, [crate::control::Report; 2]) {
    round_over(lab, permits, custodian_owner, custodian_side, |endpoint| {
        endpoint
    })
}

/// [`round_on_side`], each endpoint built as `wire` makes it (the protocol its dial offers).
fn round_over(
    lab: &Lab,
    permits: bool,
    custodian_owner: Option<CredentialOwner>,
    custodian_side: Side,
    wire: fn(Endpoint) -> Endpoint,
) -> (Repair, Repair, [crate::control::Report; 2]) {
    let holder_side = match custodian_side {
        Side::Initiator => Side::Responder,
        Side::Responder => Side::Initiator,
    };
    let endpoint = |n| {
        let endpoint = wire(
            Endpoint::from_bootstrap(&lab.bootstrap(n))
                .unwrap()
                .unwrap()
                .with_commons(lab.paths[n].clone(), lab.paths[n].home.join("config.toml"))
                .unwrap(),
        );
        if permits {
            let owner = if n == 0 {
                custodian_owner.clone()
            } else {
                None
            }
            .unwrap_or_else(|| CredentialOwner::for_test(&lab.paths[n]));
            endpoint.with_credential_owner(owner, lab.paths[n].home.join("config.toml"))
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
                Connection::admit(
                    aterm_uds::accept(
                        &listener,
                        aterm_uds::AcceptWait::Forever,
                        aterm_uds::AcceptedMode::Blocking,
                    )
                    .unwrap()
                    .0,
                    LinkScope::Loopback,
                )
                .unwrap(),
                holder_side,
                &mut right,
            )
        });
        let left = custodian
            .exchange(
                Connection::connect(address, LinkScope::Loopback, Duration::from_secs(60)).unwrap(),
                custodian_side,
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
fn fresh_vaults_with_no_requests_complete_an_empty_handoff() {
    for foundation in [false, true] {
        let roles = [Role::Writer, Role::Writer];
        let lab = if foundation {
            Lab::with_foundation(&roles)
        } else {
            Lab::new(&roles)
        };
        let request_paths: Vec<_> = lab
            .paths
            .iter()
            .enumerate()
            .map(|(n, paths)| {
                let bootstrap = lab.bootstrap(n);
                crate::catalog::Directory::new(paths, bootstrap.trust())
                    .path(descriptor(&bootstrap).unwrap().scope)
                    .unwrap()
            })
            .collect();
        assert!(request_paths.iter().all(|path| !path.exists()));
        let (left, right, _) = round(&lab, true);
        for repair in [&left, &right] {
            assert_eq!(repair.handoff(), Some(&Ok(0)), "foundation={foundation}");
        }
        assert!(request_paths.iter().all(|path| !path.exists()));
    }
}

#[test]
fn declared_missing_or_incompatible_requests_do_not_look_empty() {
    for version in [1, 2] {
        let lab = Lab::new(&[Role::Writer]);
        let bootstrap = lab.bootstrap(0);
        let directory = crate::catalog::Directory::new(&lab.paths[0], bootstrap.trust());
        let mut declared = descriptor(&bootstrap).unwrap();
        declared.version = version;
        {
            let session = Session::open(&bootstrap).unwrap();
            directory.publish(&session, &declared).unwrap();
        }
        let store = directory.path(declared.scope).unwrap();
        if version == 1 {
            std::fs::rename(&store, store.with_extension("preserved")).unwrap();
        }
        let observed = crate::documents::application::observe_store_if_declared(
            &lab.paths[0],
            descriptor,
            |_, _, _| -> Result<()> { panic!("unavailable request data must not be read") },
        );
        assert!(observed.is_err(), "declared version {version}");
    }
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
        client::read_in_place(&lab.paths[2], "fixture_password")
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

/// A label's values this Mac's store holds are the vault's own: the holder offers none of them
/// and a handoff round takes nothing, and the custodian copies the label's two entries to the
/// requester itself, never another entry the holder's store holds.
/// NEGATIVE CONTROL: with the holder's probe offering a value its store holds (`holds` reading it
/// in place), the pass offers it: `` left: 1 right: 0 ``.
#[test]
fn a_label_the_store_holds_is_copied_by_the_custodian_with_its_two_entries_only() {
    let lab = fixture();
    let holder = &lab.paths[1];
    let admin = client::admin_open(&lab.paths[0]).unwrap();
    let mut doc = admin.doc.clone();
    let m2 = Audience::Hosts(vec!["m2".into()]);
    for (name, value) in [
        ("fixture_password", SENTINEL),
        ("fixture_totp", b"FIXTURESEED".as_slice()),
        ("other_password", b"never-send".as_slice()),
    ] {
        doc.set_entry(&m2, name, value).unwrap();
    }
    client::admin_publish(&lab.paths[0], &admin, &doc).unwrap();
    client::sync(holder, None).unwrap();
    assert!(
        client::read_in_place(holder, "fixture_password")
            .unwrap()
            .is_some()
    );
    assert_eq!(pass(holder).unwrap().offered, 0);
    repair_commons(&lab, 3);
    let (left, right, _) = round(&lab, true);
    assert_eq!(left.handoff(), Some(&Ok(0)));
    assert_eq!(right.handoff(), Some(&Ok(0)));
    pass(&lab.paths[0]).unwrap();
    custodian_pass_at(&lab.paths[0], now_ms()).unwrap().unwrap();
    let admin = client::admin_cached(&lab.paths[0]).unwrap();
    let requester = Audience::Hosts(vec!["m3".into()]);
    for (name, value) in [
        ("fixture_password", SENTINEL),
        ("fixture_totp", b"FIXTURESEED".as_slice()),
    ] {
        assert_eq!(
            admin.doc.entry(&requester, name).unwrap().value.as_slice(),
            value
        );
    }
    assert!(admin.doc.entry(&requester, "other_password").is_none());
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
    let (request, offer, _) = custodian.candidate(&admin, true).unwrap().unwrap();
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
fn an_undelivered_offer_waits_for_its_window_then_retires_not_held() {
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
    assert!(immediate.retired.is_empty());
    assert_eq!(immediate.waiting.len(), 1);
    let expired = custodian_pass_at(&lab.paths[0], now + need::WANT_WINDOW_SECS * MS + 1_000)
        .unwrap()
        .unwrap();
    assert_eq!(expired.retired.len(), 1);
    assert_eq!(expired.retired[0].1, Reason::NotHeld);
    with_store(&lab.paths[0], |session, store, scope| {
        let log = Log::read_with(session, store, scope, decode)?;
        assert!(log.eligible().iter().all(|(_, command)| {
            record_of(command).is_ok_and(|record| record.kind != "needs-person")
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

#[test]
fn an_interrupted_offer_does_not_block_the_same_owners_next_authenticated_handoff() {
    struct InterruptedHello<'a, 'b>(&'a Round<'b>);
    impl std::io::Read for InterruptedHello<'_, '_> {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            panic!("the injected first write must fail before a read");
        }
    }
    impl std::io::Write for InterruptedHello<'_, '_> {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            let mut competing = std::io::Cursor::new(Vec::new());
            assert_eq!(take(self.0, &mut competing).unwrap(), 0);
            competing.set_position(0);
            assert_eq!(receive(&mut competing, 4).unwrap(), b"AQN1");
            Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "fixture interrupted hello",
            ))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let lab = fixture();
    give_from(&lab.paths[1], "mailbox:fixture", || {
        Ok(SecretBytes::from(SENTINEL))
    })
    .unwrap();
    repair_commons(&lab, 3);
    let owner = CredentialOwner::for_test(&lab.paths[0]);
    let config = lab.paths[0].home.join("config.toml");
    let actor = |n| lab.bootstrap(n).request().unwrap().binding().actor();
    {
        let custodian = Round {
            paths: &lab.paths[0],
            config: &config,
            owner: &owner,
            network: lab.bootstrap(0).trust().network,
            local: actor(0),
            peer: actor(1),
        };
        let error = take(&custodian, &mut InterruptedHello(&custodian)).unwrap_err();
        assert!(
            error.to_string().contains("fixture interrupted hello"),
            "{error}"
        );
    }

    let (left, right, _) = round_with_owner(&lab, true, Some(owner));
    assert_eq!(
        left.handoff(),
        Some(&Ok(1)),
        "the same owner can recover immediately"
    );
    assert_eq!(right.handoff(), Some(&Ok(1)));
    let admin = client::admin_cached(&lab.paths[0]).unwrap();
    assert_eq!(
        admin
            .doc
            .entry(&Audience::Hosts(vec!["m3".into()]), "fixture_password")
            .unwrap()
            .value
            .as_slice(),
        SENTINEL
    );
    repair_commons(&lab, 3);
    assert!(
        observe(&lab.paths[2])
            .unwrap()
            .book
            .threads
            .values()
            .any(|thread| thread.published.is_some())
    );
}

#[test]
fn an_incoming_connection_can_deliver_a_holders_offer_to_its_custodian() {
    let lab = fixture();
    give_from(&lab.paths[1], "mailbox:fixture", || {
        Ok(SecretBytes::from(SENTINEL))
    })
    .unwrap();
    repair_commons(&lab, 3);
    let (custodian, holder, _) = round_on_side(&lab, true, None, Side::Responder);
    assert_eq!(custodian.handoff(), Some(&Ok(1)));
    assert_eq!(holder.handoff(), Some(&Ok(1)));
    let admin = client::admin_cached(&lab.paths[0]).unwrap();
    assert_eq!(
        admin
            .doc
            .entry(&Audience::Hosts(vec!["m3".into()]), "fixture_password")
            .unwrap()
            .value
            .as_slice(),
        SENTINEL
    );
    repair_commons(&lab, 3);
    assert!(
        observe(&lab.paths[2])
            .unwrap()
            .book
            .threads
            .values()
            .any(|thread| thread.published.is_some())
    );
}

/// A Mac that administers no vault takes nothing, ever, so it owes its peer no round and no
/// judgement whatever its round could not run: the holder that answers its custodian's round
/// tells `Owing::Nothing`, though its cached administrator does not open, and so does a
/// judgement of it made with no round (`owed_take`). The custodian's own judgement still finds
/// the holder's Offer, and its own round takes it.
/// MUTANT (`NO_ADMIN_UNTOLD`): a cached administrator that does not open tells `Owing::Unknown`
/// on every Mac, so each member but the custodian judges each one-way round's take again.
#[test]
fn a_mac_that_administers_no_vault_owes_its_peer_no_take_and_no_judgement() {
    let lab = fixture();
    give_from(&lab.paths[1], "mailbox:fixture", || {
        Ok(SecretBytes::from(SENTINEL))
    })
    .unwrap();
    repair_commons(&lab, 3);
    assert_eq!(
        [0, 1].map(|n| client::can_administer(&lab.paths[n]).unwrap()),
        [true, false],
        "CONTROL: Mac 0 administers the vault and Mac 1 does not"
    );
    let actor = |n| lab.bootstrap(n).request().unwrap().binding().actor();
    let network = lab.bootstrap(0).trust().network;
    let judged = |n: usize, peer: usize| {
        let owner = CredentialOwner::for_test(&lab.paths[n]);
        owed_take(
            &lab.paths[n],
            &lab.paths[n].home.join("config.toml"),
            &owner,
            network,
            actor(n),
            actor(peer),
        )
    };
    assert!(
        matches!(judged(0, 1), Owing::Take(_)),
        "CONTROL: the custodian's judgement finds the holder's Offer"
    );
    for mutant in [false, true] {
        let owing = under(mutant.then_some(&NO_ADMIN_UNTOLD), || judged(1, 0));
        let expected = if mutant {
            Owing::Unknown
        } else {
            Owing::Nothing
        };
        assert_eq!(owing, expected, "the holder's judgement: mutant {mutant}");
    }
    let (custodian, holder, _) = round_on_side(&lab, true, None, Side::Initiator);
    assert_eq!(
        custodian.handoff(),
        Some(&Ok(1)),
        "the custodian's own round takes"
    );
    assert_eq!(
        holder.owing(),
        Owing::Nothing,
        "the answering holder, no custodian, owes nothing"
    );
}

/// An answering custodian whose handoff broke off before its take could run still tells what it
/// owes: the holder's round brought an Offer it would take, so it owes the holder a round
/// (`Owing::Take`), whatever its give met. A take of its own that broke off with no answer from
/// the holder records nothing refused, so the holder's next round owes the Offer again; a take
/// the holder refused (AQR1) does record it.
/// MUTANTS: `OWING_LOST_ON_ERROR` owes nothing for the broken-off round; `REFUSED_AT_TRY` records
/// the Offer refused once the take holds it, before any answer, and the holder's next round owes
/// nothing.
#[test]
fn a_handoff_that_broke_off_still_tells_what_it_owes_and_refuses_nothing() {
    /// A connection whose peer went away: what is sent goes, and nothing comes back.
    struct Gone;
    impl std::io::Read for Gone {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "fixture: the peer went away",
            ))
        }
    }
    impl std::io::Write for Gone {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let lab = fixture();
    give_from(&lab.paths[1], "mailbox:fixture", || {
        Ok(SecretBytes::from(SENTINEL))
    })
    .unwrap();
    repair_commons(&lab, 3);
    let owner = CredentialOwner::for_test(&lab.paths[0]);
    let config = lab.paths[0].home.join("config.toml");
    let actor = |n| lab.bootstrap(n).request().unwrap().binding().actor();
    let network = lab.bootstrap(0).trust().network;
    let offer = Round {
        paths: &lab.paths[0],
        config: &config,
        owner: &owner,
        network,
        local: actor(0),
        peer: actor(1),
    }
    .candidate(&client::admin_cached(&lab.paths[0]).unwrap(), true)
    .unwrap()
    .unwrap()
    .1;
    // The holder's one-way round reaches the custodian, and breaks off in its give.
    let answer = || {
        exchange(
            &lab.paths[0],
            &config,
            &owner,
            network,
            actor(0),
            actor(1),
            Side::Responder,
            false,
            &mut Gone,
        )
    };
    for mutant in [false, true] {
        let handed = under(mutant.then_some(&OWING_LOST_ON_ERROR), answer);
        assert!(handed.count.is_err(), "the give broke off");
        assert_eq!(
            handed.owing,
            if mutant {
                Owing::Nothing
            } else {
                Owing::Take(offer)
            },
            "the broken-off round: mutant {mutant}"
        );
    }
    // The custodian's own take sends its hello, and the holder never answers.
    for mutant in [false, true] {
        under(mutant.then_some(&REFUSED_AT_TRY), || {
            let round = Round {
                paths: &lab.paths[0],
                config: &config,
                owner: &owner,
                network,
                local: actor(0),
                peer: actor(1),
            };
            let error = take(&round, &mut Gone).unwrap_err();
            assert!(error.to_string().contains("went away"), "{error}");
        });
        assert_eq!(
            answer().owing,
            if mutant {
                Owing::Nothing
            } else {
                Owing::Take(offer)
            },
            "a take the holder never answered: mutant {mutant}"
        );
    }
}
