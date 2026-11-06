// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tables over the relay (`docs/DESIGN-alab-one-mesh-2026-11-05.md` §11 B7b): the egress, the
//! ingest consumer and the `Inventory` exchanges of one mesh, started with it where the node
//! service hands it its stores ([`crate::MeshConfig::stores`]) and stopped with it.
//!
//! Threads: the egress (`aterm-mesh-egress`) and its wake socket's reader (`aterm-mesh-wake`), the
//! ingest consumer's tail (`aterm-mesh-ingest`) and its worker (`aterm-mesh-ingest-work`), and the
//! `Inventory` worker (`aterm-mesh-inventory`). Each holds the mesh weakly and parks on a bell; none
//! reads a store on a timer.

use crate::egress::{self, Egress, WakeSocket};
use crate::ingest::Ingest;
use crate::inventory::Inventory;
use crate::logs::{Mesh, MeshLog};
use crate::relay;
use crate::stores::{Digest, Stores, TableKey};
use aterm_types::MutexExt;
use std::io;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Instant;

/// The running flow of one mesh.
pub(crate) struct Flow {
    pub(crate) stores: Arc<dyn Stores>,
    pub(crate) egress: Arc<Egress>,
    pub(crate) ingest: Arc<Ingest>,
    pub(crate) inventory: Arc<Inventory>,
    wake: Option<Arc<WakeSocket>>,
    threads: Vec<JoinHandle<()>>,
}

/// The mesh of this process that moves tables now: the node service's.
static OPEN: std::sync::Mutex<Option<std::sync::Weak<Mesh>>> = std::sync::Mutex::new(None);

/// The mesh of this process that moves tables now, if any: what the Commons runtime of the same
/// process asks whether a peer speaks `Bus` ([`Mesh::speaks_bus`]) and to push a table
/// ([`Mesh::push_table`]).
#[must_use]
pub fn current() -> Option<Arc<Mesh>> {
    OPEN.lock_or_recover()
        .as_ref()
        .and_then(std::sync::Weak::upgrade)
        .filter(|mesh| !mesh.is_stopped())
}

fn spawn(name: &str, body: impl FnOnce() + Send + 'static) -> io::Result<JoinHandle<()>> {
    std::thread::Builder::new()
        .name(name.to_owned())
        .spawn(body)
}

impl Flow {
    /// Start the flow of `mesh` over `stores`: its threads, its wake socket (where its path fits
    /// a socket's), and this process's store sinks, which feed this egress until it stops.
    pub(crate) fn start(mesh: &Arc<Mesh>, stores: Arc<dyn Stores>) -> io::Result<Self> {
        let dir = mesh.log_dir(MeshLog::Tables);
        let egress = Egress::new(Arc::clone(&stores), &dir);
        let inventory = Inventory::new(Arc::clone(&stores), Arc::clone(&egress));
        let ingest = Ingest::new(
            Arc::clone(&stores),
            Arc::clone(&egress),
            Arc::clone(&inventory),
            &dir,
        );
        egress.begin()?;
        let mut flow = Self {
            stores,
            egress,
            ingest,
            inventory,
            wake: None,
            threads: Vec::new(),
        };
        let started = flow.spawn_all(mesh);
        if let Err(e) = started {
            flow.stop();
            return Err(e);
        }
        flow.stores.hook();
        egress::sinks::serve(Some(&flow.egress));
        *OPEN.lock_or_recover() = Some(Arc::downgrade(mesh));
        Ok(flow)
    }

    fn spawn_all(&mut self, mesh: &Arc<Mesh>) -> io::Result<()> {
        let weak = Arc::downgrade(mesh);
        let path = egress::wake_path(mesh.state_dir());
        if path.as_os_str().len() < crate::logs::SUN_PATH_MAX {
            match WakeSocket::bind(&path) {
                Ok(socket) => self.wake = Some(Arc::new(socket)),
                Err(e) => aterm_log::warn!(
                    "mesh: the egress's wake socket {} did not bind ({e}): another process's \
                     writes are published at the next start",
                    path.display()
                ),
            }
        }
        if let Some(socket) = &self.wake {
            let (socket, egress, w) = (Arc::clone(socket), Arc::clone(&self.egress), weak.clone());
            self.threads
                .push(spawn("aterm-mesh-wake", move || socket.run(&egress, &w))?);
        }
        let (egress, w) = (Arc::clone(&self.egress), weak.clone());
        self.threads
            .push(spawn("aterm-mesh-egress", move || egress.run(&w))?);
        let (ingest, w) = (Arc::clone(&self.ingest), weak.clone());
        self.threads
            .push(spawn("aterm-mesh-ingest", move || ingest.read(&w))?);
        let (ingest, w) = (Arc::clone(&self.ingest), weak.clone());
        self.threads
            .push(spawn("aterm-mesh-ingest-work", move || ingest.work(&w))?);
        let (inventory, w) = (Arc::clone(&self.inventory), weak);
        self.threads
            .push(spawn("aterm-mesh-inventory", move || inventory.run(&w))?);
        Ok(())
    }

    /// Stop every thread and join it; this process's sinks feed no egress after. The ingest
    /// consumer and `Inventory` stop first, so no record arrives after the egress's last drain,
    /// which publishes what its queue holds while the logs are still open.
    pub(crate) fn stop(&mut self) {
        {
            let mut open = OPEN.lock_or_recover();
            if open
                .as_ref()
                .and_then(std::sync::Weak::upgrade)
                .is_none_or(|mesh| mesh.flow.lock_or_recover().is_none())
            {
                *open = None;
            }
        }
        self.ingest.stop();
        self.inventory.stop();
        let current = std::thread::current().id();
        let (egress_side, others): (Vec<_>, Vec<_>) = self.threads.drain(..).partition(|thread| {
            matches!(
                thread.thread().name(),
                Some("aterm-mesh-egress" | "aterm-mesh-wake")
            )
        });
        for thread in others {
            if thread.thread().id() != current {
                let _ = thread.join();
            }
        }
        if egress::sinks::open().is_some_and(|open| Arc::ptr_eq(&open, &self.egress)) {
            egress::sinks::serve(None);
        }
        self.egress.stop();
        if let Some(socket) = &self.wake {
            socket.nudge();
        }
        for thread in egress_side {
            if thread.thread().id() != current {
                let _ = thread.join();
            }
        }
        self.wake = None;
    }
}

/// Whether a push reached a neighbour ([`Mesh::push_table`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pushed {
    /// A neighbour acked the last record this Mac published of the table.
    Reached,
    /// The deadline passed first.
    TimedOut,
}

impl Mesh {
    /// The egress, where this mesh moves tables.
    #[must_use]
    pub fn egress(&self) -> Option<Arc<Egress>> {
        self.flow
            .lock_or_recover()
            .as_ref()
            .map(|flow| Arc::clone(&flow.egress))
    }

    /// The ingest consumer, where this mesh moves tables.
    #[must_use]
    pub fn ingest(&self) -> Option<Arc<Ingest>> {
        self.flow
            .lock_or_recover()
            .as_ref()
            .map(|flow| Arc::clone(&flow.ingest))
    }

    /// What this Mac's lab tables hold, as its egress has read them ([`Egress::tables_held`]):
    /// `None` where this mesh moves no tables or its egress has not read every store yet.
    #[must_use]
    pub fn tables_held(&self) -> Option<crate::presence::Held> {
        self.egress()?.tables_held()
    }

    pub(crate) fn inventory(&self) -> Option<Arc<Inventory>> {
        self.flow
            .lock_or_recover()
            .as_ref()
            .map(|flow| Arc::clone(&flow.inventory))
    }

    /// Whether the Mac whose node key is `key` speaks `Bus` with this one: a tables relay to it
    /// follows now, while this mesh moves tables. Commons repair holds its rounds and redials
    /// with such a peer (§11 B7b); with any other, or once that relay ends, it goes on.
    #[must_use]
    pub fn speaks_bus(&self, key: &[u8; 32]) -> bool {
        self.flow.lock_or_recover().is_some()
            && !self.is_stopped()
            && self
                .heard
                .lock_or_recover()
                .values()
                .any(|(heard, _)| heard == key)
    }

    /// Wait until a neighbour acked the last record this Mac published of `table` (its primary
    /// store), once the egress published everything that store holds now, or `deadline`. `None`
    /// where this mesh moves no tables, no tables relay to a neighbour follows now, or this Mac
    /// published none of the table since the mesh began (its records predate the mesh, or all
    /// came by it): the caller's old path pushes then.
    #[must_use]
    pub fn push_table(&self, table: Digest, deadline: Instant) -> Option<Pushed> {
        let (stores, egress) = {
            let flow = self.flow.lock_or_recover();
            let flow = flow.as_ref()?;
            (Arc::clone(&flow.stores), Arc::clone(&flow.egress))
        };
        let peers: Vec<String> = self.heard.lock_or_recover().keys().cloned().collect();
        if peers.is_empty() {
            return None;
        }
        let key = TableKey {
            table,
            recovery: false,
        };
        let length = stores
            .table_lengths()
            .ok()?
            .into_iter()
            .find_map(|(k, length)| (k == key).then_some(length))
            .unwrap_or(0);
        if !egress.wait_published(key, length, deadline) {
            return Some(Pushed::TimedOut);
        }
        let offset = egress.last_published(&table)?;
        let broker = &self.tables.broker;
        let mut acks = self.acks.0.lock_or_recover();
        loop {
            // The neighbour's group resumes one past the last record it acked.
            if peers
                .iter()
                .any(|peer| broker.group_start(&relay::group_of(peer)) > offset)
            {
                return Some(Pushed::Reached);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() || self.is_stopped() {
                return Some(Pushed::TimedOut);
            }
            acks = self
                .acks
                .1
                .wait_timeout(acks, left)
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
    }

    /// Cut `tables.log` below `floor` whatever holds it back, as the size rule does: what a
    /// neighbour's cursor still needed is a gap its relay reads next.
    /// # Errors
    /// What the broker answers.
    #[cfg(test)]
    pub(crate) fn cut_tables_for_test(&self, floor: u64) -> io::Result<u64> {
        self.tables.broker.retain_before_carrying_state(floor)
    }

    /// A tables relay's neighbour acked a burst, or this Mac's tables log moved (a neighbour's
    /// relay commits its group there): what a push waits on may have moved.
    pub(crate) fn acked(&self) {
        let mut acks = self.acks.0.lock_or_recover();
        *acks = acks.wrapping_add(1);
        drop(acks);
        self.acks.1.notify_all();
    }
}
