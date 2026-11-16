// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The node service's reader of its peers' quota rows (FL-117,
//! `docs/DESIGN-disk-custody-2026-11-12.md` §6): a wall another Mac saw lifted is lifted here, with
//! no read of the provider.
//!
//! Every [`READ_EVERY`] it reads every seat's newest quota rows on this Mac's `metrics.log`,
//! judged by their origin as the mesh view judges a `now` row ([`aterm_mesh::view::quota_rows`]):
//! a removed Mac's row, or one a relaying Mac forged, is not read. Where a peer's row carries a
//! lift (`aterm_metrics::row::Lift`, the newest of each window) and its CLI and account key name a
//! record of this Mac's accounts store ([`accounts::Account::pk`]) whose wall of that window
//! stands, was last seen standing before the lift's proof and names the same reset,
//! [`accounts::note_peer_lift`] lifts it in one transaction: one wall row that names the peer and
//! wakes the sessions waiting at the wall, which resume at their next wait step. The lift's mark
//! takes the clock inside that transaction, not before the read of the log or the wait for the
//! store's lock, so a session that met the wall meanwhile is older than it and resumes too. A
//! peer's row never raises a wall, never lifts one seen after its proof or of another reset, and
//! never moves `active` or a pin.
//!
//! **Cheap when nothing waits.** Each read first loads each pool's state, and reads the log only
//! where a keyed account's wall stands; each lift is first tried on that loaded copy, and the store
//! is locked only for one that lifts there.
//!
//! **Once.** A lifted wall is marked lifted, so the same lift finds nothing standing; the reader
//! also keeps each lift the store took in this process, so a later wall this Mac observes, of the
//! same reset, is not lifted again by it whatever the two Macs' clocks say. After a restart the
//! rule alone holds: a lift lifts only a wall this Mac last saw standing before its proof.
//!
//! This is its own module: the publisher (`crate::metrics_publish`) reads no other Mac. Nothing
//! here signs in or asks a provider. It reads whatever `[alab] journal` says: that switch stops
//! what this Mac publishes, not what it takes from the rows its log already holds.

use aterm_agent::harness::accounts::{self, PeerLift};
use aterm_metrics::row::{Lift, QuotaRow};
use aterm_metrics::subject::{AccountKey, Cli, Node};
use aterm_types::MutexExt as _;
use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How often the node service reads its peers' quota rows.
const READ_EVERY: Duration = Duration::from_secs(30);

/// How long one read of the log may take.
const READ_WITHIN: Duration = Duration::from_secs(10);

/// Where a read's rows come from: this Mac's `metrics.log`, or a test's.
pub(crate) trait Peers {
    /// Every seat's newest quota row this Mac's log holds, judged by its origin, by Mac, CLI and
    /// account; this Mac's own among them.
    ///
    /// # Errors
    /// Where the log cannot be read.
    fn rows(&self) -> io::Result<BTreeMap<(Node, Cli, AccountKey), QuotaRow>>;
}

/// The node service's mesh as [`Peers`].
struct MeshPeers(Arc<aterm_mesh::Mesh>);

impl Peers for MeshPeers {
    fn rows(&self) -> io::Result<BTreeMap<(Node, Cli, AccountKey), QuotaRow>> {
        aterm_mesh::view::quota_rows(&self.0, READ_WITHIN)
    }
}

/// What one read did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Read {
    /// Whether the log was read: only where a keyed account's wall stands here.
    pub(crate) looked: bool,
    /// The walls lifted.
    pub(crate) lifted: usize,
    /// The log's rows could not be read, or a store refused its transaction.
    pub(crate) failed: Option<io::ErrorKind>,
}

/// The reader: where this Mac's accounts stores are, and the lifts they took.
pub(crate) struct Reader {
    /// The harness state, where the accounts stores are.
    harness: Option<PathBuf>,
    /// The person's home, under which each CLI's store keeps its logins.
    home: Option<PathBuf>,
    /// Each lift a store took in this process: by the Mac that lifted it, the row's CLI and
    /// account, and the lift.
    taken: Mutex<BTreeSet<(Node, Cli, AccountKey, Lift)>>,
}

impl Reader {
    pub(crate) fn new(harness: Option<PathBuf>, home: Option<PathBuf>) -> Self {
        Self {
            harness,
            home,
            taken: Mutex::new(BTreeSet::new()),
        }
    }

    /// The accounts store of each pool whose rows are per account, as a survey opens it, by the
    /// metrics CLI its quota rows name.
    fn stores(&self) -> Vec<(Cli, accounts::Store)> {
        let Some(harness) = &self.harness else {
            return Vec::new();
        };
        accounts::Cli::ALL
            .into_iter()
            .filter_map(|pool| {
                let cli = crate::metrics_publish::metrics_cli(pool)?;
                let store = match (pool, &self.home) {
                    (_, Some(home)) => accounts::Store::at_for(harness, pool, home),
                    (accounts::Cli::Claude, None) => accounts::Store::at(harness),
                    (_, None) => return None,
                };
                Some((cli, store))
            })
            .collect()
    }

    /// One read of `peers`' rows, as the Mac `me`, by `clock` (Unix seconds): lift each standing
    /// wall here that a peer's lift names ([`accounts::note_peer_lift`]). Each lift's mark reads
    /// `clock` inside its transaction.
    pub(crate) fn read(&self, peers: &dyn Peers, me: Node, clock: &dyn Fn() -> i64) -> Read {
        let now = clock();
        let mut read = Read::default();
        let mut walled: BTreeMap<Cli, (accounts::Store, accounts::State)> = BTreeMap::new();
        for (cli, store) in self.stores() {
            let Ok((state, _)) = store.load(now) else {
                continue;
            };
            let stands = state
                .accounts
                .values()
                .any(|account| account.pk.is_some() && account.limited_until(now).is_some());
            if stands {
                walled.insert(cli, (store, state));
            }
        }
        if walled.is_empty() {
            return read;
        }
        read.looked = true;
        let rows = match peers.rows() {
            Ok(rows) => rows,
            Err(error) => {
                read.failed = Some(error.kind());
                return read;
            }
        };
        let mut taken = self.taken.lock_or_recover();
        for ((node, cli, account), row) in rows {
            if node == me {
                continue;
            }
            let Some((store, loaded)) = walled.get_mut(&cli) else {
                continue;
            };
            let (peer, pk) = (node.to_string(), account.to_string());
            for lift in row.lifts {
                let key = (node, cli, account, lift);
                if taken.contains(&key) {
                    continue;
                }
                let lift = &key.3;
                let peer_lift = PeerLift {
                    peer: &peer,
                    pk: &pk,
                    window: lift.window.as_str(),
                    at: i64::try_from(lift.at).unwrap_or(i64::MAX),
                    reset: lift
                        .reset
                        .map(|reset| i64::try_from(reset).unwrap_or(i64::MAX)),
                };
                // Tried on the loaded copy first: the store is locked only for a lift that lifts.
                if accounts::note_peer_lift(loaded, &peer_lift, now).is_empty() {
                    continue;
                }
                let begun = clock().max(now);
                match store.transact_within(&accounts::LOCK_LADDER, begun, &mut None, |state| {
                    Ok(accounts::note_peer_lift(
                        state,
                        &peer_lift,
                        clock().max(begun),
                    ))
                }) {
                    Ok(events) => {
                        if !events.is_empty() {
                            read.lifted += events.len();
                            taken.insert(key);
                        }
                    }
                    Err(why) => {
                        aterm_log::debug!("node: a peer's lift was not recorded: {why}");
                        read.failed = Some(io::ErrorKind::Other);
                    }
                }
            }
        }
        read
    }
}

/// Start reading from `aterm node serve`: a read every [`READ_EVERY`] on a thread of its own,
/// while the node service runs a mesh.
pub(crate) fn start() {
    let reader = Reader::new(
        aterm_agent::harness::cli::harness_state_dir().ok(),
        aterm_types::dirs::home_dir(),
    );
    let spawned = std::thread::Builder::new()
        .name("aterm-quota-lifts".to_owned())
        .spawn(move || {
            loop {
                std::thread::sleep(READ_EVERY);
                let Some(mesh) = aterm_mesh::current() else {
                    continue;
                };
                let Some(me) = Node::parse(mesh.principal()) else {
                    continue;
                };
                let clock = || {
                    aterm_time::unix_secs()
                        .and_then(|now| i64::try_from(now).ok())
                        .unwrap_or(0)
                };
                let read = reader.read(&MeshPeers(mesh), me, &clock);
                if read.lifted > 0 {
                    aterm_log::info!(
                        "node: lifted {} wall(s) another Mac saw come back",
                        read.lifted
                    );
                }
            }
        });
    if let Err(error) = spawned {
        aterm_log::warn!("node: no reader of the peers' quota rows: {error}");
    }
}

#[cfg(test)]
#[path = "quota_lifts_tests.rs"]
mod tests;
