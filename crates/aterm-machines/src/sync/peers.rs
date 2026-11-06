// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `sync.peers`: a Mac with no vault joins the open ALab vault from the window's pass, with
//! nothing typed (docs/DESIGN-init-on-by-default-2026-10-25.md §8.5). It reaches a Mac whose
//! first launch finished without a join: the vault was asleep, or this Mac was on another
//! network.
//!
//! The join is System State's own: its Secrets stage's vault item over the unattended port
//! ([`crate::init::run_initial_secrets_with`]'s, as `crate::init::run_converge_join` runs it),
//! at the ALab vault's address, with no password and replacing only the "no vault" the read saw,
//! leaving confirmation to the subscriber. The subscriber it puts in place
//! is a repair, never a person's On, so a turn-off recorded since the read stays. The read is
//! local: the join tries authenticated members before any vault lookup. An unconfirmed native
//! exchange or an unavailable vault retains its typed wait and the subscriber's continuation.
//! A vault that asks a join password or an admin's approval
//! keeps the request and this Mac waits: no pass joins it again (it reads deferred), and the
//! subscriber the join installed finishes it once a person gives the rest, as after System
//! State's own join. The look reaches the LAN through the root LAN broker (`aterm_uds::lan`), so
//! it runs only while the broker answers; until then the read waits for it, never for a person.

use super::{PeersDone, PeersRead};
use crate::engine::Journal;
use crate::init_secrets::{Replaces, SecretsOps};
use crate::items::Ctx;
use crate::port::System;
use aterm_converge::StoodAside;
use aterm_secrets::client::Membership;
use std::sync::Arc;

/// What stands before a join besides the vault: a person's choice that keeps it out (System
/// State's plan leaving the Secrets vault out, or a system System State does not run on), a run
/// of System State's that still owes its Secrets stage, and a LAN broker that does not answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gate {
    /// Why no vault is joined here, where a choice says so.
    pub off: Option<String>,
    /// A run System State admitted still owes its Secrets stage, which joins the vault itself
    /// (`aterm_cli::macos_doctor::initial_setup_owes_secrets_stage`).
    pub owed: bool,
    /// Why the LAN broker, through which a look reaches the LAN on macOS, does not answer
    /// (`aterm_uds::lan::broker::hello`'s error); `None` where it answers, or where the system
    /// needs none.
    pub broker: Option<String>,
}

/// What `sync.peers` adds to why the LAN broker does not answer ([`Gate::broker`]).
pub const BROKER_WAIT: &str = "no pass looks for the ALab vault until it answers";

/// What it says while a run of System State's still owes its Secrets stage.
pub const STAGE_OWED: &str = "System State's run has its Secrets stage still to do, and that \
     stage joins the vault: no pass joins beside it";

/// What it says to a pass that does not look: only the window's does.
pub const NOT_LOOKED: &str = "no vault is configured: the aterm window's pass looks for the \
     ALab vault and joins it";

/// `sync.peers`' read over `ops`, the vault as System State's Secrets stage reads it. A vault
/// configured here reads joined once it let this Mac in, and deferred while it has not (it waits
/// for a member's Allow, a join password or an admin's approval, which a pass never gives;
/// [`not_let_in`]) or once this Mac was removed from it, or a rotation dropped it (its words say
/// to set the vault folder aside and come back as a new Mac): a pass never joins it again.
/// With none, the person's turn-off or `gate`'s choice reads off, and a run of System State's
/// that owes its Secrets stage, or a LAN broker that does not answer, reads deferred without a
/// LAN use. Only an applying window pass (`can_join`) reads eligible for automatic admission;
/// that admission, including discovery and its pinned-vault fallback, belongs to apply alone.
///
/// # Errors
/// The vault's local state cannot be read.
pub fn read(ops: &dyn SecretsOps, gate: &Gate, can_join: bool) -> Result<PeersRead, String> {
    if ops.configured().map_err(|why| why.to_string())?.is_some() {
        let status = ops.status().map_err(|why| why.to_string())?;
        let name = status
            .config
            .as_ref()
            .map_or_else(|| "?".to_owned(), |config| config.name.clone());
        return Ok(match status.membership {
            Membership::In { .. } | Membership::Plain => {
                PeersRead::Joined(format!("in vault {name}"))
            }
            Membership::Pending => PeersRead::Deferred(not_let_in(
                &format!("vault {name}"),
                (waits_for(&status), &|| ops.join_place()),
                "no pass joins it again",
            )),
            Membership::Revoked { seq } => PeersRead::Deferred(match ops.dropped(seq) {
                Some(words) => format!("vault {name}: {words}; no pass joins it again"),
                None => format!(
                    "this Mac was removed from vault {name}{}: to rejoin, run aterm members join \
                     and press Allow again on any Mac in ALab; no pass joins it again",
                    aterm_secrets::client::out_since(seq)
                ),
            }),
            // The configuration went between the two reads: the next pass reads it again.
            Membership::Unconfigured => PeersRead::Deferred(format!(
                "vault {name}'s configuration went while this pass read it; the next pass reads \
                 it again"
            )),
        });
    }
    if ops.turned_off() {
        return Ok(PeersRead::Off(
            "sync is off on this Mac by your choice (`aterm secrets agent on` turns it on)"
                .to_owned(),
        ));
    }
    if let Some(why) = &gate.off {
        return Ok(PeersRead::Off(why.clone()));
    }
    if gate.owed {
        return Ok(PeersRead::Deferred(STAGE_OWED.to_owned()));
    }
    if let Some(why) = &gate.broker {
        return Ok(PeersRead::Deferred(format!("{why}; {BROKER_WAIT}")));
    }
    if !can_join {
        return Ok(PeersRead::Deferred(NOT_LOOKED.to_owned()));
    }
    Ok(PeersRead::Joinable(
        "no vault is configured; ALab admission is ready to try".to_owned(),
    ))
}

/// `sync.peers`' apply: System State's join of the ALab vault over `ops`, with no password,
/// replacing only the "no vault" the read saw, journaled in `journal` and confirmed by its
/// subscriber. In: joined. A retained automatic request, an offline source
/// or a missing prerequisite preserves the stage's typed stand-aside. No second HTTP approval
/// wait follows a native exchange, including one that wrote only part of its local source.
///
/// # Errors
/// The guarded stage failed or its local state cannot be read. Ordinary pending and offline
/// states preserve their typed stand-aside instead.
pub fn join(
    sys: &dyn System,
    ctx: &Ctx,
    ops: Arc<dyn SecretsOps>,
    journal: &mut dyn Journal,
    current: &(dyn Fn() -> Result<(), String> + Sync),
) -> Result<PeersDone, String> {
    let rows = crate::init::run_converge_join(
        sys,
        ctx,
        Arc::clone(&ops),
        aterm_types::macos_setup::DEFAULT_VAULT_URL,
        Replaces::Read(None),
        journal,
        current,
    )?;
    if let Some(row) = rows.iter().find(|row| row.outcome.is_failure()) {
        return Err(row
            .why
            .clone()
            .unwrap_or_else(|| "the ALab vault join failed".to_owned()));
    }
    if let Some(aside) = rows.iter().find_map(|row| row.stood_aside.as_ref()) {
        return Ok(PeersDone::Deferred(aside.clone()));
    }
    if let Some(row) = rows
        .iter()
        .find(|row| row.outcome == crate::model::Outcome::NotApplicable)
    {
        return Ok(PeersDone::Deferred(StoodAside::Waiting(
            row.why
                .clone()
                .unwrap_or_else(|| "current setup holds background admission".to_owned()),
        )));
    }
    let status = ops.status().map_err(|why| why.to_string())?;
    let name = status.config.as_ref().map_or_else(
        || "the ALab vault".to_owned(),
        |config| format!("vault {}", config.name),
    );
    match status.membership {
        Membership::In { .. } | Membership::Plain => {
            Ok(PeersDone::Joined(format!("joined {name}, nothing typed")))
        }
        Membership::Pending => Ok(PeersDone::Deferred(StoodAside::Waiting(not_let_in(
            &name,
            (waits_for(&status), &|| ops.join_place()),
            "its request stands, and the subscriber confirms it",
        )))),
        Membership::Unconfigured | Membership::Revoked { .. } => {
            Err(format!("{name} was not joined"))
        }
    }
}

/// What a Mac a vault has not let in waits for ([`waits_for`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Waits {
    /// The ALab vault: a member's Allow, with the phrase that member shows typed here
    /// (docs/DESIGN-fluid-swarm-2026-10-28.md §3.3.1), which its approver records once it runs
    /// on the vault's admin Mac; or the join password or an admin's approval by code, which a
    /// Mac cannot tell apart from waiting.
    Approver,
    /// Any other vault: the join password or an admin's approval.
    PasswordOrApproval,
}

/// What the vault `status` names waits for before it lets this Mac in: the ALab vault's Allow
/// first, any other vault's password or approval.
fn waits_for(status: &aterm_secrets::client::Status) -> Waits {
    if status
        .config
        .as_ref()
        .is_some_and(aterm_secrets::client::alab_vault)
    {
        Waits::Approver
    } else {
        Waits::PasswordOrApproval
    }
}

/// What `sync.peers` says of `vault` (`vault alab`) while it has not let this Mac in, waiting
/// for `waits`, then `then`: at the ALab vault, what lets this Mac in where it is (`at`, read
/// only then: `aterm_secrets::alab_join::let_in`).
fn not_let_in(
    vault: &str,
    (waits, at): (Waits, &dyn Fn() -> aterm_secrets::alab_join::JoinPlace),
    then: &str,
) -> String {
    match waits {
        Waits::Approver => format!(
            "{vault} has not let this Mac in yet: {}, or give the join password, which System \
             State's Secrets row or `aterm init` takes; {then}",
            aterm_secrets::alab_join::let_in(at())
        ),
        Waits::PasswordOrApproval => format!(
            "{vault} has not let this Mac in: it asks a join password or an admin's approval, \
             which System State's Secrets row or `aterm init` gives; {then}"
        ),
    }
}

#[cfg(test)]
mod tests {
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
    fn an_offline_native_miss_keeps_its_retry_class_and_a_real_join_failure_stays_failed() {
        let ops = Scripted::new(vec![Ok(unconfigured())]);
        let done = joined(&ops);
        assert!(
            matches!(&done, Ok(PeersDone::Deferred(StoodAside::Offline(_)))),
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
}
