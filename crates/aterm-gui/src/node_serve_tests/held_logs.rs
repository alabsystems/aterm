// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The mesh logs the node job holds across its generations
//! (`docs/DESIGN-alab-one-mesh-2026-11-05.md` §11 B7a).

use aterm_net::fleet::link::ManualClock;

use aterm_net::fleet_member::read_node_identity;

use super::super::MeshHeld;
use super::*;

/// A node job's configuration over `state` with the mesh on and nothing else to serve.
fn mesh_on(state: &Path) -> NodeServeConfig {
    NodeServeConfig {
        state_dir: state.to_path_buf(),
        relay_dir: state.join("relay"),
        home_dir: None,
        config_path: None,
        vault_paths: None,
        members: None,
        alab_browse: None,
        prebound_listener: None,
        on_generation: None,
        tick_every: Duration::from_millis(40),
        wake_timeout: Duration::from_secs(1),
        lan_unavailable: || None,
        stop: Arc::new(NodeStop::default()),
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

/// THE MESH LOGS FOLLOW THE NODE KEY: they open under this Mac's node key and stay open across
/// generations while it stays; a new key (the Mac rejoined) closes them and opens them under the
/// new one, since their records sign and their grants name the key they opened with; and with no
/// key (the Mac left) they close and stay closed.
#[test]
fn the_mesh_logs_follow_the_node_key() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let state = root.path().join("state");
    let first_id = seed_id(&state);
    let cfg = mesh_on(&state);
    let mut held = MeshHeld::default();
    let first = held
        .get(&cfg, &read_node_identity(&state))
        .expect("the mesh opens under the node key");
    assert_eq!(first.key(), *first_id.key().bytes());
    let again = held
        .get(&cfg, &read_node_identity(&state))
        .expect("still open");
    assert!(Arc::ptr_eq(&first, &again), "kept across generations");
    drop(again);

    std::fs::remove_file(state.join(aterm_net::fleet_member::NODE_KEY_FILE))
        .expect("the old key goes");
    let second_id = node_identity(&state).expect("a new node key");
    assert_ne!(second_id.key().bytes(), first_id.key().bytes());
    // The generation that held the old mesh has ended, as it has whenever the job asks.
    let old = Arc::downgrade(&first);
    drop(first);
    let second = held
        .get(&cfg, &read_node_identity(&state))
        .expect("the mesh reopens under the new key");
    assert!(
        old.upgrade().is_none(),
        "the old key's mesh closed and went"
    );
    assert_eq!(second.key(), *second_id.key().bytes());

    std::fs::remove_file(state.join(aterm_net::fleet_member::NODE_KEY_FILE)).expect("the Mac left");
    assert!(
        held.get(&cfg, &read_node_identity(&state)).is_none(),
        "no key, no mesh"
    );
    assert!(second.is_stopped(), "the last key's mesh closed");
    assert!(
        held.get(&cfg, &read_node_identity(&state)).is_none(),
        "and stays closed"
    );
}
