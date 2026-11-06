// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Presence on the mesh the node job holds (`docs/DESIGN-alab-one-mesh-2026-11-05.md` §11 B7c).

use std::collections::BTreeSet;
use std::sync::atomic::AtomicU16;

use aterm_mesh::presence::{Build, Held};
use aterm_net::fleet::link::ManualClock;
use aterm_net::fleet_member::read_node_identity;

use super::super::{MeshHeld, presence_config};
use super::*;

/// A node job's configuration over `state` with the mesh on and nothing else to serve.
fn mesh_on(state: &Path, members: Option<Arc<LocalLog>>) -> NodeServeConfig {
    NodeServeConfig {
        state_dir: state.to_path_buf(),
        relay_dir: state.join("relay"),
        home_dir: None,
        config_path: None,
        vault_paths: None,
        members,
        alab_browse: None,
        prebound_listener: None,
        tick_every: Duration::from_millis(40),
        wake_timeout: Duration::from_secs(1),
        lan_unavailable: || None,
        stop: Arc::new(AtomicBool::new(false)),
        launcher: idle_launcher(),
        updates: None,
        mesh: NodeMesh {
            on: true,
            clock: ManualClock::new() as Arc<dyn aterm_net::fleet::link::LinkClock>,
            watch: Arc::new(|_| {}),
            units: Arc::new(|| None),
            on_neighbours: None,
            bus: None,
            ..NodeMesh::live()
        },
    }
}

/// Presence runs while the node job holds the mesh: it starts as the mesh opens, reading this
/// Mac's own seat, and stops with it (the held logs' test shows a closed mesh leaves nothing
/// holding it, presence included).
#[test]
fn presence_runs_while_the_mesh_is_held() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let state = root.path().join("state");
    let id = seed_id(&state);
    let cfg = mesh_on(&state, None);
    let mut held = MeshHeld::default();
    held.listening(48_123);
    let mesh = held
        .get(&cfg, &read_node_identity(&state))
        .expect("the mesh opens under the node key");
    assert_eq!(mesh.key(), *id.key().bytes());
    let presence = held
        .presence
        .as_ref()
        .expect("presence starts with the mesh");
    assert!(
        presence.seat(mesh.principal()).is_some_and(|seat| seat.me),
        "it reads this Mac's own seat"
    );
    drop(held);
    assert!(mesh.is_stopped(), "the mesh closed after presence stopped");
}

/// What a row reads: this build, this Mac's fleet addresses at the port the latest generation
/// bound, and the membership log the job holds (with no vault, the record store it was given);
/// with neither, no membership digest. The lab tables and the held topics are the mesh's: with no
/// mesh, neither is read.
#[test]
fn a_row_reads_this_build_the_bound_port_and_the_records_held() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let state = root.path().join("state");
    let id = seed_id(&state);
    let other = seed_id(&root.path().join("other"));
    let port = Arc::new(AtomicU16::new(48_123));

    let alone = presence_config(&mesh_on(&state, None), &port, std::sync::Weak::new());
    assert_eq!(alone.dir, state.join(aterm_mesh::logs::MESH_DIR));
    let facts = (alone.facts)();
    assert_eq!(facts.build, Build::running());
    assert!(facts.addresses.iter().all(|at| at.port() == 48_123));
    assert_eq!(facts.membership, None, "no vault and no record store");
    assert_eq!(
        facts.tables, None,
        "no mesh, so no egress has read the tables"
    );
    assert_eq!(
        facts.topics,
        Vec::<String>::new(),
        "no bus.log, so no held topics"
    );
    port.store(48_124, Ordering::Release);
    assert!(
        (alone.facts)()
            .addresses
            .iter()
            .all(|at| at.port() == 48_124),
        "the next generation's port"
    );

    let file = root.path().join("records");
    let members = lab_log(&file, &[(1, &id), (2, &other)], 2);
    let lab = presence_config(
        &mesh_on(&state, Some(members)),
        &port,
        std::sync::Weak::new(),
    );
    let membership = (lab.facts)().membership.expect("the record store's digest");
    assert_eq!(membership.count, 2, "the genesis and the removal");
    assert_eq!(
        Some(membership),
        aterm_mesh::presence::membership_of_records(&file)
    );
    assert_ne!(Some(membership), Some(Held::membership(&BTreeSet::new())));
}
