// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::init_secrets::tests::{DROPPED, Scripted, configured, unconfigured};
use crate::items::defaults::tests::{ctx, user};
use aterm_secrets::alab_join::JoinPlace;
use aterm_secrets::client::JoinOutcome;

const ALLOWED: Gate = Gate {
    off: None,
    owed: false,
    broker: None,
};

fn sys() -> crate::port::Fake {
    let mut sys = crate::port::Fake::root();
    sys.euid = 501;
    sys
}

/// A join over `ops` as the window's pass makes it.
fn joined(ops: &Arc<Scripted>) -> Result<PeersDone, String> {
    let port: Arc<dyn SecretsOps> = ops.clone();
    join(
        &sys(),
        &ctx(&[user("alice", 501)]),
        port,
        &mut crate::engine::NoJournal,
        &|| Ok(()),
    )
}

fn observed(ops: &Arc<Scripted>, gate: &Gate) -> PeersRead {
    read(ops.as_ref(), gate, true).expect("a local read")
}

fn pending() -> JoinOutcome {
    JoinOutcome::Pending {
        code: "abcd-ef23-ghij-k234".into(),
        host: "m22".into(),
    }
}

#[test]
fn native_recovery_is_eligible_without_a_vault_preflight() {
    let ops = Scripted::new(vec![Ok(unconfigured())]);
    let prepared = read(ops.as_ref(), &ALLOWED, true).unwrap();
    assert!(matches!(prepared, PeersRead::Joinable(_)), "{prepared:?}");
    assert_eq!(ops.probes(), 0);
    assert!(ops.joins().is_empty());
    assert_eq!(ops.installs(), 0);
}

#[test]
fn unconfirmed_native_admission_keeps_its_automatic_wait_instead_of_failing() {
    for saved in [None, Some(configured(Membership::Pending))] {
        let ops = Scripted::new(vec![Ok(unconfigured())]);
        ops.bootstraps_as(aterm_secrets::client::NativeAdmission::Uncertain, saved);
        assert!(matches!(observed(&ops, &ALLOWED), PeersRead::Joinable(_)));
        let done = joined(&ops);
        assert!(
            matches!(&done, Ok(PeersDone::Deferred(StoodAside::Waiting(why)))
            if why.contains(crate::init_secrets::NATIVE_ADMISSION_PENDING)),
            "{done:?}"
        );
        assert_eq!(ops.installs(), 1);
        assert_eq!(ops.probes(), 0);
        assert!(ops.joins().is_empty());
    }
}

#[test]
fn a_new_setup_hold_reaches_the_background_native_publication_guard() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let stopped = Arc::new(AtomicBool::new(false));
    let ops = Scripted::new(vec![Ok(unconfigured())]);
    ops.stops_during_bootstrap(Arc::clone(&stopped));
    let current = || {
        if stopped.load(Ordering::SeqCst) {
            Err("setup stopped during the background proof".to_owned())
        } else {
            Ok(())
        }
    };
    let done = join(
        &sys(),
        &ctx(&[user("alice", 501)]),
        ops.clone(),
        &mut crate::engine::NoJournal,
        &current,
    );
    assert!(
        matches!(&done, Ok(PeersDone::Deferred(StoodAside::Waiting(why)))
        if why.contains("setup stopped")),
        "{done:?}"
    );
    assert_eq!(ops.installs(), 0);
    assert_eq!(ops.probes(), 0);
    assert!(ops.joins().is_empty());
}

#[test]
fn subscriber_off_after_the_read_prevents_background_enrollment() {
    for during_native in [false, true] {
        let ops = Scripted::new(vec![Ok(unconfigured())]);
        ops.alab_answers();
        ops.joins_as(
            Some(configured(Membership::In { seq: 7, heard: 1 })),
            Ok(JoinOutcome::Joined {
                seq: 7,
                names: Vec::new(),
            }),
        );
        assert!(matches!(observed(&ops, &ALLOWED), PeersRead::Joinable(_)));
        if during_native {
            ops.turns_off_during_bootstrap();
        } else {
            ops.turn_off();
        }
        let done = joined(&ops);
        assert!(
            matches!(&done, Ok(PeersDone::Deferred(StoodAside::Waiting(_)))),
            "{done:?}"
        );
        assert_eq!(ops.installs(), 0);
        assert_eq!(ops.probes(), 0);
        assert!(ops.joins().is_empty());
        assert_eq!(ops.status().unwrap().membership, Membership::Unconfigured);
    }
}

#[test]
fn an_absent_native_miss_keeps_its_retry_class_and_a_real_join_failure_stays_failed() {
    // The vault's address answered that it holds none: a stand-aside of its own kind, still
    // temporary (exit 69), never a failure.
    let ops = Scripted::new(vec![Ok(unconfigured())]);
    let done = joined(&ops);
    assert!(
        matches!(&done, Ok(PeersDone::Deferred(aside @ StoodAside::Absent(_)))
            if aside.exit_status() == 69),
        "{done:?}"
    );
    assert_eq!(ops.installs(), 1);
    assert_eq!(ops.probes(), 1);
    assert!(ops.joins().is_empty());

    let ops = Scripted::new(vec![Ok(unconfigured())]);
    ops.alab_answers();
    ops.joins_as(
        None,
        Err(aterm_secrets::Error::new("credential request refused")),
    );
    let why = joined(&ops).unwrap_err();
    assert!(why.contains("credential request refused"), "{why}");
    assert_eq!(ops.joins().len(), 1);
}

/// An unjoined Mac whose look hears the open ALab vault reads joinable, and its join sends
/// no password and nothing a person types, at the ALab vault's address, and is let in
/// by its completed join operation.
#[test]
fn an_open_vault_joins_with_nothing_typed() {
    let ops = Scripted::new(vec![Ok(unconfigured())]);
    ops.alab_answers();
    let read = observed(&ops, &ALLOWED);
    assert!(matches!(read, PeersRead::Joinable(_)), "{read:?}");
    assert_eq!(ops.probes(), 0);

    ops.joins_as(
        Some(configured(Membership::In { seq: 7, heard: 1 })),
        Ok(JoinOutcome::Joined {
            seq: 7,
            names: vec!["gh_access_token".into()],
        }),
    );
    let done = joined(&ops);
    assert!(matches!(done, Ok(PeersDone::Joined(_))), "{done:?}");
    assert_eq!(
        ops.joins(),
        [(
            aterm_types::macos_setup::DEFAULT_VAULT_URL.to_owned(),
            false
        )],
        "one join, nobody asked"
    );
    assert_eq!(ops.passwords(), [false], "no password sent");
}

/// A vault that asks a join password (or an admin's approval) does not let this Mac in: the
/// join leaves it waiting, and every later read is deferred, so no pass joins it again.
/// NEGATIVE CONTROL: a read that takes a pending membership for joinable joins it again.
#[test]
fn a_vault_that_asks_a_password_is_deferred_and_never_joined_again() {
    let ops = Scripted::new(vec![Ok(unconfigured())]);
    ops.alab_answers();
    ops.joins_as(Some(configured(Membership::Pending)), Ok(pending()));
    let done = joined(&ops);
    assert!(
        matches!(done, Ok(PeersDone::Deferred(StoodAside::Waiting(_)))),
        "{done:?}"
    );
    assert_eq!(ops.passwords(), [false], "no password sent");

    for _ in 0..2 {
        let read = observed(&ops, &ALLOWED);
        assert!(
            matches!(&read, PeersRead::Deferred(why) if why.contains("join password")),
            "{read:?}"
        );
    }
    assert_eq!(
        ops.probes(),
        1,
        "only the original join looks for the fallback vault"
    );
    assert_eq!(ops.joins().len(), 1, "never joined again");
}

/// A Mac the ALab vault has not let in yet is told what lets it in where it is (or to give a
/// password), in the join's answer and in every later read: the automatic admission's words on
/// the ALab LAN, and off it the words that promise none; another vault's Mac is told its vault
/// asks a password or an approval. No pass joins either again.
/// NEGATIVE CONTROLS: a [`waits_for`] that ignores the vault tells the ALab Mac its vault asks
/// a password or an approval; `not_let_in` giving `LET_IN` wherever this Mac is fails at
/// "off the ALab LAN, no automatic admission".
#[test]
fn a_pending_alab_join_waits_for_the_vaults_approver() {
    let alab_pending = || {
        let mut alab = configured(Membership::Pending);
        let signer: [u8; 32] = aterm_secrets::b64::decode_padded(aterm_secrets::ALAB_SIGNER)
            .and_then(|key| key.try_into().ok())
            .expect("the ALab signer");
        alab.config.as_mut().expect("a config").signer = Some(signer);
        alab
    };
    let alab = alab_pending();
    let other = configured(Membership::Pending);
    for (status, waits) in [(alab, Waits::Approver), (other, Waits::PasswordOrApproval)] {
        let ops = Scripted::new(vec![Ok(unconfigured())]);
        ops.alab_answers();
        ops.joins_as(Some(status.clone()), Ok(pending()));
        let done = joined(&ops);
        assert!(
            matches!(done, Ok(PeersDone::Deferred(StoodAside::Waiting(_)))),
            "{waits:?}: {done:?}"
        );
        let ops = Scripted::new(vec![Ok(status)]);
        assert_eq!(
            observed(&ops, &ALLOWED),
            PeersRead::Deferred(not_let_in(
                "vault alab",
                (waits, &|| JoinPlace::Home),
                "no pass joins it again"
            )),
            "{waits:?}"
        );
        assert!(ops.joins().is_empty(), "no pass joins it again");
    }
    let ops = Scripted::new(vec![Ok(alab_pending())]);
    ops.placed(JoinPlace::Away);
    let read = observed(&ops, &ALLOWED);
    assert!(
        matches!(&read, PeersRead::Deferred(why)
            if why.contains(aterm_secrets::alab_join::LET_IN_AWAY)
                && !why.contains(crate::init_secrets::LET_IN)),
        "off the ALab LAN, no automatic admission: {read:?}"
    );
}

/// A Mac a rotation dropped from its vault with no `Remove` (one-mesh E2) reads deferred with
/// its words, the way back as a new Mac, not the removal's "press Allow again": no pass joins
/// it again, and none looks.
/// NEGATIVE CONTROL: a `Revoked` arm that ignores `SecretsOps::dropped` tells it to press
/// Allow again, which every member that read the rotation refuses for its key.
#[test]
fn a_dropped_mac_reads_deferred_with_how_it_comes_back() {
    let ops = Scripted::new(vec![Ok(configured(Membership::Revoked { seq: 9 }))]);
    ops.drops(DROPPED);
    assert_eq!(
        observed(&ops, &ALLOWED),
        PeersRead::Deferred(format!("vault alab: {DROPPED}; no pass joins it again"))
    );
    assert_eq!(ops.dropped_asks(), [9]);
    assert_eq!(ops.probes(), 0);
    assert!(ops.joins().is_empty(), "no pass joins it");
}

/// A Mac removed from its vault reads deferred, naming the removal, not joined: no
/// pass joins it again, and none reports it in.
/// NEGATIVE CONTROL: a read that takes every membership but a pending one for joined reads
/// the removed Mac as in its vault.
#[test]
fn a_mac_an_admin_removed_reads_deferred_not_joined() {
    let ops = Scripted::new(vec![Ok(configured(Membership::Revoked { seq: 9 }))]);
    let read = observed(&ops, &ALLOWED);
    let PeersRead::Deferred(why) = &read else {
        panic!("{read:?}");
    };
    assert!(
        why.contains("this Mac was removed from vault alab"),
        "{why}"
    );
    assert_eq!(ops.probes(), 0);
    assert!(ops.joins().is_empty(), "no pass joins it");
    let ops = Scripted::new(vec![Ok(configured(Membership::In { seq: 9, heard: 1 }))]);
    assert_eq!(
        observed(&ops, &ALLOWED),
        PeersRead::Joined("in vault alab".to_owned())
    );
}

/// While the LAN broker does not answer no pass looks: the read waits for the broker,
/// deferred and naming why, and looks once it answers (a look macOS refused waits for the
/// next); nor is anything looked for by a pass other than the window's. NEGATIVE CONTROL: a
/// read that ignores `broker` looks.
#[test]
fn no_look_runs_until_the_lan_broker_answers() {
    let ops = Scripted::new(vec![Ok(unconfigured())]);
    let absent = Gate {
        broker: Some(aterm_uds::lan::Unavailable::NotInstalled.to_string()),
        ..ALLOWED
    };
    for _ in 0..2 {
        assert_eq!(
            observed(&ops, &absent),
            PeersRead::Deferred(format!(
                "{}; {BROKER_WAIT}",
                aterm_uds::lan::Unavailable::NotInstalled
            ))
        );
    }
    assert!(matches!(observed(&ops, &ALLOWED), PeersRead::Joinable(_)));
    assert_eq!(
        read(ops.as_ref(), &ALLOWED, false),
        Ok(PeersRead::Deferred(NOT_LOOKED.to_owned()))
    );
    assert_eq!(ops.probes(), 0);
    assert_eq!(ops.installs(), 0);
    assert!(ops.joins().is_empty());
}

/// A person's choice keeps the vault out before any effect: the subscriber turned off, or
/// System State's plan without the Secrets vault.
#[test]
fn a_choice_keeps_automatic_admission_off() {
    let ops = Scripted::new(vec![Ok(unconfigured())]);
    let chosen = Gate {
        off: Some("System State's plan leaves the Secrets vault out".to_owned()),
        ..ALLOWED
    };
    assert!(matches!(observed(&ops, &chosen), PeersRead::Off(_)));
    ops.turn_off();
    assert!(matches!(observed(&ops, &ALLOWED), PeersRead::Off(_)));
    assert_eq!(ops.probes(), 0);
    assert_eq!(ops.installs(), 0);
    assert!(ops.joins().is_empty());
}

/// While a run of System State's owes its Secrets stage, the window's pass leaves the join
/// to that stage: deferred, with no effect; once the stage is done a later pass is eligible.
/// NEGATIVE CONTROL: ignoring `owed` permits admission beside the pending stage.
#[test]
fn a_run_that_owes_its_secrets_stage_keeps_the_join() {
    let ops = Scripted::new(vec![Ok(unconfigured())]);
    let owed = Gate {
        owed: true,
        ..ALLOWED
    };
    assert_eq!(
        observed(&ops, &owed),
        PeersRead::Deferred(STAGE_OWED.to_owned())
    );
    assert_eq!(ops.probes(), 0);
    assert!(matches!(observed(&ops, &ALLOWED), PeersRead::Joinable(_)));
}
