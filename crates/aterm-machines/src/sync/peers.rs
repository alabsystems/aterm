// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `sync.peers`: a Mac with no vault joins the open ALab vault from the window's pass, with
//! nothing typed (docs/DESIGN-init-on-by-default-2026-10-25.md §8.5). It reaches a Mac whose
//! first launch finished without a join: the vault was asleep, or this Mac was on another
//! network.
//!
//! The join is System State's own: its Secrets stage's vault item over the unattended port
//! ([`crate::init::run_initial_secrets_with`]), at the ALab vault's address, with no password and
//! replacing only the "no vault" the read saw, then a wait of [`JOIN_WAIT`] for the vault to let
//! this Mac in. An open vault does. A vault that asks a join password or an admin's approval
//! keeps the request and this Mac waits: no pass joins it again (it reads deferred), and the
//! subscriber the join installed finishes it once a person gives the rest, as after System
//! State's own join. Nothing asks macOS's Local Network question: the look runs only once macOS
//! asked aterm before, and an answer of Don't Allow is the person's, so no pass looks again.

use super::{PeersDone, PeersRead};
use crate::engine::Journal;
use crate::init_secrets::{Replaces, SecretsOps};
use crate::items::Ctx;
use crate::port::System;
use aterm_secrets::client::Membership;
use std::sync::Arc;
use std::time::Duration;

/// How long a join waits for the vault to let this Mac in, as System State's Secrets stage
/// waits for its approver.
pub const JOIN_WAIT: Duration = Duration::from_secs(20);

/// Local Network as aterm last heard it (a look at the LAN, or the window's own step).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lan {
    /// A look reached the LAN, or macOS asked aterm already: a look raises no question.
    Allowed,
    /// macOS refused a look the LAN: the person answered Don't Allow, or turned aterm off since.
    Refused,
    /// Nothing says, and macOS has not asked aterm: a look could raise its question.
    Untold,
}

/// What a look for the ALab vault on this network heard (System State's own look).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Heard {
    /// The ALab vault answered under the key built into aterm.
    Found,
    /// Something else answered at its address, or a kept look on this network says so.
    Elsewhere,
    /// Nothing answered: why.
    Unreached(String),
    /// macOS refused the look the LAN.
    Refused,
}

/// What stands before a join besides the vault: a person's choice that keeps it out (System
/// State's plan leaving the Secrets vault out, or a system System State does not run on), and
/// Local Network as last heard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gate {
    /// Why no vault is joined here, where a choice says so.
    pub off: Option<String>,
    /// Local Network as last heard.
    pub lan: Lan,
}

/// What `sync.peers` says once macOS refused aterm the local network.
pub const LAN_REFUSED: &str = "macOS does not let aterm use the local network (Don't Allow, or \
     turned off since), so no pass looks for a vault or asks again: System Settings > Privacy & \
     Security > Local Network turns aterm on";

/// What it says while macOS has not asked aterm about the local network.
pub const LAN_UNTOLD: &str = "macOS has not asked aterm about the local network yet: System \
     State's Local Network row asks, and no pass raises the question";

/// What it says to a pass that does not look: only the window's does.
pub const NOT_LOOKED: &str = "no vault is configured: the aterm window's pass looks for the \
     ALab vault and joins it, where Local Network is aterm.app's";

/// `sync.peers`' read over `ops`, the vault as System State's Secrets stage reads it. A vault
/// configured here reads joined once it let this Mac in, and deferred while it has not (it asks
/// a join password or an admin's approval, which a pass never gives) or once an admin removed
/// this Mac from it: a pass never joins it again.
/// With none, the person's turn-off or `gate`'s choice reads off, and Local Network refused or
/// untold reads deferred without a look; only then is `look` asked (`None`: this pass does not
/// look), and the ALab vault answering reads joinable.
///
/// # Errors
/// The vault's local state cannot be read.
pub fn read(
    ops: &dyn SecretsOps,
    gate: &Gate,
    look: Option<&dyn Fn() -> Heard>,
) -> Result<PeersRead, String> {
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
            Membership::Pending => PeersRead::Deferred(format!(
                "vault {name} has not let this Mac in: it asks a join password or an admin's \
                 approval, which System State's Secrets row or `aterm init` gives; no pass \
                 joins it again"
            )),
            Membership::Revoked { seq } => PeersRead::Deferred(format!(
                "an admin removed this Mac from vault {name} at seq {seq}: to rejoin, run aterm \
                 secrets join and have an admin approve it; no pass joins it again"
            )),
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
    match gate.lan {
        Lan::Refused => return Ok(PeersRead::Deferred(LAN_REFUSED.to_owned())),
        Lan::Untold => return Ok(PeersRead::Deferred(LAN_UNTOLD.to_owned())),
        Lan::Allowed => {}
    }
    let Some(look) = look else {
        return Ok(PeersRead::Deferred(NOT_LOOKED.to_owned()));
    };
    Ok(match look() {
        Heard::Found => PeersRead::Joinable("the ALab vault answers on this network".to_owned()),
        Heard::Elsewhere => PeersRead::Off("no ALab vault on this network".to_owned()),
        Heard::Unreached(why) => PeersRead::Off(format!(
            "nothing answered at the ALab vault's address ({why}); the next pass looks again"
        )),
        Heard::Refused => PeersRead::Deferred(LAN_REFUSED.to_owned()),
    })
}

/// `sync.peers`' apply: System State's join of the ALab vault over `ops`, with no password,
/// replacing only the "no vault" the read saw, journaled in `journal`, then a wait of
/// [`JOIN_WAIT`] for the vault to let this Mac in. In: joined. A vault that asks a join password
/// or an admin's approval leaves this Mac waiting: its request stands, and no pass sends another.
///
/// # Errors
/// The join failed (the vault did not answer it, its subscriber did not start, say).
pub fn join(
    sys: &dyn System,
    ctx: &Ctx,
    ops: Arc<dyn SecretsOps>,
    journal: &mut dyn Journal,
) -> Result<PeersDone, String> {
    let rows = crate::init::run_initial_secrets_with(
        sys,
        ctx,
        Arc::clone(&ops),
        aterm_types::macos_setup::DEFAULT_VAULT_URL,
        Replaces::Read(None),
        &mut None,
        JOIN_WAIT,
        journal,
    )?;
    let status = ops.status().map_err(|why| why.to_string())?;
    let name = status.config.as_ref().map_or_else(
        || "the ALab vault".to_owned(),
        |config| format!("vault {}", config.name),
    );
    let inside = match status.membership {
        Membership::In { .. } | Membership::Plain => true,
        // A join sent with no password waits for nobody: the open vault lets this Mac in
        // within the wait, and one that asks more never does.
        Membership::Pending => ops.await_approval(JOIN_WAIT, &|| false),
        Membership::Unconfigured | Membership::Revoked { .. } => {
            return Err(rows
                .iter()
                .find_map(|row| row.outcome.is_failure().then(|| row.why.clone()).flatten())
                .unwrap_or_else(|| format!("{name} was not joined")));
        }
    };
    Ok(if inside {
        PeersDone::Joined(format!("joined {name}, nothing typed"))
    } else {
        PeersDone::Waiting(format!(
            "{name} has not let this Mac in: it asks a join password or an admin's approval, \
             which System State's Secrets row or `aterm init` gives; its request stands, and no \
             pass sends another"
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::init_secrets::tests::{Scripted, configured, unconfigured};
    use crate::items::defaults::tests::{ctx, user};
    use aterm_secrets::client::JoinOutcome;
    use std::cell::Cell;

    const ALLOWED: Gate = Gate {
        off: None,
        lan: Lan::Allowed,
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
        )
    }

    /// A read over `ops` that counts its looks, each hearing `heard`.
    fn looked(ops: &Arc<Scripted>, gate: &Gate, heard: &Heard, looks: &Cell<u32>) -> PeersRead {
        let look = || {
            looks.set(looks.get() + 1);
            heard.clone()
        };
        read(ops.as_ref(), gate, Some(&look)).expect("a read")
    }

    fn pending() -> JoinOutcome {
        JoinOutcome::Pending {
            code: "abcd-ef23-ghij-k234".into(),
            host: "m22".into(),
        }
    }

    /// An unjoined Mac whose look hears the open ALab vault reads joinable, and its join sends
    /// no password and nothing a person types, at the ALab vault's address, and is let in
    /// within the wait.
    #[test]
    fn an_open_vault_joins_with_nothing_typed() {
        let ops = Scripted::new(vec![Ok(unconfigured())]);
        let looks = Cell::new(0);
        let read = looked(&ops, &ALLOWED, &Heard::Found, &looks);
        assert!(matches!(read, PeersRead::Joinable(_)), "{read:?}");
        assert_eq!(looks.get(), 1);

        ops.joins_as(Some(configured(Membership::Pending)), Ok(pending()));
        ops.approves(vec![true]);
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
        assert_eq!(ops.windows(), [JOIN_WAIT]);
    }

    /// A vault that asks a join password (or an admin's approval) does not let this Mac in: the
    /// join leaves it waiting, and every later read is deferred, so no pass joins it again.
    /// NEGATIVE CONTROL: a read that takes a pending membership for joinable joins it again.
    #[test]
    fn a_vault_that_asks_a_password_is_deferred_and_never_joined_again() {
        let ops = Scripted::new(vec![Ok(unconfigured())]);
        ops.joins_as(Some(configured(Membership::Pending)), Ok(pending()));
        let done = joined(&ops);
        assert!(matches!(done, Ok(PeersDone::Waiting(_))), "{done:?}");
        assert_eq!(ops.passwords(), [false], "no password sent");

        let looks = Cell::new(0);
        for _ in 0..2 {
            let read = looked(&ops, &ALLOWED, &Heard::Found, &looks);
            assert!(
                matches!(&read, PeersRead::Deferred(why) if why.contains("join password")),
                "{read:?}"
            );
        }
        assert_eq!(looks.get(), 0, "a vault configured here is not looked for");
        assert_eq!(ops.joins().len(), 1, "never joined again");
    }

    /// A Mac an admin removed from its vault reads deferred, naming the removal, not joined: no
    /// pass joins it again, and none reports it in.
    /// NEGATIVE CONTROL: a read that takes every membership but a pending one for joined reads
    /// the removed Mac as in its vault.
    #[test]
    fn a_mac_an_admin_removed_reads_deferred_not_joined() {
        let ops = Scripted::new(vec![Ok(configured(Membership::Revoked { seq: 9 }))]);
        let looks = Cell::new(0);
        let read = looked(&ops, &ALLOWED, &Heard::Found, &looks);
        let PeersRead::Deferred(why) = &read else {
            panic!("{read:?}");
        };
        assert!(
            why.contains("removed this Mac from vault alab at seq 9"),
            "{why}"
        );
        assert_eq!(looks.get(), 0, "a vault configured here is not looked for");
        assert!(ops.joins().is_empty(), "no pass joins it");
        let ops = Scripted::new(vec![Ok(configured(Membership::In { seq: 9, heard: 1 }))]);
        assert_eq!(
            looked(&ops, &ALLOWED, &Heard::Found, &looks),
            PeersRead::Joined("in vault alab".to_owned())
        );
    }

    /// Once macOS refused aterm the local network no pass looks or asks: a refusal on record
    /// reads deferred without a look, as does a look macOS refused; an untold Local Network
    /// is not looked at either (a look could raise the question), nor is anything looked for
    /// by a pass other than the window's.
    #[test]
    fn a_refused_local_network_is_never_looked_at_again() {
        let ops = Scripted::new(vec![Ok(unconfigured())]);
        let looks = Cell::new(0);
        let refused = Gate {
            off: None,
            lan: Lan::Refused,
        };
        for _ in 0..2 {
            assert_eq!(
                looked(&ops, &refused, &Heard::Found, &looks),
                PeersRead::Deferred(LAN_REFUSED.to_owned())
            );
        }
        let untold = Gate {
            off: None,
            lan: Lan::Untold,
        };
        assert_eq!(
            looked(&ops, &untold, &Heard::Found, &looks),
            PeersRead::Deferred(LAN_UNTOLD.to_owned())
        );
        assert_eq!(looks.get(), 0, "no look");

        assert_eq!(
            looked(&ops, &ALLOWED, &Heard::Refused, &looks),
            PeersRead::Deferred(LAN_REFUSED.to_owned())
        );
        assert_eq!(looks.get(), 1);
        assert_eq!(
            read(ops.as_ref(), &ALLOWED, None),
            Ok(PeersRead::Deferred(NOT_LOOKED.to_owned()))
        );
        assert!(ops.joins().is_empty());
    }

    /// A person's choice keeps the vault out before any look: the subscriber turned off, or
    /// System State's plan without the Secrets vault; a network with no ALab vault reads off.
    #[test]
    fn a_choice_or_a_network_without_the_vault_reads_off() {
        let ops = Scripted::new(vec![Ok(unconfigured())]);
        let looks = Cell::new(0);
        let chosen = Gate {
            off: Some("System State's plan leaves the Secrets vault out".to_owned()),
            lan: Lan::Allowed,
        };
        assert!(matches!(
            looked(&ops, &chosen, &Heard::Found, &looks),
            PeersRead::Off(_)
        ));
        assert_eq!(looks.get(), 0);
        for heard in [Heard::Elsewhere, Heard::Unreached("timed out".to_owned())] {
            let read = looked(&ops, &ALLOWED, &heard, &looks);
            assert!(matches!(read, PeersRead::Off(_)), "{heard:?}: {read:?}");
        }
        ops.turn_off();
        assert!(matches!(
            looked(&ops, &ALLOWED, &Heard::Found, &looks),
            PeersRead::Off(_)
        ));
        assert_eq!(looks.get(), 2);
    }
}
