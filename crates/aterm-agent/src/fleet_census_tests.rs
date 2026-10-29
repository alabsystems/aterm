// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::collections::BTreeSet;
use std::time::Duration;

use super::*;
use crate::harness::accounts::{Cli, Vendor};
use crate::harness::ai_pool::{Pool, PoolAccount};
use crate::harness::claims;
use crate::survey_facts::{ServerHeardFact, SurveyAccountFact, SwarmSurveyFacts};
use aterm_secrets::swarm_roster::{SwarmMember, SwarmRoster};

fn sample_member(
    label: &str,
    aliases: &[&str],
    recipient: &str,
    node_id: Option<&str>,
) -> SwarmMember {
    SwarmMember {
        recipient: recipient.to_owned(),
        vault_name: label.to_owned(),
        label: label.to_owned(),
        aliases: aliases.iter().map(|s| (*s).to_owned()).collect(),
        hint: None,
        admitted_ms: 1_793_000_000_000,
        claimed_label: None,
        claimed_at_ms: None,
        node_id: node_id.map(str::to_owned),
        node_key: None,
    }
}

fn sample_facts(
    node: &str,
    label: &str,
    version: &str,
    window: bool,
    window_last: Option<u64>,
    sessions: usize,
    accounts: Vec<SurveyAccountFact>,
) -> SwarmSurveyFacts {
    SwarmSurveyFacts {
        node: Some(node.to_owned()),
        label: Some(label.to_owned()),
        version: version.to_owned(),
        window,
        window_last,
        sessions,
        ssh_host_key: None,
        accounts,
        accounts_unread: Vec::new(),
        session_accounts: BTreeMap::new(),
        server_heard: Vec::new(),
    }
}

#[test]
fn census_joins_five_sources_identically_across_stewards_and_classifies_stragglers() {
    let now_s = 1_793_300_000_u64;
    let now_i64 = i64::try_from(now_s).unwrap();
    let n22 = "22222222222222222222222222222222";
    let n100 = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    let roster = SwarmRoster {
        members: vec![
            sample_member("m100", &["m32"], "age1m100", Some(n100)),
            sample_member("m22", &[], "age1m22", Some(n22)),
        ],
        deny_labels: vec!["m9".to_owned()],
        deny_nodes: Vec::new(),
    };

    let pool = Pool {
        source: "test".to_owned(),
        last_heard: BTreeMap::new(),
        accounts: vec![PoolAccount {
            provider: Cli::Claude,
            vendor: Vendor::default(),
            label: Some("c01".to_owned()),
            email: "c01@example.com".to_owned(),
            browser: None,
            warm: vec!["m32".to_owned(), "m22".to_owned()],
            since: Vec::new(),
            why: Vec::new(),
            used: Vec::new(),
        }],
    };

    // m100 holds a lease claim on c01; another Mac's junior claim on it counts for nobody.
    let lease = |machine: &str, label: &str, since: i64| claims::Row {
        machine: machine.to_owned(),
        label: label.to_owned(),
        counter: 1,
        until: now_i64 + claims::HORIZON_S,
        claims: vec![claims::Claim {
            key: claims::LeaseKey::new("claude", "c01@example.com").unwrap(),
            kind: claims::Kind::Lease,
            since,
        }],
    };
    let leases = claims::Book::of([
        lease("aa100", "m100", now_i64 - 60),
        lease("aa22", "m22", now_i64 - 30),
    ]);

    let mut commons_labels = BTreeSet::new();
    commons_labels.insert("m22".to_owned());
    commons_labels.insert("m100".to_owned());

    let inputs_a = CensusInputs {
        now_s,
        steward: Some("m1".to_owned()),
        roster: roster.clone(),
        server_heard: vec![
            ServerHeardFact {
                vault: "alab".to_owned(),
                subject: "m22".to_owned(),
                at: now_s - 60,
            },
            ServerHeardFact {
                vault: "alab".to_owned(),
                subject: "m32".to_owned(),
                at: now_s - 30,
            },
        ],
        fleet_book: vec![
            FleetBookPeerInput {
                node: Some(n100.to_owned()),
                host: "m100.local".to_owned(),
                seen_at: Some(now_s - 45),
            },
            FleetBookPeerInput {
                node: Some(n22.to_owned()),
                host: "m22.local".to_owned(),
                seen_at: Some(now_s - 90),
            },
        ],
        heartbeats: vec![
            HeartbeatInput {
                node: n22.to_owned(),
                facts: sample_facts(
                    n22,
                    "m22",
                    "0.146.0",
                    false,
                    Some(now_s - 3_600),
                    4,
                    Vec::new(),
                ),
                received_at: Some(now_s - 120),
                converge_failures: 0,
            },
            HeartbeatInput {
                node: n100.to_owned(),
                facts: sample_facts(
                    n100,
                    "m32",
                    "0.146.0",
                    true,
                    Some(now_s - 10),
                    2,
                    vec![SurveyAccountFact {
                        pool: "claude".to_owned(),
                        id: "c01".to_owned(),
                        condition: "rate_limited".to_owned(),
                        enrolled: true,
                        usage_5h_pct: Some(100),
                        usage_7d_pct: Some(82),
                        week_reset: Some(now_i64 + 86_400),
                        walled_until: Some(now_i64 + 1_800),
                        last_used: Some(now_i64 - 20),
                    }],
                ),
                received_at: Some(now_s - 20),
                converge_failures: 0,
            },
        ],
        commons_labels: commons_labels.clone(),
        commons_member_count: None,
        pool: Some(pool.clone()),
        claims: leases.clone(),
        claims_view: claims::View::default(),
        bonjour_hosts: vec![
            BonjourHostInput {
                name: "M32".to_owned(),
                dot_local: "m32.local".to_owned(),
                is_linux: false,
                seen_at: now_s - 12,
            },
            BonjourHostInput {
                name: "m100-Mac-Studio".to_owned(),
                dot_local: "m100.local".to_owned(),
                is_linux: false,
                seen_at: now_s - 5,
            },
            BonjourHostInput {
                name: "m9".to_owned(),
                dot_local: "m9.local".to_owned(),
                is_linux: false,
                seen_at: now_s - 15,
            },
            BonjourHostInput {
                name: "gpu-rig".to_owned(),
                dot_local: "gpu-rig.local".to_owned(),
                is_linux: true,
                seen_at: now_s - 25,
            },
            BonjourHostInput {
                name: "guest-macbook".to_owned(),
                dot_local: "guest-macbook.local".to_owned(),
                is_linux: false,
                seen_at: now_s - 35,
            },
        ],
        journal_shortfall_hours: 3,
    };

    // Permute every input order on Steward B (`same steward label` for P2 JSON equality check).
    let mut inputs_b = inputs_a.clone();
    inputs_b.roster.members.reverse();
    inputs_b.server_heard.reverse();
    inputs_b.fleet_book.reverse();
    inputs_b.heartbeats.reverse();
    inputs_b.bonjour_hosts.reverse();

    let census_a = SwarmCensus::compute(&inputs_a);
    let census_b = SwarmCensus::compute(&inputs_b);
    assert_eq!(
        census_a.to_json(),
        census_b.to_json(),
        "P2: census JSON must be byte-for-byte identical from any steward with permuted input order"
    );

    let roundtrip = SwarmCensus::from_json(&census_a.to_json()).expect("roundtrip JSON");
    assert_eq!(roundtrip, census_a);

    // Natural `m<N>` ordering: m22 comes before m100 (and alias m32 folded into m100!).
    assert_eq!(census_a.members.len(), 2);
    assert_eq!(census_a.members[0].label, "m22");
    assert_eq!(census_a.members[1].label, "m100");
    assert_eq!(census_a.members[1].accounts.warm, 1);
    assert_eq!(census_a.members[1].accounts.leased, 1);
    assert_eq!(census_a.members[1].accounts.walled, 1);
    assert_eq!(census_a.members[1].last_seen, Some(now_s - 5));

    // P5: zero unlabeled and zero conflicts on clean bound roster.
    assert_eq!(census_a.summary.unlabeled, 0);
    assert_eq!(census_a.summary.label_conflicts, 0);
    assert_eq!(census_a.summary.fleet_members, 2);
    assert_eq!(census_a.summary.commons_members, 2);

    // P3: 2 active-session Macs vs 0 unwalled accounts -> "short by 2 accounts" and 3h lost.
    assert_eq!(census_a.summary.shortfall_accounts, 2);
    assert_eq!(census_a.summary.shortfall_hours, 3);
    let human = census_a.render_human();
    assert!(
        human.contains("short by 2 accounts (3h lost to shortfall)"),
        "P3 human output must print 'short by N accounts' with hours lost: {human}"
    );

    // P2: every straggler classified into {revoked, Linux, no aterm}.
    assert_eq!(census_a.stragglers.len(), 3);
    assert_eq!(census_a.stragglers[0].name, "m9");
    assert_eq!(census_a.stragglers[0].reason, StragglerReason::Revoked);
    assert_eq!(census_a.stragglers[1].name, "gpu-rig");
    assert_eq!(census_a.stragglers[1].reason, StragglerReason::Linux);
    assert_eq!(census_a.stragglers[2].name, "guest-macbook");
    assert_eq!(census_a.stragglers[2].reason, StragglerReason::NoAterm);

    // Negative control: introduce a conflicting second node claiming `m22` + an unlabelled peer,
    // and verify P5 counters (`label_conflicts`, `unlabeled`, `converge_failed_24h`) fire!
    let mut mutated = inputs_a;
    mutated.fleet_book.push(FleetBookPeerInput {
        node: Some("ffffffffffffffffffffffffffffffff".to_owned()),
        host: "m22.local".to_owned(),
        seen_at: Some(now_s - 10),
    });
    mutated.fleet_book.push(FleetBookPeerInput {
        node: Some("eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee".to_owned()),
        host: "macbook-pro-m4.local".to_owned(),
        seen_at: Some(now_s - 10),
    });
    mutated.heartbeats[0].converge_failures = 2;
    let bad = SwarmCensus::compute(&mutated);
    assert_eq!(
        bad.summary.label_conflicts, 1,
        "negative control: second node claiming m22 must increment label_conflicts"
    );
    assert_eq!(
        bad.summary.unlabeled, 1,
        "negative control: mid-string 'macbook-pro-m4' must never become m4 and must increment unlabeled"
    );
    assert_eq!(bad.summary.converge_failed_24h, 1);
}

fn clock_inputs(now_s: u64) -> CensusInputs {
    CensusInputs {
        now_s,
        roster: SwarmRoster {
            members: vec![sample_member(
                "m22",
                &[],
                "age1m22",
                Some("22222222222222222222222222222222"),
            )],
            ..SwarmRoster::default()
        },
        ..CensusInputs::default()
    }
}

fn one_warm_pool() -> Pool {
    Pool {
        source: "test".to_owned(),
        last_heard: BTreeMap::new(),
        accounts: vec![PoolAccount {
            provider: Cli::Claude,
            vendor: Vendor::default(),
            label: Some("c01".to_owned()),
            email: "c01@example.com".to_owned(),
            browser: None,
            warm: vec!["m22".to_owned()],
            since: Vec::new(),
            why: Vec::new(),
            used: Vec::new(),
        }],
    }
}

#[test]
fn census_clock_future_last_seen_is_unheard_and_old_contacts_stay_away() {
    let now = 1_800_000_000;
    let mut member = SwarmCensus::compute(&clock_inputs(now)).members.remove(0);
    for at in [now + 1, u64::MAX] {
        member.last_seen = Some(at);
        assert_eq!(member.monitor_status(now), "admitted (unheard)");
    }
    for at in [now, now - LIVE_HEARD_S] {
        member.last_seen = Some(at);
        assert_eq!(member.monitor_status(now), "online (swarm)");
    }
    let old = now - LIVE_HEARD_S - 1;
    member.last_seen = Some(old);
    assert_eq!(member.monitor_status(now), format!("away since {old}"));
    member.window = true;
    assert_eq!(member.monitor_status(now), format!("asleep since {old}"));
}

#[test]
fn census_clock_future_sources_do_not_outrank_a_known_contact() {
    let now = 1_800_000_000;
    let node = "22222222222222222222222222222222";
    let mut inputs = clock_inputs(now);
    inputs.pool = Some(one_warm_pool());
    inputs.fleet_book = vec![
        FleetBookPeerInput {
            node: Some(node.to_owned()),
            host: "known-m22.local".to_owned(),
            seen_at: Some(now - 60),
        },
        FleetBookPeerInput {
            node: Some(node.to_owned()),
            host: "future-m22.local".to_owned(),
            seen_at: Some(now + 60),
        },
    ];
    inputs.heartbeats = vec![
        HeartbeatInput {
            node: node.to_owned(),
            facts: sample_facts(node, "m22", "known", false, Some(now + 3600), 2, Vec::new()),
            received_at: Some(now - 20),
            converge_failures: 0,
        },
        HeartbeatInput {
            node: node.to_owned(),
            facts: sample_facts(
                node,
                "m22",
                "future",
                true,
                Some(now + 3600),
                7,
                vec![SurveyAccountFact {
                    pool: "claude".to_owned(),
                    id: "c01".to_owned(),
                    condition: "rate_limited".to_owned(),
                    enrolled: true,
                    usage_5h_pct: Some(100),
                    usage_7d_pct: Some(82),
                    week_reset: None,
                    walled_until: None,
                    last_used: None,
                }],
            ),
            received_at: Some(now + 30),
            converge_failures: 1,
        },
    ];
    inputs.server_heard.push(ServerHeardFact {
        vault: "alab".to_owned(),
        subject: "m22".to_owned(),
        at: now + 120,
    });
    inputs.bonjour_hosts.push(BonjourHostInput {
        name: "m22".to_owned(),
        dot_local: "future-bonjour.local".to_owned(),
        is_linux: false,
        seen_at: now + 120,
    });
    let census = SwarmCensus::compute(&inputs);
    let member = &census.members[0];
    assert_eq!(member.last_seen, Some(now - 20));
    assert_eq!(member.host.as_deref(), Some("known-m22.local"));
    assert_eq!(member.version.as_deref(), Some("known"));
    assert_eq!(member.sessions, 2);
    assert_eq!(member.window_last, None);
    assert!(!member.window);
    assert!(!member.eligible);
    assert_eq!(member.accounts.warm, 1);
    assert_eq!(member.accounts.walled, 0);
    assert_eq!(member.monitor_status(now), "online (swarm)");
    assert_eq!(census.summary.converge_failed_24h, 0);
    assert_eq!(census.summary.shortfall_accounts, 0);
    inputs.fleet_book.reverse();
    inputs.heartbeats.reverse();
    inputs.server_heard.reverse();
    inputs.bonjour_hosts.reverse();
    assert_eq!(SwarmCensus::compute(&inputs).to_json(), census.to_json());
}

#[test]
fn census_clock_future_only_evidence_retains_identity_without_live_claims() {
    let now = 1_800_000_000;
    let node = "22222222222222222222222222222222";
    let mut inputs = clock_inputs(now);
    inputs.heartbeats.push(HeartbeatInput {
        node: node.to_owned(),
        facts: sample_facts(node, "m22", "future", true, Some(now + 60), 7, Vec::new()),
        received_at: Some(now + 60),
        converge_failures: 1,
    });
    inputs.server_heard.push(ServerHeardFact {
        vault: "alab".to_owned(),
        subject: "m22".to_owned(),
        at: now + 60,
    });
    for name in ["m22", "unheard-mac"] {
        inputs.bonjour_hosts.push(BonjourHostInput {
            name: name.to_owned(),
            dot_local: format!("{name}.local"),
            is_linux: false,
            seen_at: now + 60,
        });
    }
    let census = SwarmCensus::compute(&inputs);
    assert_eq!(census.members.len(), 1);
    let member = &census.members[0];
    assert_eq!(member.label, "m22");
    assert_eq!(member.node.as_deref(), Some(node));
    assert!(member.in_fleet);
    assert_eq!(member.last_seen, None);
    assert_eq!(member.window_last, None);
    assert!(!member.window);
    assert_eq!(member.sessions, 0);
    assert!(!member.eligible);
    assert_eq!(member.monitor_status(now), "admitted (unheard)");
    assert_eq!(census.summary.distinct_heard_7d, 0);
    assert_eq!(census.summary.converge_failed_24h, 0);
    assert!(census.stragglers.is_empty());
}

#[cfg(unix)]
#[test]
fn census_clock_both_cache_readers_recompute_a_future_generation() {
    let now = 1_800_000_000;
    let dir = aterm_tempfile::tempdir().unwrap();
    for served in [false, true] {
        for (generated, fresh) in [
            (now + 3600, false),
            (now, true),
            (now - CENSUS_REFRESH_S + 1, true),
            (now - CENSUS_REFRESH_S, false),
        ] {
            let cached = SwarmCensus::compute(&clock_inputs(generated));
            write_cached_census(dir.path(), &cached).unwrap();
            let read = if served {
                let wire = serve_fleet_census(dir.path(), None, now).unwrap();
                SwarmCensus::from_json(&wire).unwrap()
            } else {
                read_or_compute_census(dir.path(), None, false, now).unwrap()
            };
            assert_eq!(read.generated_at, if fresh { generated } else { now });
            assert_eq!(read.members.is_empty(), !fresh);
        }
    }
}

fn healthy_pool_inputs(now: u64) -> CensusInputs {
    let node = "22222222222222222222222222222222";
    let mut inputs = clock_inputs(now);
    inputs.pool = Some(one_warm_pool());
    inputs.heartbeats.push(HeartbeatInput {
        node: node.to_owned(),
        facts: sample_facts(node, "m22", "known", true, Some(now - 5), 1, Vec::new()),
        received_at: Some(now - 5),
        converge_failures: 0,
    });
    inputs
}

fn wall_report(node: &str, label: &str, received_at: u64) -> HeartbeatInput {
    HeartbeatInput {
        node: node.to_owned(),
        facts: sample_facts(
            node,
            label,
            "wall",
            false,
            None,
            0,
            vec![SurveyAccountFact {
                pool: "claude".to_owned(),
                id: "c01".to_owned(),
                condition: "rate_limited".to_owned(),
                enrolled: true,
                usage_5h_pct: Some(100),
                usage_7d_pct: None,
                week_reset: None,
                walled_until: None,
                last_used: None,
            }],
        ),
        received_at: Some(received_at),
        converge_failures: 0,
    }
}

fn assert_warm_account_available(inputs: &CensusInputs) {
    let census = SwarmCensus::compute(inputs);
    assert_eq!(census.members.len(), 1);
    let member = &census.members[0];
    assert_eq!(member.label, "m22");
    assert_eq!(member.sessions, 1);
    assert_eq!(member.accounts.warm, 1);
    assert_eq!(member.accounts.walled, 0);
    assert_eq!(census.summary.shortfall_accounts, 0);
}

#[test]
fn census_walls_from_denied_nodes_do_not_remove_available_pool_supply() {
    let now = 1_800_000_000;
    let denied = "33333333333333333333333333333333";
    let mut inputs = healthy_pool_inputs(now);
    inputs.roster.deny_nodes.push(denied.to_owned());
    inputs.heartbeats.push(wall_report(denied, "m33", now - 1));
    assert_warm_account_available(&inputs);
}

#[test]
fn census_walls_from_denied_labels_do_not_remove_available_pool_supply() {
    let now = 1_800_000_000;
    let mut inputs = healthy_pool_inputs(now);
    inputs.roster.deny_labels.push("m33".to_owned());
    inputs.heartbeats.push(wall_report(
        "33333333333333333333333333333333",
        "m33",
        now - 1,
    ));
    assert_warm_account_available(&inputs);
}

#[test]
fn census_walls_use_the_selected_report_and_keep_current_walls() {
    let now = 1_800_000_000;
    let node = "22222222222222222222222222222222";
    let mut inputs = healthy_pool_inputs(now);
    inputs.heartbeats.push(wall_report(node, "m22", now - 30));
    assert_warm_account_available(&inputs);
    inputs.heartbeats.reverse();
    assert_warm_account_available(&inputs);

    let mut current = wall_report(node, "m22", now - 1);
    current.facts.sessions = 1;
    inputs.heartbeats.push(current);
    let census = SwarmCensus::compute(&inputs);
    assert_eq!(census.members[0].accounts.walled, 1);
    assert_eq!(census.summary.shortfall_accounts, 1);
}

/// A lease whose Mac the census host has not heard for longer than `claims::PRESENCE_TTL_S` is
/// no claim on the claims path (any other Mac may take its account), so it is no Mac's leased
/// account in the census either; the same row is that Mac's while the host hears it.
#[test]
fn a_lease_whose_mac_the_census_host_has_not_heard_counts_for_nobody() {
    let now_s = 1_800_000_000u64;
    let now = i64::try_from(now_s).unwrap();
    let n100 = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let inputs = CensusInputs {
        now_s,
        steward: Some("m1".to_owned()),
        roster: SwarmRoster {
            members: vec![sample_member("m100", &[], "age1m100", Some(n100))],
            deny_labels: Vec::new(),
            deny_nodes: Vec::new(),
        },
        claims: claims::Book::of([claims::Row {
            machine: "aa100".to_owned(),
            label: "m100".to_owned(),
            counter: 1,
            until: now + claims::HORIZON_S,
            claims: vec![claims::Claim {
                key: claims::LeaseKey::new("claude", "c01@example.com").unwrap(),
                kind: claims::Kind::Lease,
                since: now - 3600,
            }],
        }]),
        ..CensusInputs::default()
    };
    let heard_at = |last: i64| claims::View {
        me: "aa1".to_owned(),
        presence: Some(claims::Presence {
            at: now,
            heard: [("aa100".to_owned(), last)].into_iter().collect(),
        }),
    };
    let leased = |view: claims::View| {
        let census = SwarmCensus::compute(&CensusInputs {
            claims_view: view,
            ..inputs.clone()
        });
        census.members[0].accounts.leased
    };
    assert_eq!(leased(heard_at(now - 60)), 1, "heard a minute ago");
    assert_eq!(
        leased(heard_at(now - claims::PRESENCE_TTL_S - 60)),
        0,
        "unheard past the presence life"
    );
}

/// The census host reads the claims table with what its Commons hears, as its claims path does:
/// `collect_local_census_inputs` keeps the view `claims::read_book` gives with the rows.
#[test]
fn the_census_host_reads_its_claims_with_what_its_commons_hears() {
    let now = 1_800_000_000_i64;
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let state = dir.path().join("state");
    let home = dir.path().join("home");
    aterm_uds::private_file::ensure_private_dir(&state).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    let presence = claims::Presence {
        at: now,
        heard: [("bb02".to_owned(), now - 60)].into_iter().collect(),
    };
    let table = std::sync::Arc::new(claims::tests::Replica::new("aa01"));
    let inputs = claims::with_table(table, || {
        claims::with_presence(presence.clone(), || {
            collect_local_census_inputs(&state, Some(&home), Vec::new(), now.unsigned_abs())
        })
    });
    assert_eq!(
        inputs.claims_view,
        claims::View {
            me: "aa01".to_owned(),
            presence: Some(presence),
        }
    );
}

#[cfg(unix)]
#[test]
fn every_mac_computes_its_census_from_its_own_book_and_dials_no_member() {
    use aterm_net::fleet::{FleetOp, FleetScope, FleetSecret, Listening};
    use aterm_net::fleet_member::{
        Began, Peer, SECRET_LEN, Via, node_identity, serve_census, write_held,
    };
    use std::net::{IpAddr, Ipv4Addr, TcpListener, TcpStream};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    let now_s = 1_793_310_000_u64;
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let steward_home = dir.path().join("steward-home");
    let steward_state = dir.path().join("steward-state");
    let reader_home = dir.path().join("reader-home");
    let reader_state = dir.path().join("reader-state");
    for d in [&steward_home, &reader_home] {
        std::fs::create_dir_all(d.join(".secrets")).unwrap();
        std::fs::create_dir_all(d.join(".config/aterm")).unwrap();
    }
    aterm_uds::private_file::ensure_private_dir(&steward_state).unwrap();
    aterm_uds::private_file::ensure_private_dir(&reader_state).unwrap();
    std::fs::write(
        steward_home.join(".config/aterm/aterm.toml"),
        "[swarm]\nlabel = \"m1\"\n",
    )
    .unwrap();
    std::fs::write(
        reader_home.join(".config/aterm/aterm.toml"),
        "[swarm]\nlabel = \"m15\"\n",
    )
    .unwrap();
    // The reader minted no fleet (no join code on disk): the census is its to compute all the same
    // (C24, F17), where main's census went to the Mac that holds the code.
    let steward_node = node_identity(&steward_state).unwrap();
    let secret_bytes = [0x3c; SECRET_LEN];
    let secret = FleetSecret::new(&secret_bytes).unwrap();
    let mark = secret.mark();

    // Another member listens and counts each census asked of it.
    let tcp = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let addr = tcp.local_addr().unwrap();
    let running = Arc::new(AtomicBool::new(true));
    let server_dials = Arc::new(AtomicUsize::new(0));
    let listening = Listening {
        secret: secret.clone(),
        scope: FleetScope::Facts,
        node: Some(steward_node.clone()),
        serves: &[FleetOp::Survey, FleetOp::Census],
        slots: 16,
        per_source: 16,
        lifetime: Duration::from_secs(60),
    };
    let run_flag = Arc::clone(&running);
    let dials_flag = Arc::clone(&server_dials);
    let st_state = steward_state.clone();
    let st_home = steward_home.clone();
    let srv = std::thread::spawn(move || {
        aterm_net::fleet::serve(
            &tcp,
            &listening,
            &run_flag,
            move |served, _| {
                dials_flag.fetch_add(1, Ordering::SeqCst);
                let s = st_state.clone();
                let h = st_home.clone();
                served.answer(|stream| {
                    let _ = serve_census(stream, || serve_fleet_census(&s, Some(&h), now_s))?;
                    Ok(())
                })
            },
            |_| {},
        );
    });

    write_held(&reader_state, Began::Here, |book| {
        book.write_key(&secret_bytes).unwrap();
        book.update(|peers| {
            peers.push(Peer {
                node: Some(steward_node.node().to_owned()),
                key: Some(steward_node.key()),
                key_from: None,
                host: "m1.local".to_owned(),
                addr: Some(IpAddr::V4(Ipv4Addr::LOCALHOST)),
                port: addr.port(),
                version: Some("0.146.0".to_owned()),
                seen_ms: Some(1_000),
                via: Via::Joined,
                fleet: Some(mark),
                signed: None,
            });
            for other in 2..=5u16 {
                peers.push(Peer {
                    node: Some(format!("{other:032x}")),
                    key: None,
                    key_from: None,
                    host: format!("m{other}.local"),
                    addr: Some(IpAddr::V4(Ipv4Addr::LOCALHOST)),
                    port: 9,
                    version: Some("0.146.0".to_owned()),
                    seen_ms: Some(1_000),
                    via: Via::Joined,
                    fleet: Some(mark),
                    signed: None,
                });
            }
            Ok(())
        })
    })
    .unwrap()
    .unwrap();

    let own = SwarmCensus::compute(&collect_local_census_inputs(
        &reader_state,
        Some(&reader_home),
        Vec::new(),
        now_s,
    ));
    let first = read_or_compute_census(&reader_state, Some(&reader_home), false, now_s)
        .expect("the census this Mac computes");
    assert_eq!(first, own, "the census of this Mac's own book");
    let cached = read_or_compute_census(&reader_state, Some(&reader_home), false, now_s + 30)
        .expect("the cached census");
    assert_eq!(
        cached, first,
        "read again from the cache within its refresh"
    );
    assert_eq!(
        server_dials.load(Ordering::SeqCst),
        0,
        "no member is asked for it"
    );
    // A Mac asked for its census with none cached computes one on the spot, code or none.
    let fresh_dir = dir.path().join("fresh-state");
    aterm_uds::private_file::ensure_private_dir(&fresh_dir).unwrap();
    assert!(
        serve_fleet_census(&fresh_dir, Some(&reader_home), now_s).is_ok(),
        "every Mac serves the census of its own book"
    );

    // A Mac's own census inputs include its own node and host once it holds `fleet.key`, before
    // any member mirrors its book back.
    write_held(&steward_state, Began::Here, |book| {
        book.write_key(&secret_bytes).unwrap();
        Ok::<(), String>(())
    })
    .unwrap()
    .unwrap();
    let local_inputs =
        collect_local_census_inputs(&steward_state, Some(&steward_home), Vec::new(), now_s);
    assert!(
        local_inputs
            .fleet_book
            .iter()
            .any(|p| p.node.as_deref() == Some(steward_node.node()) && p.host == "m1.local"),
        "a Mac with fleet.key includes its own node and host in fleet_book: {:?}",
        local_inputs.fleet_book
    );

    running.store(false, Ordering::Release);
    let _ = TcpStream::connect(addr);
    let _ = srv.join();
}

#[cfg(unix)]
#[test]
fn collect_local_census_inputs_populates_commons_labels_and_count() {
    let root = aterm_tempfile::tempdir().unwrap();
    let srv_dir = root.path().join("server");
    let socket = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let running = aterm_secrets::server::start(
        socket,
        &aterm_secrets::server::Options {
            listen: "127.0.0.1:0".parse().unwrap(),
            store: srv_dir,
            advertise: None,
            pins: Vec::new(),
            hold: std::time::Duration::from_millis(50),
            keep_awake: false,
        },
    )
    .unwrap();
    let url = format!("http://{}/v1/test", running.addr);
    let home = root.path().join("m22-home");
    let state = root.path().join("m22-state");
    aterm_uds::private_file::ensure_private_dir(&state).unwrap();

    let paths = aterm_secrets::client::Paths::for_home(&home);
    aterm_secrets::client::create(&paths, &url, None, Some("m22")).unwrap();
    aterm_secrets::client::admin_seed_swarm_roster(&paths, &[]).unwrap();
    let bootstrap = aterm_secrets::commons::bootstrap::Bootstrap::required(&paths).unwrap();
    assert!(bootstrap.access().unwrap().is_some());

    let inputs = collect_local_census_inputs(&state, Some(&home), Vec::new(), 1_762_000_000);
    assert!(
        inputs.commons_labels.contains("m22"),
        "expected m22 in commons_labels: {:?}",
        inputs.commons_labels
    );
    assert_eq!(inputs.commons_member_count, Some(1));
    let census = SwarmCensus::compute(&inputs);
    assert_eq!(census.summary.commons_members, 1);
    assert!(
        census
            .members
            .iter()
            .any(|m| m.label == "m22" && m.in_commons)
    );

    running.stop();
}

#[cfg(unix)]
#[test]
fn census_excludes_denied_and_log_held_out_members_from_book_facts_and_walled_accounts() {
    use aterm_net::fleet::FleetSecret;
    use aterm_net::fleet_member::{
        Began, Peer, SECRET_LEN, Via, node_identity, read_pushed_facts, store_pushed_facts,
        write_held,
    };
    use std::net::{IpAddr, Ipv4Addr};

    let now_s = 1_793_320_000_u64;
    let now_i64 = i64::try_from(now_s).unwrap();
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let state = dir.path().join("state");
    let good_dir = dir.path().join("good-node");
    let denied_dir = dir.path().join("denied-node");
    for d in [&state, &good_dir, &denied_dir] {
        aterm_uds::private_file::ensure_private_dir(d).unwrap();
    }

    let good_id = node_identity(&good_dir).unwrap();
    let denied_id = node_identity(&denied_dir).unwrap();
    let secret_bytes = [0x4d; SECRET_LEN];
    let secret = FleetSecret::new(&secret_bytes).unwrap();
    let mark = secret.mark();

    write_held(&state, Began::Here, |book| {
        book.write_key(&secret_bytes).unwrap();
        book.update(|peers| {
            peers.push(Peer {
                node: Some(good_id.node().to_owned()),
                key: Some(good_id.key()),
                key_from: None,
                host: "m1.local".to_owned(),
                addr: Some(IpAddr::V4(Ipv4Addr::LOCALHOST)),
                port: 7101,
                version: Some("0.147.0".to_owned()),
                seen_ms: Some(now_s * 1_000),
                via: Via::Joined,
                fleet: Some(mark),
                signed: None,
            });
            peers.push(Peer {
                node: Some(denied_id.node().to_owned()),
                key: Some(denied_id.key()),
                key_from: None,
                host: "m9.local".to_owned(),
                addr: Some(IpAddr::V4(Ipv4Addr::LOCALHOST)),
                port: 7109,
                version: Some("0.147.0".to_owned()),
                seen_ms: Some(now_s * 1_000),
                via: Via::Joined,
                fleet: Some(mark),
                signed: None,
            });
            Ok(())
        })
    })
    .unwrap()
    .unwrap();

    let denied_facts = sample_facts(
        denied_id.node(),
        "m9",
        "0.147.0",
        true,
        Some(now_s - 5),
        2,
        vec![SurveyAccountFact {
            pool: "claude".to_owned(),
            id: "c01".to_owned(),
            condition: "rate_limited".to_owned(),
            enrolled: true,
            usage_5h_pct: Some(100),
            usage_7d_pct: Some(100),
            week_reset: Some(now_i64 + 86_400),
            walled_until: Some(now_i64 + 3_600),
            last_used: Some(now_i64 - 5),
        }],
    );
    let payload = denied_facts.to_survey_reply(1, Some("m9.local"), now_s * 1_000, 1, false);
    store_pushed_facts(&state, denied_id.node(), &payload).unwrap();
    let facts_path = state
        .join(aterm_net::fleet_member::SWARM_FACTS_DIR)
        .join(format!("{}.txt", denied_id.node()));
    std::fs::File::options()
        .write(true)
        .open(&facts_path)
        .unwrap()
        .set_times(
            std::fs::FileTimes::new()
                .set_modified(std::time::UNIX_EPOCH + Duration::from_secs(now_s - 1)),
        )
        .unwrap();
    assert_eq!(read_pushed_facts(&state).len(), 1);

    // This Mac is m1, by its `aterm.toml`: without one the census names this Mac from the host's
    // name, which on a fleet Mac is an `m<N>` label of its own and a member more than this counts.
    let home = dir.path().join("home");
    std::fs::create_dir_all(home.join(".config/aterm")).unwrap();
    std::fs::write(
        home.join(".config/aterm/aterm.toml"),
        "[swarm]\nlabel = \"m1\"\n",
    )
    .unwrap();
    let mut inputs = collect_local_census_inputs(&state, Some(&home), Vec::new(), now_s);
    assert_eq!(inputs.heartbeats[0].received_at, Some(now_s - 1));
    inputs.roster = SwarmRoster {
        members: vec![sample_member("m1", &[], "age1m1", Some(good_id.node()))],
        deny_labels: vec!["m9".to_owned()],
        deny_nodes: Vec::new(),
    };
    inputs.pool = Some(Pool {
        source: "test".to_owned(),
        last_heard: BTreeMap::new(),
        accounts: vec![PoolAccount {
            provider: Cli::Claude,
            vendor: Vendor::default(),
            label: Some("c01".to_owned()),
            email: "c01@example.com".to_owned(),
            browser: None,
            warm: vec!["m1".to_owned()],
            since: Vec::new(),
            why: Vec::new(),
            used: Vec::new(),
        }],
    });
    let census = SwarmCensus::compute(&inputs);
    assert_eq!(census.members.len(), 1);
    assert_eq!(census.members[0].label, "m1");
    assert_eq!(
        census.members[0].accounts.walled, 0,
        "a denied label's heartbeat must not mark pool accounts walled"
    );

    aterm_net::fleet_member::prune_pushed_facts(&state, &[], &[denied_id.node().to_owned()], None);
    assert!(
        read_pushed_facts(&state).is_empty(),
        "prune_pushed_facts must remove denied node facts from swarm-facts/"
    );
}

#[cfg(unix)]
#[test]
fn denied_node_key_excludes_member_peer_and_pushed_facts_in_census() {
    use aterm_net::fleet::{FleetSecret, NodeProof};
    use aterm_net::fleet_member::{
        Began, Peer, SECRET_LEN, Via, node_identity, read_pushed_facts, store_pushed_facts,
        write_held,
    };
    use std::net::{IpAddr, Ipv4Addr};

    let now_s = 1_760_000_000_u64;
    let now_i64 = i64::try_from(now_s).unwrap();
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let state = dir.path().join("state");
    let good_dir = dir.path().join("good-node");
    let denied_dir = dir.path().join("denied-node");
    for d in [&state, &good_dir, &denied_dir] {
        aterm_uds::private_file::ensure_private_dir(d).unwrap();
    }

    let good_id = node_identity(&good_dir).unwrap();
    let denied_id = node_identity(&denied_dir).unwrap();
    assert!(
        aterm_net::fleet::Denied::nodes(&[denied_id.key().hex()]).refuses(Some(&NodeProof {
            node: denied_id.node().to_owned(),
            key: denied_id.key(),
        }))
    );

    let secret_bytes = [0x4e; SECRET_LEN];
    let secret = FleetSecret::new(&secret_bytes).unwrap();
    let mark = secret.mark();
    write_held(&state, Began::Here, |book| {
        book.write_key(&secret_bytes).unwrap();
        book.update(|peers| {
            peers.push(Peer {
                node: Some(good_id.node().to_owned()),
                key: Some(good_id.key()),
                key_from: None,
                host: "m1.local".to_owned(),
                addr: Some(IpAddr::V4(Ipv4Addr::LOCALHOST)),
                port: 7101,
                version: Some("0.147.0".to_owned()),
                seen_ms: Some(now_s * 1_000),
                via: Via::Joined,
                fleet: Some(mark),
                signed: None,
            });
            peers.push(Peer {
                node: Some(denied_id.node().to_owned()),
                key: Some(denied_id.key()),
                key_from: None,
                host: "m8.local".to_owned(),
                addr: Some(IpAddr::V4(Ipv4Addr::LOCALHOST)),
                port: 7108,
                version: Some("0.147.0".to_owned()),
                seen_ms: Some(now_s * 1_000),
                via: Via::Joined,
                fleet: Some(mark),
                signed: None,
            });
            Ok(())
        })
    })
    .unwrap()
    .unwrap();

    let denied_facts = sample_facts(
        denied_id.node(),
        "m8",
        "0.147.0",
        true,
        Some(now_s - 5),
        1,
        vec![SurveyAccountFact {
            pool: "claude".to_owned(),
            id: "c01".to_owned(),
            condition: "rate_limited".to_owned(),
            enrolled: true,
            usage_5h_pct: Some(100),
            usage_7d_pct: Some(100),
            week_reset: Some(now_i64 + 86_400),
            walled_until: Some(now_i64 + 3_600),
            last_used: Some(now_i64 - 5),
        }],
    );
    let payload = denied_facts.to_survey_reply(1, Some("m8.local"), now_s * 1_000, 1, false);
    store_pushed_facts(&state, denied_id.node(), &payload).unwrap();
    assert_eq!(read_pushed_facts(&state).len(), 1);

    // Prune pushed facts using only the 64-hex `node_key` in `denied_nodes`.
    let peers = aterm_net::fleet_member::read_peers(&state).unwrap();
    aterm_net::fleet_member::prune_pushed_facts(&state, &peers, &[denied_id.key().hex()], None);
    assert!(
        read_pushed_facts(&state).is_empty(),
        "prune_pushed_facts must remove peer facts when peer's 64-hex node_key is in denied_nodes"
    );

    let mut bad_member = sample_member("m8", &[], "age1m8", None);
    bad_member.node_key = Some(denied_id.key().hex());
    // This Mac is m1, by its `aterm.toml`: without one the census names this Mac from the host's
    // name, which on a fleet Mac is an `m<N>` label of its own and a member more than this counts.
    let home = dir.path().join("home");
    std::fs::create_dir_all(home.join(".config/aterm")).unwrap();
    std::fs::write(
        home.join(".config/aterm/aterm.toml"),
        "[swarm]\nlabel = \"m1\"\n",
    )
    .unwrap();
    let mut inputs = collect_local_census_inputs(&state, Some(&home), Vec::new(), now_s);
    inputs.steward = Some("m1".to_owned());
    inputs.roster = SwarmRoster {
        members: vec![
            sample_member("m1", &[], "age1m1", Some(good_id.node())),
            bad_member,
        ],
        deny_labels: Vec::new(),
        deny_nodes: vec![denied_id.key().hex()],
    };
    let census = SwarmCensus::compute(&inputs);
    assert_eq!(
        census
            .members
            .iter()
            .map(|m| m.label.as_str())
            .collect::<Vec<_>>(),
        vec!["m1"],
        "member whose node_key is in deny_nodes must be excluded from census"
    );
}
