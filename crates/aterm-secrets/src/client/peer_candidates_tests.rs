// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Coverage of the final bounded peer list across policy and discovered endpoints.

use super::*;
use crate::discover::{DiscoveredVault, MAX_CANDIDATES};
use crate::swarm_roster::SwarmPolicy;
use std::collections::BTreeSet;

#[test]
fn direct_peer_selection_survives_process_restarts() {
    const CHILD_HOME: &str = "ATERM_PEER_SELECTION_TEST_HOME";
    const PREFIX: &str = "selected-peers=";
    const PRIMARY: &str = "http://primary.local:7484/v1/team";
    if let Some(home) = std::env::var_os(CHILD_HOME) {
        let paths = Paths::for_home(Path::new(&home));
        let config = configured(&paths).unwrap();
        let found = PeerDiscovery::direct(None);
        let mut peers = [
            PeerUse::Records,
            PeerUse::Envelope,
            PeerUse::Reader,
            PeerUse::Queue,
        ]
        .map(|purpose| collect_sync_peer_urls(&paths, &config, PRIMARY, &[], Some(&found), purpose))
        .to_vec();
        peers.push(join_routing::targets(&paths, &config, PRIMARY, &found));
        println!("{PREFIX}{}", aterm_json::to_string(&peers).unwrap());
        return;
    }

    let root = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(&root.path().join("reader"));
    let source = root.path().join("team.vault");
    create(&paths, source.to_str().unwrap(), None, Some("m3")).unwrap();
    let policy = SwarmPolicy {
        servers: (0..35)
            .map(|n| format!("http://policy-{n}.local:7484/v1/team"))
            .collect(),
        ..SwarmPolicy::default()
    };
    let admin = admin_open(&paths).unwrap();
    let mut doc = admin.doc.clone();
    doc.set_entry(
        &Audience::Everyone,
        crate::SWARM_POLICY_ENTRY,
        policy.to_toml().as_bytes(),
    )
    .unwrap();
    admin_publish(&paths, &admin, &doc).unwrap();
    let mut reached: [BTreeSet<String>; 5] = std::array::from_fn(|_| BTreeSet::new());
    for _ in 0..policy.servers.len().div_ceil(MAX_CANDIDATES) {
        // Each invocation has fresh process statics, as the account sign-in
        // ladder's separate `aterm secrets sync` command does. A libtest that runs
        // one test at a time writes `test <name> ... ` to stdout before the test
        // runs, with no newline, so the child's line starts after it. The child
        // runs that way whatever `RUST_TEST_THREADS` this run inherited.
        let mut child = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
        child
            .args([
                "--exact",
                "client::peer_candidates_tests::direct_peer_selection_survives_process_restarts",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(CHILD_HOME, &paths.home);
        let output = aterm_uds::deadline::output_until_complete_stdout(
            &mut child,
            Instant::now() + aterm_spec::wait::MUST_HAPPEN,
        )
        .unwrap();
        assert!(output.status.success(), "{output:?}");
        let text = String::from_utf8(output.stdout).unwrap();
        let selected = text
            .lines()
            .find_map(|line| line.split_once(PREFIX).map(|(_, json)| json))
            .unwrap_or_else(|| panic!("the child selected no peers:\n{text}"));
        let batches: [Vec<String>; 5] = aterm_json::from_str(selected).unwrap();
        for (seen, batch) in reached.iter_mut().zip(batches) {
            assert!(batch.len() <= MAX_CANDIDATES);
            seen.extend(batch);
        }
    }
    let expected = policy.servers.into_iter().collect();
    for seen in reached {
        assert_eq!(seen, expected, "a restarted reader must reach every peer");
    }
}

#[test]
fn policy_and_discovered_peers_share_bounded_passes_without_starvation() {
    for (policy_count, discovered_count) in [(2, 15), (16, 1), (16, 16), (35, 0)] {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let paths = Paths::for_home(&root.path().join("reader"));
        let source = root.path().join("team.vault");
        create(&paths, source.to_str().expect("path"), None, Some("m3"))
            .expect("create local vault");
        let policy = SwarmPolicy {
            servers: (0..policy_count)
                .map(|n| format!("http://policy-{n}.local:7484/v1/team"))
                .collect(),
            ..SwarmPolicy::default()
        };
        let admin = admin_open(&paths).expect("admin");
        let mut doc = admin.doc.clone();
        doc.set_entry(
            &Audience::Everyone,
            crate::SWARM_POLICY_ENTRY,
            policy.to_toml().as_bytes(),
        )
        .expect("policy entry");
        admin_publish(&paths, &admin, &doc).expect("publish local policy");
        let config = configured(&paths).expect("config");
        assert_eq!(policy_peer_urls(&paths, &config), policy.servers);
        let discovered: Vec<_> = (0..discovered_count)
            .map(|n| DiscoveredVault {
                url: format!("http://discovered-{n}.local:7484/v1/team"),
                txt: None,
            })
            .collect();
        let expected: BTreeSet<_> = policy
            .servers
            .iter()
            .cloned()
            .chain(discovered.iter().map(|peer| peer.url.clone()))
            .collect();
        let found = |_: &Config| discovered.clone();
        let found = PeerDiscovery::supplied(&found);
        let mut selected = BTreeSet::new();
        let mut envelopes = BTreeSet::new();
        for _ in 0..expected.len().div_ceil(MAX_CANDIDATES) {
            let peers = collect_sync_peer_urls(
                &paths,
                &config,
                "http://primary.local:7484/v1/team",
                &[],
                Some(&found),
                PeerUse::Records,
            );
            assert!(peers.len() <= MAX_CANDIDATES);
            let envelope_peers = collect_sync_peer_urls(
                &paths,
                &config,
                "http://primary.local:7484/v1/team",
                &[],
                Some(&found),
                PeerUse::Envelope,
            );
            assert_eq!(
                peers, envelope_peers,
                "independent consumers progress equally"
            );
            envelopes.extend(envelope_peers);
            let reader = PeerDiscovery::selected(&peers);
            let reused = collect_sync_peer_urls(
                &paths,
                &config,
                "http://primary.local:7484/v1/team",
                &[],
                Some(&reader),
                PeerUse::Records,
            );
            assert_eq!(
                reused, peers,
                "a selected batch is not combined with policy again"
            );
            selected.extend(peers);
        }
        assert_eq!(
            selected, expected,
            "policy={policy_count}, discovered={discovered_count}"
        );
        assert_eq!(envelopes, expected);
    }
}
