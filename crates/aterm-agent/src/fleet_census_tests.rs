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
        hw_hint: None,
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
        fleet_names: BTreeMap::new(),
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
    let saturated = SwarmCensus::from_json(
        r#"{"generated_at":1,"members":[],"stragglers":[],"summary":{"p2_ratio_bps":5000000000}}"#,
    )
    .expect("large p2_ratio_bps saturates instead of wrapping to zero");
    assert_eq!(saturated.summary.p2_ratio_bps, u32::MAX);
    #[cfg(unix)]
    {
        let tmp = aterm_tempfile::tempdir().expect("tempdir");
        let state = tmp.path().join("state");
        write_cached_census(&state, &census_a).expect("write cached census");
        assert_eq!(read_cached_census(&state).as_ref(), Some(&census_a));
        let outside = tmp.path().join("outside.json");
        std::fs::write(&outside, census_a.to_json()).expect("write outside");
        std::fs::remove_file(state.join(CENSUS_CACHE_FILE)).expect("rm cache");
        std::os::unix::fs::symlink(&outside, state.join(CENSUS_CACHE_FILE)).expect("symlink cache");
        assert!(
            read_cached_census(&state).is_none(),
            "symlinked swarm-census.json must be refused"
        );
        let home = tmp.path().join("home");
        std::fs::create_dir_all(home.join(".secrets")).expect("secrets dir");
        let outside_pool = tmp.path().join("outside-pool.toml");
        std::fs::write(&outside_pool, pool.to_toml()).expect("write outside pool");
        std::os::unix::fs::symlink(&outside_pool, home.join(".secrets").join("ai-pool.toml"))
            .expect("symlink ai-pool.toml");
        let symlink_inputs = collect_local_census_inputs(&state, Some(&home), Vec::new(), now_s);
        assert!(
            symlink_inputs.pool.is_none(),
            "symlinked ~/.secrets/ai-pool.toml must be refused"
        );
    }

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

#[test]
fn primary_fleet_names_do_not_reassign_pool_identity_or_accounts() {
    let mut inputs = clock_inputs(1_800_000_000);
    let node = "22222222222222222222222222222222";
    let baseline = SwarmCensus::compute(&inputs);
    inputs.fleet_names.insert(node.into(), "m62".into());
    inputs
        .fleet_names
        .insert("another-node".into(), "unrelated".into());
    let named = SwarmCensus::compute(&inputs);
    assert_eq!(named.members[0].label, "m22");
    assert_eq!(named.members[0].fleet_name.as_deref(), Some("m62"));
    let mut without_display_name = named.clone();
    without_display_name.members[0].fleet_name = None;
    assert_eq!(without_display_name, baseline);
    let json = named.to_json();
    assert_eq!(SwarmCensus::from_json(&json).unwrap(), named);
    let value: aterm_json::Value = aterm_json::from_str(&json).unwrap();
    let alias = format!("node-{node}");
    assert_eq!(
        value["members"][0]["node_alias"].as_str(),
        Some(alias.as_str())
    );
    assert!(named.render_human().contains("m62"));

    inputs.fleet_names.insert(node.into(), "lab-mac".into());
    assert_eq!(
        SwarmCensus::compute(&inputs).members[0]
            .fleet_name
            .as_deref(),
        Some("lab-mac")
    );
    inputs
        .fleet_names
        .insert(node.into(), "invalid\nname".into());
    assert_eq!(SwarmCensus::compute(&inputs), baseline);
    inputs.fleet_names.remove(node);
    assert_eq!(SwarmCensus::compute(&inputs), baseline);
}

#[cfg(unix)]
#[test]
fn census_collects_only_authenticated_peer_names_and_current_local_name() {
    use aterm_net::fleet::{FleetSecret, NodeIdentity};
    use aterm_net::fleet_member::{self as member, Began, Peer};
    let dir = aterm_tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    aterm_uds::private_file::ensure_private_dir(&state).unwrap();
    let own = member::node_identity(&state).unwrap();
    let good = NodeIdentity::from_seed("11111111111111111111111111111111", &[11; 32]).unwrap();
    let bad = NodeIdentity::from_seed("33333333333333333333333333333333", &[33; 32]).unwrap();
    let secret = [4; member::SECRET_LEN];
    let mark = FleetSecret::new(&secret).unwrap().mark();
    let mut good_peer = Peer::own(&good, ("192.168.1.11", 7100, "0.150.0"), 1)
        .with_primary_name(&good, Some("lab-peer"));
    good_peer.fleet = Some(mark);
    let mut bad_peer = Peer::own(&bad, ("192.168.1.33", 7100, "0.150.0"), 1)
        .with_primary_name(&bad, Some("forged-peer"));
    bad_peer.fleet = Some(mark);
    bad_peer.host = "192.168.1.34".into();
    member::write_held(&state, Began::Here, |book| {
        book.write_key(&secret).map_err(|e| e.to_string())?;
        book.update(|peers| {
            peers.extend([good_peer, bad_peer]);
            Ok(())
        })
    })
    .unwrap()
    .unwrap();
    aterm_uds::private_file::write_private(&state, member::NAME_FILE, b"m62\n").unwrap();
    let inputs = collect_local_census_inputs(&state, None, Vec::new(), 100);
    assert_eq!(
        inputs.fleet_names.get(good.node()).map(String::as_str),
        Some("lab-peer")
    );
    assert!(!inputs.fleet_names.contains_key(bad.node()));
    assert_eq!(
        inputs.fleet_names.get(own.node()).map(String::as_str),
        Some("m62")
    );
    std::fs::remove_file(state.join(member::NAME_FILE)).unwrap();
    assert!(
        !collect_local_census_inputs(&state, None, Vec::new(), 101)
            .fleet_names
            .contains_key(own.node())
    );
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
    assert_eq!(member.facts_at, Some(now - 20));
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

#[test]
fn census_facts_receipt_stays_old_when_presence_is_fresh() {
    let now = 1_800_000_000;
    let mut inputs = healthy_pool_inputs(now);
    inputs.heartbeats[0].received_at = Some(now - 3600);
    inputs.server_heard.push(ServerHeardFact {
        vault: "alab".to_owned(),
        subject: "m22".to_owned(),
        at: now,
    });
    let census = SwarmCensus::compute(&inputs);
    let member = &census.members[0];
    assert_eq!(member.last_seen, Some(now));
    assert_eq!(member.facts_at, Some(now - 3600));
    assert_eq!(SwarmCensus::from_json(&census.to_json()).unwrap(), census);
    assert!(census.render_human().contains("FACTS_AT"));
    inputs.heartbeats[0].received_at = None;
    assert_eq!(SwarmCensus::compute(&inputs).members[0].facts_at, None);
}

#[test]
fn census_window_eligibility_expires_despite_retained_window_and_fresh_presence() {
    let seen = 1_793_300_000;
    let day = 24 * 60 * 60;
    let mut mismatches = Vec::new();
    for window in [false, true] {
        let mut inputs = healthy_pool_inputs(seen);
        inputs.heartbeats[0].facts.window = window;
        inputs.heartbeats[0].facts.window_last = Some(seen);
        inputs.heartbeats[0].received_at = Some(seen);
        inputs.bonjour_hosts.push(BonjourHostInput {
            name: "m22".into(),
            dot_local: "m22.local".into(),
            is_linux: false,
            seen_at: seen,
        });
        for (age, eligible) in [(0, true), (day, true), (day + 1, false), (8 * day, false)] {
            inputs.now_s = seen + age;
            inputs.bonjour_hosts[0].seen_at = inputs.now_s;
            let census = SwarmCensus::compute(&inputs);
            let member = &census.members[0];
            if member.eligible != eligible {
                mismatches.push((window, age, member.eligible));
            }
            assert!(member.in_fleet);
            assert_eq!(
                member.window, window,
                "the historical report remains available"
            );
            assert_eq!(member.window_last, Some(seen));
            assert_eq!(member.last_seen, Some(inputs.now_s));
            assert_eq!(
                member.accounts.warm, 1,
                "the existing assignment is retained"
            );
            assert_eq!(member.monitor_status(inputs.now_s), "online (swarm)");
        }
    }
    assert!(
        mismatches.is_empty(),
        "incorrect window eligibility: {mismatches:?}"
    );
}

#[test]
fn census_window_eligibility_requires_a_valid_window_timestamp() {
    let now = 1_793_300_000;
    for window_last in [None, Some(now + 1)] {
        let mut inputs = healthy_pool_inputs(now);
        inputs.heartbeats[0].facts.window_last = window_last;
        inputs.heartbeats[0].received_at = Some(now);
        let census = SwarmCensus::compute(&inputs);
        let member = &census.members[0];
        assert!(
            member.window,
            "the retained window flag is not an eligibility grant"
        );
        assert_eq!(member.window_last, None);
        assert!(
            !member.eligible,
            "missing or future evidence: {window_last:?}"
        );
    }
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
        ..claims::View::default()
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
            ..claims::View::default()
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
                seen_ms: Some((now_s - 10) * 1_000),
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
        .join(format!("{}.facts", denied_id.node()));
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

    // This Mac is m1, by its `aterm.toml`: this crate's tests read no host name
    // (`ai_pool::system_hostnames`), so a test that names no home names no Mac.
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
                seen_ms: Some((now_s - 10) * 1_000),
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

    let mut log_admitted = sample_member("m2", &[], "age1m2", None);
    log_admitted.node_key = Some(good_id.key().hex());
    let mut bad_member = sample_member("m8", &[], "age1m8", None);
    bad_member.node_key = Some(denied_id.key().hex());
    // This Mac is m1, by its `aterm.toml`: this crate's tests read no host name
    // (`ai_pool::system_hostnames`), so a test that names no home names no Mac.
    let home = dir.path().join("home");
    std::fs::create_dir_all(home.join(".config/aterm")).unwrap();
    std::fs::write(
        home.join(".config/aterm/aterm.toml"),
        "[swarm]\nlabel = \"m1\"\n",
    )
    .unwrap();
    let srv_dir = aterm_tempfile::tempdir().expect("server dir");
    let socket = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let running = aterm_secrets::server::start(
        socket,
        &aterm_secrets::server::Options {
            listen: "127.0.0.1:0".parse().unwrap(),
            store: srv_dir.path().to_path_buf(),
            advertise: None,
            pins: Vec::new(),
            hold: Duration::from_millis(50),
            keep_awake: false,
        },
    )
    .expect("start server");
    let url = format!("http://{}/v1/test", running.addr);
    let paths = aterm_secrets::client::Paths::for_home(&home);
    aterm_secrets::client::create(&paths, &url, None, Some("m1")).expect("create vault");
    let seeded_roster = SwarmRoster {
        members: vec![log_admitted, bad_member],
        deny_labels: Vec::new(),
        deny_nodes: vec![denied_id.key().hex()],
    };
    let adm = aterm_secrets::client::admin_open(&paths).expect("admin open");
    let mut doc = adm.doc.clone();
    doc.set_entry(
        &aterm_secrets::text::Audience::Everyone,
        aterm_secrets::swarm_roster::ROSTER_ENTRY,
        seeded_roster.to_toml().as_bytes(),
    )
    .expect("set roster");
    aterm_secrets::client::admin_publish(&paths, &adm, &doc).expect("publish roster");
    aterm_secrets::client::sync(&paths, None).expect("sync local copy");

    let mut inputs = collect_local_census_inputs(&state, Some(&home), Vec::new(), now_s);
    inputs.steward = None;
    let shared_key = crate::harness::claims::LeaseKey::new("claude", "shared@example.com").unwrap();
    inputs.claims = crate::harness::claims::Book::of([
        crate::harness::claims::Row {
            machine: "00000000000000000000000000000008".to_owned(),
            label: "m8".to_owned(),
            counter: 1,
            until: (now_s + 3_600) as i64,
            claims: vec![crate::harness::claims::Claim {
                key: shared_key.clone(),
                kind: crate::harness::claims::Kind::Lease,
                since: (now_s - 500) as i64,
            }],
        },
        crate::harness::claims::Row {
            machine: "00000000000000000000000000000002".to_owned(),
            label: "m2".to_owned(),
            counter: 1,
            until: (now_s + 3_600) as i64,
            claims: vec![crate::harness::claims::Claim {
                key: shared_key,
                kind: crate::harness::claims::Kind::Lease,
                since: (now_s - 100) as i64,
            }],
        },
    ]);
    inputs.server_heard.push(ServerHeardFact {
        vault: "test".to_owned(),
        subject: "age1m2".to_owned(),
        at: now_s - 2,
    });
    inputs.server_heard.push(ServerHeardFact {
        vault: "test".to_owned(),
        subject: "age1m8".to_owned(),
        at: now_s - 1,
    });
    let census = SwarmCensus::compute(&inputs);
    assert_eq!(
        census
            .members
            .iter()
            .map(|m| (
                m.label.as_str(),
                m.node.as_deref(),
                m.last_seen,
                m.accounts.leased
            ))
            .collect::<Vec<_>>(),
        vec![("m2", Some(good_id.node()), Some(now_s - 2), 1)],
        "log-admitted member with node_key must bind node_id, resolve age1 recipient in server_heard, and count its lease without being shadowed by denied m8"
    );

    // Even if `inputs.roster.members` contains a stale label-denied row (`m40`) before active `m2`
    // sharing `m2`'s `node_id` and `node_key`, `SwarmCensus::compute` must not propagate `m2`'s
    // active `node_id` / `node_key` into `deny_nodes` or drop `m2`.
    inputs.roster.deny_labels.push("m40".to_owned());
    inputs.roster.members.insert(
        0,
        SwarmMember {
            recipient: "age1m40stale".to_owned(),
            vault_name: "m40".to_owned(),
            label: "m40".to_owned(),
            aliases: Vec::new(),
            hint: None,
            hw_hint: None,
            admitted_ms: 500,
            claimed_label: None,
            claimed_at_ms: None,
            node_id: Some(good_id.node().to_owned()),
            node_key: Some(good_id.key().hex()),
        },
    );
    let census_with_stale = SwarmCensus::compute(&inputs);
    assert_eq!(
        census_with_stale
            .members
            .iter()
            .map(|m| (m.label.as_str(), m.node.as_deref()))
            .collect::<Vec<_>>(),
        vec![("m2", Some(good_id.node()))],
        "stale label-denied row sharing node_id/node_key with active m2 must not poison m2 in SwarmCensus::compute"
    );

    // Even when `aterm-swarm-roster` in `vault` has empty `deny_nodes` (`m8` still listed as a
    // member with `node_id: None, node_key: Some(denied_id.key().hex())`), if `LocalLog` holds
    // `denied_id.key()` out (because a log member rotated from `denied_id` to `good_id` in
    // `records`), `collect_local_census_inputs` must propagate `denied_id`'s key and node into
    // `roster.deny_nodes` and exclude `m8` from `SwarmCensus::compute`.
    let roster_without_explicit_deny = SwarmRoster {
        members: vec![
            SwarmMember {
                recipient: "age1m2".to_owned(),
                vault_name: "m2".to_owned(),
                label: "m2".to_owned(),
                aliases: Vec::new(),
                hint: None,
                hw_hint: None,
                admitted_ms: 100,
                claimed_label: None,
                claimed_at_ms: None,
                node_id: None,
                node_key: Some(good_id.key().hex()),
            },
            SwarmMember {
                recipient: "age1m8".to_owned(),
                vault_name: "m8".to_owned(),
                label: "m8".to_owned(),
                aliases: Vec::new(),
                hint: None,
                hw_hint: None,
                admitted_ms: 100,
                claimed_label: None,
                claimed_at_ms: None,
                node_id: None,
                node_key: Some(denied_id.key().hex()),
            },
        ],
        deny_labels: Vec::new(),
        deny_nodes: Vec::new(),
    };
    let adm = aterm_secrets::client::admin_open(&paths).expect("admin open");
    let mut doc = adm.doc.clone();
    doc.set_entry(
        &aterm_secrets::text::Audience::Everyone,
        aterm_secrets::swarm_roster::ROSTER_ENTRY,
        roster_without_explicit_deny.to_toml().as_bytes(),
    )
    .expect("set roster without explicit deny");
    aterm_secrets::client::admin_publish(&paths, &adm, &doc)
        .expect("publish roster without explicit deny");
    aterm_secrets::client::admin_seed_swarm_roster(&paths, &[]).expect("seed roster");
    aterm_secrets::client::seed_vault_genesis(&paths, &dir.path().join("test.genesis"))
        .expect("seed genesis");
    aterm_secrets::client::admin_mirror_log(&paths, &[]).expect("mirror log");
    assert!(crate::fleet_log::ensure_local_node_key_in_log(
        &paths, &denied_id, None
    ));
    assert!(crate::fleet_log::ensure_local_node_key_in_log(
        &paths, &good_id, None
    ));
    aterm_secrets::client::sync(&paths, None).expect("sync local copy");
    let log_held_out_inputs = collect_local_census_inputs(&state, Some(&home), Vec::new(), now_s);
    assert!(
        log_held_out_inputs
            .roster
            .is_node_denied(&denied_id.key().hex()),
        "log-held-out node_key must be added to roster.deny_nodes by collect_local_census_inputs"
    );
    assert!(
        log_held_out_inputs.roster.is_node_denied(denied_id.node()),
        "log-held-out peer node_id must be added to roster.deny_nodes by collect_local_census_inputs"
    );
    let log_held_out_census = SwarmCensus::compute(&log_held_out_inputs);
    assert!(
        !log_held_out_census.members.iter().any(|m| m.label == "m8"),
        "member whose node_key is held out by LocalLog must be excluded from census.members: {:?}",
        log_held_out_census.members
    );

    // Even when `<state>/swarm-census.json` is fresher than `CENSUS_REFRESH_S`, if it still lists
    // a member (`m8`) that is now denied / held out by `LocalLog` (or if `swarm-facts/` holds an
    // unpruned fact for that denied node), both `read_or_compute_census` and `serve_fleet_census`
    // must immediately recompute and prune rather than serving the stale cached census.
    let mut stale_cached = log_held_out_census.clone();
    stale_cached.generated_at = now_s;
    stale_cached.members.push(CensusMember {
        label: "m8".to_owned(),
        fleet_name: None,
        aliases: Vec::new(),
        node: Some(denied_id.node().to_owned()),
        host: Some("m8.local".to_owned()),
        version: Some("0.147.0".to_owned()),
        facts_at: Some(now_s - 5),
        window: true,
        window_last: Some(now_s - 5),
        sessions: 1,
        accounts: MemberAccountSummary::default(),
        last_seen: Some(now_s - 1),
        converge_failures: 0,
        eligible: true,
        in_fleet: true,
        in_commons: true,
    });
    for served in [false, true] {
        write_cached_census(&state, &stale_cached).expect("seed stale cached census");
        store_pushed_facts(&state, denied_id.node(), &payload).expect("seed unpruned denied facts");
        let fresh_read = if served {
            let wire = serve_fleet_census(&state, Some(&home), now_s + 5).expect("serve census");
            SwarmCensus::from_json(&wire).expect("parse served census")
        } else {
            read_or_compute_census(&state, Some(&home), false, now_s + 5).expect("read census")
        };
        assert_eq!(
            fresh_read.generated_at,
            now_s + 5,
            "fresh cache containing newly denied member must be recomputed (served={served})"
        );
        assert!(
            !fresh_read.members.iter().any(|m| m.label == "m8"),
            "recomputed census must exclude denied m8 (served={served}): {:?}",
            fresh_read.members
        );
        assert!(
            read_pushed_facts(&state).is_empty(),
            "denied node facts must be pruned when checking cached census (served={served})"
        );
    }

    running.stop();
}

/// A Mac in no fleet hears nobody: its census says so instead of the ratio's 100% default, and
/// the JSON keeps the field as computed.
#[test]
fn a_census_that_heard_nobody_prints_none_heard_and_keeps_the_json_ratio() {
    let alone = SwarmCensus::compute(&CensusInputs {
        now_s: 1_793_300_000,
        ..CensusInputs::default()
    });
    assert_eq!(alone.summary.distinct_heard_7d, 0);
    let human = alone.render_human();
    let head = human.lines().next().unwrap_or_default();
    assert!(
        head.contains("(0 heard 7d, none heard)") && !head.contains('%'),
        "{head}"
    );
    assert_eq!(alone.summary.p2_ratio_bps, 10_000);
    assert!(
        alone.to_json().contains("\"p2_ratio_bps\":10000"),
        "{}",
        alone.to_json()
    );
    assert_eq!(
        SwarmCensus::from_json(&alone.to_json())
            .expect("roundtrip")
            .summary
            .p2_ratio_bps,
        10_000
    );

    // Negative control: once anyone is heard, the same 10_000 is a real ratio and prints as one.
    for (heard, bps, shown) in [
        (1, 10_000, "(1 heard 7d, 100.0% joined)"),
        (4, 5_000, "(4 heard 7d, 50.0% joined)"),
    ] {
        let mut heard_some = alone.clone();
        heard_some.summary.distinct_heard_7d = heard;
        heard_some.summary.p2_ratio_bps = bps;
        let human = heard_some.render_human();
        assert!(
            human.contains(shown) && !human.contains("none heard"),
            "{human}"
        );
    }
}

#[test]
fn census_walled_accounts_follow_limited_condition_wall_expiry_and_enrollment() {
    let now_s = 1_800_000_000_u64;
    let now_i64 = i64::try_from(now_s).unwrap();
    let n22 = "22222222222222222222222222222222";
    let n23 = "23232323232323232323232323232323";

    // Case 1: real `Condition::Limited::as_str()` ("limited") with `walled_until: None`
    // marks both pool-backed (`m22`) and heartbeat-fallback (`m23`) enrolled rows walled,
    // while unenrolled rows (`enrolled: false`) neither inflate `walled > warm` nor mark
    // another Mac's shared pool account walled.
    let inputs_active = CensusInputs {
        now_s,
        roster: SwarmRoster {
            members: vec![
                sample_member("m22", &[], "age1m22", Some(n22)),
                sample_member("m23", &[], "age1m23", Some(n23)),
            ],
            ..SwarmRoster::default()
        },
        pool: Some(Pool {
            source: "test".to_owned(),
            last_heard: BTreeMap::new(),
            accounts: vec![
                PoolAccount {
                    provider: Cli::Claude,
                    vendor: Vendor::default(),
                    label: Some("c01".to_owned()),
                    email: "c01@example.com".to_owned(),
                    browser: None,
                    warm: vec!["m22".to_owned()],
                    since: Vec::new(),
                    why: Vec::new(),
                    used: Vec::new(),
                },
                PoolAccount {
                    provider: Cli::Claude,
                    vendor: Vendor::default(),
                    label: Some("c02".to_owned()),
                    email: "c02@example.com".to_owned(),
                    browser: None,
                    warm: vec!["m22".to_owned()],
                    since: Vec::new(),
                    why: Vec::new(),
                    used: Vec::new(),
                },
            ],
        }),
        heartbeats: vec![
            HeartbeatInput {
                node: n22.to_owned(),
                facts: sample_facts(
                    n22,
                    "m22",
                    "0.148.0",
                    true,
                    Some(now_s - 5),
                    2,
                    vec![SurveyAccountFact {
                        pool: "claude".to_owned(),
                        id: "c01".to_owned(),
                        condition: crate::harness::accounts::Condition::Limited { until: 0 }
                            .as_str()
                            .to_owned(),
                        enrolled: true,
                        usage_5h_pct: Some(100),
                        usage_7d_pct: Some(80),
                        week_reset: None,
                        walled_until: None,
                        last_used: Some(now_i64 - 10),
                    }],
                ),
                received_at: Some(now_s - 5),
                converge_failures: 0,
            },
            HeartbeatInput {
                node: n23.to_owned(),
                facts: sample_facts(
                    n23,
                    "m23",
                    "0.148.0",
                    true,
                    Some(now_s - 5),
                    1,
                    vec![
                        SurveyAccountFact {
                            pool: "codex".to_owned(),
                            id: "x01".to_owned(),
                            condition: "limited".to_owned(),
                            enrolled: true,
                            usage_5h_pct: Some(100),
                            usage_7d_pct: None,
                            week_reset: None,
                            walled_until: None,
                            last_used: Some(now_i64 - 10),
                        },
                        SurveyAccountFact {
                            pool: "claude".to_owned(),
                            id: "c02".to_owned(),
                            condition: "limited".to_owned(),
                            enrolled: false,
                            usage_5h_pct: None,
                            usage_7d_pct: None,
                            week_reset: None,
                            walled_until: Some(now_i64 + 3_600),
                            last_used: None,
                        },
                    ],
                ),
                received_at: Some(now_s - 5),
                converge_failures: 0,
            },
        ],
        ..CensusInputs::default()
    };
    let active = SwarmCensus::compute(&inputs_active);
    assert_eq!(active.members[0].label, "m22");
    assert_eq!(active.members[0].accounts.warm, 2);
    assert_eq!(
        active.members[0].accounts.walled, 1,
        "enrolled `condition=limited` with `walled_until=None` must mark c01 walled while unenrolled c02 on m23 does not"
    );
    assert_eq!(active.members[1].label, "m23");
    assert_eq!(active.members[1].accounts.warm, 1);
    assert_eq!(
        active.members[1].accounts.walled, 1,
        "heartbeat fallback must count enrolled `condition=limited` and exclude unenrolled rows (`walled <= warm`)"
    );

    // Case 2: once `walled_until <= now_i64` in an older pushed `.facts` heartbeat, the wall has
    // already ended and neither `"limited"` nor `"rate_limited"` keeps the account walled.
    let mut inputs_expired = inputs_active;
    inputs_expired.heartbeats[0].facts.accounts[0].walled_until = Some(now_i64);
    inputs_expired.heartbeats[1].facts.accounts[0].condition = "rate_limited".to_owned();
    inputs_expired.heartbeats[1].facts.accounts[0].walled_until = Some(now_i64 - 1);
    let expired = SwarmCensus::compute(&inputs_expired);
    assert_eq!(
        expired.members[0].accounts.walled, 0,
        "wall at `walled_until == now` must lift for pool-backed accounts"
    );
    assert_eq!(
        expired.members[1].accounts.walled, 0,
        "wall at `walled_until < now` must lift for heartbeat-fallback accounts"
    );
}

/// NEGATIVE CONTROL: `collect_local_census_inputs` resolves `fleet_names` by highest verified
/// `signed.seq` bound to each node's established key across `fleet-peers` and `fleet-issuer-book`
/// (including clearing a superseded name when a newer signed descriptor carries none and rejecting
/// self-signed impostor keys), suppresses `fleet_name` for denied nodes, and refreshes cached
/// census outputs immediately when the local `fleet-name` file changes.
#[cfg(unix)]
#[test]
fn census_fleet_names_resolve_bound_issuer_book_sequences_reject_impostor_keys_and_refresh_local_renames()
 {
    use aterm_net::fleet::{FleetSecret, NodeIdentity};
    use aterm_net::fleet_member::{self as member, Began, ISSUER_BOOK_FILE, Peer, Via};

    let dir = aterm_tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    let home = dir.path().join("home");
    aterm_uds::private_file::ensure_private_dir(&state).unwrap();
    std::fs::create_dir_all(home.join(".config/aterm")).unwrap();
    std::fs::write(
        home.join(".config/aterm/aterm.toml"),
        "[ai]\nmachine = \"m22\"\n",
    )
    .unwrap();

    let own = member::node_identity(&state).unwrap();
    let issuer = NodeIdentity::from_seed("10101010101010101010101010101010", &[10; 32]).unwrap();
    let peer_a = NodeIdentity::from_seed("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", &[11; 32]).unwrap();
    let peer_b = NodeIdentity::from_seed("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", &[12; 32]).unwrap();
    let peer_c = NodeIdentity::from_seed("cccccccccccccccccccccccccccccccc", &[13; 32]).unwrap();
    let impostor = NodeIdentity::from_seed("cccccccccccccccccccccccccccccccc", &[99; 32]).unwrap();
    assert_ne!(peer_c.key(), impostor.key());

    let secret = [7; member::SECRET_LEN];
    let mark = FleetSecret::new(&secret).unwrap().mark();

    let mut issuer_peer = Peer::own(&issuer, ("192.168.1.10", 7100, "0.150.0"), 1);
    issuer_peer.via = Via::Issuer;
    issuer_peer.fleet = Some(mark);

    // `peer_a` is unsigned in `fleet-peers` (`signed = None`), while `fleet-issuer-book` holds a
    // newer self-signed copy under `peer_a`'s bound key with `primary_name = "mac-a"`.
    let mut a_unsigned = Peer::own(&peer_a, ("192.168.1.21", 7100, "0.150.0"), 1);
    a_unsigned.signed = None;
    a_unsigned.fleet = Some(mark);

    // `peer_b` has `seq = 10` with `"old-b"` in `fleet-peers`, while `fleet-issuer-book` holds a
    // newer `seq = 20` signed by `peer_b` with `primary_name = None` (clearing the name).
    let mut b_old = Peer::own(&peer_b, ("192.168.1.22", 7100, "0.150.0"), 10)
        .with_primary_name(&peer_b, Some("old-b"));
    b_old.fleet = Some(mark);

    // `peer_c` is bound to `peer_c.key()` in `fleet-peers` (`signed = None`); `fleet-issuer-book`
    // holds a self-signed copy claiming `peer_c.node()` under `impostor.key()`.
    let mut c_bound = Peer::own(&peer_c, ("192.168.1.23", 7100, "0.150.0"), 1);
    c_bound.signed = None;
    c_bound.fleet = Some(mark);

    member::write_held(&state, Began::Here, |book| {
        book.write_key(&secret).map_err(|e| e.to_string())?;
        book.update(|peers| {
            peers.extend([issuer_peer, a_unsigned, b_old, c_bound]);
            Ok(())
        })
    })
    .unwrap()
    .unwrap();

    let mut a_copy = Peer::own(&peer_a, ("192.168.1.21", 7100, "0.150.0"), 5)
        .with_primary_name(&peer_a, Some("mac-a"));
    a_copy.via = Via::IssuerBook;
    a_copy.fleet = None;

    let mut b_cleared = Peer::own(&peer_b, ("192.168.1.22", 7100, "0.150.0"), 20);
    b_cleared.via = Via::IssuerBook;
    b_cleared.fleet = None;

    let mut c_forged = Peer::own(&impostor, ("192.168.1.99", 7100, "0.150.0"), 99)
        .with_primary_name(&impostor, Some("spoofed"));
    c_forged.via = Via::IssuerBook;
    c_forged.fleet = None;
    assert!(c_forged.signature_holds());

    let issuer_book_wire = format!(
        "issuer node={}\n{}\n{}\n{}\n",
        issuer.node(),
        a_copy.line(),
        b_cleared.line(),
        c_forged.line(),
    );
    aterm_uds::private_file::write_private(&state, ISSUER_BOOK_FILE, issuer_book_wire.as_bytes())
        .unwrap();
    aterm_uds::private_file::write_private(&state, member::NAME_FILE, b"m62\n").unwrap();

    let now_s = 1_800_000_000;
    let inputs = collect_local_census_inputs(&state, Some(&home), Vec::new(), now_s);
    assert_eq!(
        inputs.fleet_names.get(peer_a.node()).map(String::as_str),
        Some("mac-a"),
        "bound issuer-book copy must supply signed primary name when fleet-peers entry is unsigned"
    );
    assert!(
        !inputs.fleet_names.contains_key(peer_b.node()),
        "newer signed sequence without primary name must clear older signed name"
    );
    assert!(
        !inputs.fleet_names.contains_key(peer_c.node()),
        "impostor key claiming an already-bound node id must be rejected"
    );
    assert_eq!(
        inputs.fleet_names.get(own.node()).map(String::as_str),
        Some("m62")
    );

    // Local rename or removal invalidates the cached census within `CENSUS_REFRESH_S`.
    let initial = read_or_compute_census(&state, Some(&home), false, now_s).unwrap();
    let own_row = initial
        .members
        .iter()
        .find(|m| m.node.as_deref() == Some(own.node()))
        .unwrap();
    assert_eq!(own_row.fleet_name.as_deref(), Some("m62"));

    aterm_uds::private_file::write_private(&state, member::NAME_FILE, b"m63\n").unwrap();
    let renamed = read_or_compute_census(&state, Some(&home), false, now_s + 5).unwrap();
    let renamed_row = renamed
        .members
        .iter()
        .find(|m| m.node.as_deref() == Some(own.node()))
        .unwrap();
    assert_eq!(renamed_row.fleet_name.as_deref(), Some("m63"));

    std::fs::remove_file(state.join(member::NAME_FILE)).unwrap();
    let served_wire = serve_fleet_census(&state, Some(&home), now_s + 10).unwrap();
    let cleared = SwarmCensus::from_json(&served_wire).unwrap();
    let cleared_row = cleared
        .members
        .iter()
        .find(|m| m.node.as_deref() == Some(own.node()))
        .unwrap();
    assert_eq!(cleared_row.fleet_name, None);

    // Case-insensitive lookup and `deny_nodes` filtering in `SwarmCensus::compute`.
    let mut upper_inputs = clock_inputs(now_s);
    upper_inputs.fleet_names.insert(
        "22222222222222222222222222222222".to_ascii_uppercase(),
        "upper-mac".into(),
    );
    assert_eq!(
        SwarmCensus::compute(&upper_inputs).members[0]
            .fleet_name
            .as_deref(),
        Some("upper-mac")
    );
}
