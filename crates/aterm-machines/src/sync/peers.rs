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
#[path = "peers_tests.rs"]
mod tests;
