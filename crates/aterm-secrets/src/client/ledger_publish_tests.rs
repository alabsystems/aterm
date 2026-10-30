// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::server::{AlabNameInput, AlabNameWatcher, Options as ServerOptions};
use crate::swarm_roster::{
    LEASE_CLOCK_MARGIN_SECS, LEASE_TTL_SECS, StewardPolicy, SwarmLease, SwarmPolicy,
};

fn join_host(paths: &Paths, source: &str, host: &str) {
    join(
        paths,
        &JoinOptions {
            source: source.to_owned(),
            host: Some(host.to_owned()),
            interactive: false,
            wait: Duration::from_secs(2),
            confirm: &|_| false,
            password: JoinPassword::Given(None),
            fetched: None,
            requested: &|| {},
            say: &|_| {},
        },
    )
    .expect("join host");
}

fn start_loopback_server(store: &Path) -> crate::server::Running {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    crate::server::start(
        listener,
        &ServerOptions {
            listen: "127.0.0.1:0".parse().expect("addr"),
            store: store.to_path_buf(),
            advertise: None,
            pins: Vec::new(),
            hold: Duration::from_millis(100),
            keep_awake: false,
        },
    )
    .expect("start server")
}

/// Replay one abstract `LedgerPublish` transition through the real parent-CAS / merge / lease /
/// read-back primitives to verify model-to-code conformance (`StepEvidence`).
pub(crate) fn ledger_publish_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    let get = |k: &str| *state.get(k).unwrap_or(&0);

    match action {
        "Partition" => {
            next.insert("part", 1);
        }
        "Heal" => {
            next.insert("part", 0);
        }
        "RevokeOnS1" => {
            let s1_seq = get("s1_seq") + 1;
            next.insert("s1_seq", s1_seq);
            next.insert("s1_rev", 1);
            next.insert("ever_rev", 1);
            if get("part") == 0 {
                next.insert("s2_seq", get("s2_seq") + 1);
                next.insert("s2_rev", 1);
            }
        }
        "IslandAdvanceS2" => {
            next.insert("s2_seq", get("s2_seq") + 1);
        }
        "QueueAdmission" => {
            next.insert("q_adm", 1);
        }
        "ApproveAndReadBack" => {
            // Real spine helper confirms parent-CAS digest ordering stays bounded and non-empty.
            let h = [u8::try_from(get("s1_seq")).unwrap_or(1); 32];
            let spine = extend_parent_spine(&[h], &[]);
            assert_eq!(spine, vec![h]);
            next.insert("s1_seq", get("s1_seq") + 1);
            next.insert("s2_seq", get("s2_seq") + 1);
            next.insert("q_adm", 0);
            next.insert("adm_live", 1);
        }
        "MergeDivergedHeads" => {
            let h1 = [1u8; 32];
            let h2 = [2u8; 32];
            let spine = extend_parent_spine(&[h1, h2], &[]);
            assert!(spine.contains(&h1) && spine.contains(&h2));
            let merged_seq = get("s1_seq").max(get("s2_seq")) + 1;
            let merged_rev = i64::from(get("s1_rev") + get("s2_rev") > 0);
            next.insert("s1_seq", merged_seq);
            next.insert("s2_seq", merged_seq);
            next.insert("s1_rev", merged_rev);
            next.insert("s2_rev", merged_rev);
            if get("q_adm") == 1 {
                next.insert("adm_live", 1);
            }
            next.insert("q_adm", 0);
        }
        "MemberSync" => {
            next.insert("m_seq", get("s1_seq"));
        }
        "ClaimLease" => {
            let policy = SwarmPolicy {
                network_id: Some("alab-lan".to_owned()),
                stewards: vec![StewardPolicy {
                    label: "m3".to_owned(),
                    rank: 1,
                    always_on: true,
                }],
                ..SwarmPolicy::default()
            };
            let decision = crate::swarm_roster::decide_lease_action(
                &policy,
                "m3",
                Some("alab-lan"),
                None,
                1_800_000_000,
            );
            assert!(matches!(
                decision,
                crate::swarm_roster::LeaseDecision::Claim(ref l) if l.epoch == 1
            ));
            next.insert("lease_ep", 1);
            next.insert("ep1_holders", 1);
            next.insert("s1_seq", get("s1_seq") + 1);
            next.insert("s2_seq", get("s2_seq") + 1);
        }
        "RenewLease" => {
            let policy = SwarmPolicy {
                network_id: Some("alab-lan".to_owned()),
                stewards: vec![StewardPolicy {
                    label: "m3".to_owned(),
                    rank: 1,
                    always_on: true,
                }],
                ..SwarmPolicy::default()
            };
            let cur = SwarmLease {
                holder: "m3".to_owned(),
                epoch: 1,
                until: 1_800_000_000 + 3_600,
            };
            let decision = crate::swarm_roster::decide_lease_action(
                &policy,
                "m3",
                Some("alab-lan"),
                Some(&cur),
                1_800_000_000,
            );
            assert!(matches!(
                decision,
                crate::swarm_roster::LeaseDecision::Renew(ref l) if l.epoch == 1
            ));
            next.insert("s1_seq", get("s1_seq") + 1);
            next.insert("s2_seq", get("s2_seq") + 1);
        }
        "ExpireLease" => {
            next.insert("lease_exp", 1);
        }
        "TakeoverLease" => {
            let policy = SwarmPolicy {
                network_id: Some("alab-lan".to_owned()),
                stewards: vec![
                    StewardPolicy {
                        label: "m3".to_owned(),
                        rank: 1,
                        always_on: true,
                    },
                    StewardPolicy {
                        label: "m5".to_owned(),
                        rank: 2,
                        always_on: true,
                    },
                ],
                ..SwarmPolicy::default()
            };
            let cur = SwarmLease {
                holder: "m3".to_owned(),
                epoch: 1,
                until: 1_800_000_000,
            };
            let decision = crate::swarm_roster::decide_lease_action(
                &policy,
                "m5",
                Some("alab-lan"),
                Some(&cur),
                1_800_000_000 + LEASE_CLOCK_MARGIN_SECS,
            );
            assert!(matches!(
                decision,
                crate::swarm_roster::LeaseDecision::Takeover(ref l) if l.epoch == 2
            ));
            next.insert("lease_ep", 2);
            next.insert("lease_exp", 0);
            next.insert("ep2_holders", 1);
            next.insert("s1_seq", get("s1_seq") + 1);
            next.insert("s2_seq", get("s2_seq") + 1);
        }
        other => panic!("unexpected enabled action in Buggy=0 model: {other}"),
    }
    next
}

#[test]
fn ledger_publish_model_conforms_across_reachable_transitions_and_catches_mutants() {
    let model = aterm_models_swarm::ledger_publish_model();
    aterm_spec::interp::prove_and_catch(&model);

    let reachable = aterm_spec::interp::reachable_states(&model);
    assert!(
        reachable.len() >= 20,
        "expected non-trivial state space, got {}",
        reachable.len()
    );
    let fired = aterm_spec::interp::fired_actions(&model);
    // All 12 non-mutant actions must fire under Buggy = 0.
    assert_eq!(fired.len(), 12, "fired actions: {fired:?}");

    for state in &reachable {
        for action in &model.actions {
            if model.action_enabled(action.name, state) {
                let next = ledger_publish_transition(state, action.name);
                assert!(
                    model.successors(action.name, state).contains(&next),
                    "conformance mismatch on {}: {next:?}",
                    action.name
                );
                for inv in &model.invariants {
                    assert!(
                        model.check_invariant(inv.name, &next),
                        "invariant {} failed on {}",
                        inv.name,
                        action.name
                    );
                }
            }
        }
    }

    // Negative control: a buggy merge that drops `s1_rev` violates `RevocationsGrowOnly`.
    let mut split_healed = model.init_state();
    split_healed.insert("ever_rev", 1);
    split_healed.insert("s1_seq", 2);
    split_healed.insert("s2_seq", 3);
    split_healed.insert("s1_rev", 1);
    split_healed.insert("s2_rev", 0);
    assert!(model.action_enabled("MergeDivergedHeads", &split_healed));
    let mut buggy_merge = ledger_publish_transition(&split_healed, "MergeDivergedHeads");
    buggy_merge.insert("s1_rev", 0);
    buggy_merge.insert("s2_rev", 0);
    assert!(
        !model
            .successors("MergeDivergedHeads", &split_healed)
            .contains(&buggy_merge)
    );
    assert!(!model.check_invariant("RevocationsGrowOnly", &buggy_merge));
}

#[test]
fn approver_read_back_gates_admission_retirement_and_negative_control_drops_it() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let srv = start_loopback_server(&root.path().join("srv"));
    let url = format!("http://{}/v1/team", srv.addr);
    let st = Paths::for_home(&root.path().join("st"));
    create(&st, &url, None, Some("m3")).expect("create");

    let newcomer = crate::age::Identity::generate()
        .expect("generate newcomer")
        .recipient()
        .to_bech32();
    let admin = admin_open(&st).expect("open seq 1");

    // Negative control: before publishing `newcomer`, the server still serves seq 1 without
    // `newcomer`. `verify_approved_in_served_head` MUST refuse so the approver never marks the
    // queued join request answered prematurely!
    let premature = verify_approved_in_served_head(&st, &admin, 2, std::slice::from_ref(&newcomer));
    assert!(
        premature.is_err(),
        "read-back must fail when the server has not stored the approved newcomer"
    );

    // Once published, read-back succeeds!
    let mut doc = admin.doc.clone();
    doc.add_machine("m14", &newcomer).expect("add m14");
    let published = admin_publish(&st, &admin, &doc).expect("publish seq 2");
    verify_approved_in_served_head(&st, &admin, published.seq, &[newcomer])
        .expect("read-back succeeds once newcomer is in the served head");

    srv.stop();
}

#[test]
fn steward_lease_cas_prevents_split_brain_epoch_holders() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let srv = start_loopback_server(&root.path().join("srv"));
    let url = format!("http://{}/v1/{}", srv.addr, crate::ALAB_VAULT);

    let st1 = Paths::for_home(&root.path().join("st1"));
    test_create_unbound(&st1, &url, None, Some("m3")).expect("create alab vault");
    let st2 = Paths::for_home(&root.path().join("st2"));
    let st2_rec = machine_identity(&st2).unwrap().recipient().to_bech32();
    let laptop = Paths::for_home(&root.path().join("m22"));
    let m22_rec = machine_identity(&laptop).unwrap().recipient().to_bech32();

    let policy = SwarmPolicy {
        network_id: Some("alab-lan".to_owned()),
        stewards: vec![
            StewardPolicy {
                label: "m3".to_owned(),
                rank: 1,
                always_on: true,
            },
            StewardPolicy {
                label: "m5".to_owned(),
                rank: 2,
                always_on: true,
            },
            StewardPolicy {
                label: "m22".to_owned(),
                rank: 3,
                always_on: false,
            },
        ],
        ..SwarmPolicy::default()
    };

    {
        let admin = admin_open(&st1).expect("open");
        let mut doc = admin.doc.clone();
        doc.add_machine("m5", &st2_rec).unwrap();
        doc.add_admin("m5").unwrap();
        doc.add_machine("m22", &m22_rec).unwrap();
        doc.add_admin("m22").unwrap();
        let admins = doc.admins();
        doc.set_entry(
            &Audience::Hosts(admins),
            crate::SWARM_POLICY_ENTRY,
            policy.to_toml().as_bytes(),
        )
        .unwrap();
        admin_publish(&st1, &admin, &doc).unwrap();
    }

    let pin = config(&st1).unwrap().unwrap().signer.unwrap();
    let pin_b64 = crate::b64::encode_padded(&pin);
    for (paths, host) in [(&st2, "m5"), (&laptop, "m22")] {
        join_host(paths, &format!("{url}#{pin_b64}"), host);
    }

    struct ResetRetired;
    impl Drop for ResetRetired {
        fn drop(&mut self) {
            envelope::TEST_RETIRED.with(|cell| *cell.borrow_mut() = None);
        }
    }
    envelope::TEST_RETIRED.with(|cell| {
        *cell.borrow_mut() = Some(Retired {
            keys: Vec::new(),
            current: Some(pin),
        });
    });
    let _reset = ResetRetired;

    let t0 = 1_800_000_000;
    // Laptop steward (`always_on = false`) and off-LAN stewards NEVER claim the lease.
    assert!(matches!(
        steward_ensure_lease(&laptop, Some("alab-lan"), t0).unwrap(),
        StewardLease::Idle(_)
    ));
    assert!(matches!(
        steward_ensure_lease(&st1, Some("coffee-shop-wifi"), t0).unwrap(),
        StewardLease::Idle(_)
    ));

    // m3 claims epoch 1, then sees Current while >= 3 h remain, then renews when < 3 h remain.
    assert!(matches!(
        steward_ensure_lease(&st1, Some("alab-lan"), t0).unwrap(),
        StewardLease::Done(_)
    ));
    assert!(matches!(
        steward_ensure_lease(&st1, Some("alab-lan"), t0 + 3_600).unwrap(),
        StewardLease::Current(_)
    ));
    assert!(matches!(
        steward_ensure_lease(&st1, Some("alab-lan"), t0 + 4 * 3_600).unwrap(),
        StewardLease::Done(_)
    ));

    // Contended CAS race: if st2 held a stale admin snapshot from before st1's renewal and tries
    // to publish a competing epoch-2 takeover over the same parent, parent-CAS rejects the stale
    // write (`NotStored`), and `steward_ensure_lease` converges cleanly to `Current`!
    sync(&st2, None).unwrap();
    let expired_at = t0 + 4 * 3_600 + LEASE_TTL_SECS + LEASE_CLOCK_MARGIN_SECS + 120;
    let stale_st1 = admin_open(&st1).unwrap();
    assert!(matches!(
        steward_ensure_lease(&st2, Some("alab-lan"), expired_at).unwrap(),
        StewardLease::Done(_)
    ));
    let mut competing_doc = stale_st1.published_doc.clone();
    let admins = competing_doc.admins();
    competing_doc
        .set_entry(
            &Audience::Hosts(admins),
            crate::SWARM_LEASE_ENTRY,
            SwarmLease {
                holder: "m3".to_owned(),
                epoch: 2,
                until: expired_at + LEASE_TTL_SECS,
            }
            .to_toml()
            .as_bytes(),
        )
        .unwrap();
    let cas_rejected = admin_publish(&st1, &stale_st1, &competing_doc)
        .expect_err("parent-CAS must reject second contender at epoch 2");
    assert_eq!(cas_rejected.kind(), crate::ErrorKind::NotStored);
    assert_eq!(swarm_lease_holder(&st2).unwrap().as_deref(), Some("m5"));

    srv.stop();
}

fn alab_holder_hint(seq: u64, label: &str) -> crate::discover::DiscoveredVault {
    crate::discover::DiscoveredVault {
        url: format!("http://alab-secrets.local:{}/v1/alab", crate::DEFAULT_PORT),
        txt: Some(crate::server::VaultTxt {
            vault: "alab".to_owned(),
            signer: "alab-fp".to_owned(),
            seq,
            head: [0x11; 32],
            label: Some(label.to_owned()),
        }),
    }
}

/// With no lease published anywhere, once `alab-secrets.local` goes silent for 90 continuous
/// seconds, a server takes the name (`docs/DESIGN-fluid-swarm-2026-10-28.md` F7).
/// NEGATIVE CONTROL: the retired lease-required rule advertises nothing when `lease` is `None`.
#[test]
fn with_no_lease_one_server_takes_the_name_after_the_holder_goes_silent() {
    let start = Instant::now();
    // Stale mDNS TXT from a crashed former holder (`m3` at seq 10) must NOT prevent 90 s fallback
    // when nothing actually answers on `alab-secrets.local`.
    let stale_holders = [alab_holder_hint(10, "m3")];
    let mut watcher = AlabNameWatcher::new();
    assert!(!watcher.decide(
        &AlabNameInput {
            own_label: "m5",
            own_seq: Some(10),
            holders: &stale_holders,
            now: start,
        },
        |_| Some(10),
    ));
    let silence_start = start + Duration::from_secs(10);
    for secs in [0_u64, 30, 60, 89] {
        assert!(
            !watcher.decide(
                &AlabNameInput {
                    own_label: "m5",
                    own_seq: Some(10),
                    holders: &stale_holders,
                    now: silence_start + Duration::from_secs(secs),
                },
                |_| None,
            ),
            "must NOT take alab-secrets before 90s of continuous silence (at {secs}s)"
        );
    }
    assert!(
        watcher.decide(
            &AlabNameInput {
                own_label: "m5",
                own_seq: Some(10),
                holders: &stale_holders,
                now: silence_start + Duration::from_secs(90),
            },
            |_| None,
        ),
        "must take alab-secrets at 90s continuous outage with no lease"
    );

    // Negative control: under the retired lease-required rule (`lease: None`), nobody ever takes
    // `alab-secrets`, even past 90 s of silence.
    let lease_required_decide = |lease: Option<&SwarmLease>, elapsed_secs: u64| -> bool {
        let Some(_lease) = lease else {
            return false;
        };
        elapsed_secs >= 90
    };
    assert!(
        !lease_required_decide(None, 120),
        "negative control: lease-required rule leaves alab-secrets unheld when no lease exists"
    );
}

/// A returning server (`m3`) leaves `alab-secrets` to a holder (`m19`) that already answers with a
/// verified ALab envelope, even when the returning server has a lower label rank (`m3 < m19`).
/// NEGATIVE CONTROL: removing the holder-answers gate causes the returning lower-label server to
/// grab the name immediately and flap it.
#[test]
fn a_returning_server_leaves_the_name_to_a_holder_that_answers() {
    let start = Instant::now();
    let holders = [alab_holder_hint(12, "m19")];
    let mut w_m19 = AlabNameWatcher::new();
    let _ = w_m19.decide(
        &AlabNameInput {
            own_label: "m19",
            own_seq: Some(12),
            holders: &[],
            now: start,
        },
        |_| None,
    );
    assert!(w_m19.decide(
        &AlabNameInput {
            own_label: "m19",
            own_seq: Some(12),
            holders: &holders,
            now: start + Duration::from_secs(90),
        },
        |_| None,
    ));

    // Lower-numbered `m3` returns at `seq 12`; because `m19` answers (`probe_alab == Some(12)`),
    // `m3` leaves `alab-secrets` to `m19` across every tick.
    let mut w_m3 = AlabNameWatcher::new();
    for secs in [95_u64, 150, 240] {
        assert!(
            !w_m3.decide(
                &AlabNameInput {
                    own_label: "m3",
                    own_seq: Some(12),
                    holders: &holders,
                    now: start + Duration::from_secs(secs),
                },
                |_| Some(12),
            ),
            "returning server must leave alab-secrets to an answering holder (at {secs}s)"
        );
    }

    // Negative control: a flap-prone rule that skips the holder-answers check and claims the name
    // whenever no peer outranks `(own_seq, own_label)` makes returning `m3` immediately collide.
    let without_holder_answers_gate =
        |own_seq: u64, own_label: &str, peer_seq: u64, peer_label: &str| -> bool {
            own_seq > peer_seq
                || (own_seq == peer_seq
                    && crate::server::label_rank(own_label) < crate::server::label_rank(peer_label))
        };
    assert!(
        without_holder_answers_gate(12, "m3", 12, "m19"),
        "negative control: skipping holder-answers check makes returning m3 flap m19"
    );
}

/// When two islands heal and both servers hold `alab-secrets`, deterministic TXT ordering
/// (`seq` descending, then natural label order `m3 < m5 < m19 < m22`) settles on exactly one
/// holder (`docs/DESIGN-fluid-swarm-2026-10-28.md` F7).
/// NEGATIVE CONTROL: without the tie-break, symmetric holders either both keep or both drop.
#[test]
fn two_holders_at_a_heal_settle_on_one() {
    let start = Instant::now();
    let make_holding = |label: &str, seq: u64| -> AlabNameWatcher {
        let mut w = AlabNameWatcher::new();
        let _ = w.decide(
            &AlabNameInput {
                own_label: label,
                own_seq: Some(seq),
                holders: &[],
                now: start,
            },
            |_| None,
        );
        assert!(w.decide(
            &AlabNameInput {
                own_label: label,
                own_seq: Some(seq),
                holders: &[],
                now: start + Duration::from_secs(90),
            },
            |_| None,
        ));
        w
    };

    // 1. Equal `seq = 7` on `m5` and `m19` (note lexicographically `"m19" < "m5"`, whereas natural
    //    swarm label order is `m5` (5) < `m19` (19)): `m5` keeps the name and `m19` withdraws.
    let mut w_m5 = make_holding("m5", 7);
    let mut w_m19 = make_holding("m19", 7);
    let equal_seq_holders = [alab_holder_hint(7, "m5"), alab_holder_hint(7, "m19")];
    let m5_keeps = w_m5.decide(
        &AlabNameInput {
            own_label: "m5",
            own_seq: Some(7),
            holders: &equal_seq_holders,
            now: start + Duration::from_secs(100),
        },
        |_| Some(7),
    );
    let m19_keeps = w_m19.decide(
        &AlabNameInput {
            own_label: "m19",
            own_seq: Some(7),
            holders: &equal_seq_holders,
            now: start + Duration::from_secs(100),
        },
        |_| Some(7),
    );
    assert!(
        m5_keeps && !m19_keeps,
        "expected m5 only: m5={m5_keeps}, m19={m19_keeps}"
    );

    // 2. Higher `seq` beats lower label: `m19` at `seq = 9` keeps the name over `m3` at `seq = 8`.
    let mut w_m3_lagging = make_holding("m3", 8);
    let mut w_m19_ahead = make_holding("m19", 9);
    let diff_seq_holders = [alab_holder_hint(8, "m3"), alab_holder_hint(9, "m19")];
    let m3_keeps = w_m3_lagging.decide(
        &AlabNameInput {
            own_label: "m3",
            own_seq: Some(8),
            holders: &diff_seq_holders,
            now: start + Duration::from_secs(100),
        },
        |_| Some(9),
    );
    let m19_ahead_keeps = w_m19_ahead.decide(
        &AlabNameInput {
            own_label: "m19",
            own_seq: Some(9),
            holders: &diff_seq_holders,
            now: start + Duration::from_secs(100),
        },
        |_| Some(9),
    );
    assert!(
        !m3_keeps && m19_ahead_keeps,
        "higher seq must beat lower label: m3={m3_keeps}, m19={m19_ahead_keeps}"
    );

    // Negative control: without a total tie-break on equal `seq`, either both keep (`>=`) or both
    // drop (`>`), leaving 2 or 0 holders instead of 1.
    let no_tiebreak_keep_if_ge = |a_seq: u64, b_seq: u64| (a_seq >= b_seq, b_seq >= a_seq);
    let no_tiebreak_keep_if_gt = |a_seq: u64, b_seq: u64| (a_seq > b_seq, b_seq > a_seq);
    assert_eq!(no_tiebreak_keep_if_ge(7, 7), (true, true));
    assert_eq!(no_tiebreak_keep_if_gt(7, 7), (false, false));
}

/// A `>= 0.146` server yields `alab-secrets` to a `0.145` holder (`txt: None`, no watcher) that
/// answers with a verified ALab envelope at least as new as its own, both before claiming the name
/// and when already holding it as `m22` returns (`docs/DESIGN-fluid-swarm-2026-10-28.md` F7).
#[test]
fn a_new_server_yields_the_name_to_a_0_145_holder_that_answers() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let store_0145 = root.path().join("s0145");
    let store_new = root.path().join("snew");
    std::fs::create_dir_all(&store_0145).unwrap();
    std::fs::create_dir_all(&store_new).unwrap();

    // Mint a verified `alab` envelope at `seq = 4` signed by `seed` and pin `pub_key` on both
    // servers so `Running::probe_alab` runs the real HTTP + signature + pin check.
    let seed = [0x42_u8; 32];
    let pub_key = envelope::signer_public(&seed).expect("signer_public");
    let env_seq4 = envelope::Unsigned {
        name: "alab".to_owned(),
        seq: 4,
        time: 1_800_000_000,
        handoff: Vec::new(),
        parents: Vec::new(),
        lockers: Vec::new(),
    };
    let signed_seq4 = envelope::sign(&env_seq4, &seed).expect("sign seq 4");
    std::fs::write(store_0145.join("alab.vault"), &signed_seq4).unwrap();
    std::fs::write(store_new.join("alab.vault"), &signed_seq4).unwrap();

    let listen_pinned = |store: &Path| {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        crate::server::start(
            listener,
            &ServerOptions {
                listen: "127.0.0.1:0".parse().expect("addr"),
                store: store.to_path_buf(),
                advertise: None,
                pins: vec![("alab".to_owned(), pub_key)],
                hold: Duration::from_millis(100),
                keep_awake: false,
            },
        )
        .expect("start pinned server")
    };
    let srv_0145 = listen_pinned(&store_0145);
    let srv_new = listen_pinned(&store_new);
    let url_0145 = format!("http://{}/v1/alab", srv_0145.addr);
    let holder_0145 = crate::discover::DiscoveredVault {
        url: url_0145.clone(),
        txt: None,
    };

    let start = Instant::now();
    // 1. Standby `srv_new` (`own_seq = Some(4)`) probes `srv_0145` (`seq = 4 >= 4`) on loopback
    //    without touching `.local` mDNS, verifies the envelope, and never takes `alab-secrets`.
    let probe_loopback = |target: &str| {
        (target != crate::alab_advertised())
            .then(|| srv_new.probe_alab(target))
            .flatten()
    };
    let mut watcher = AlabNameWatcher::new();
    for secs in [0_u64, 60, 90, 120] {
        assert!(
            !watcher.decide(
                &AlabNameInput {
                    own_label: "m3",
                    own_seq: Some(4),
                    holders: std::slice::from_ref(&holder_0145),
                    now: start + Duration::from_secs(secs),
                },
                probe_loopback,
            ),
            "must yield to answering 0.145 holder before claiming (at {secs}s)"
        );
    }

    // 2. When `srv_new` was ALREADY holding `alab-secrets` while `0.145` `m22` was away, and `m22`
    //    returns (`holder_0145` appears with `txt: None` and answers at `seq = 4 >= 4`), `srv_new`
    //    immediately withdraws (`decide == false`) because a `0.145` holder never withdraws.
    let mut holding_watcher = AlabNameWatcher::new();
    let _ = holding_watcher.decide(
        &AlabNameInput {
            own_label: "m3",
            own_seq: Some(4),
            holders: &[],
            now: start,
        },
        |_| None,
    );
    assert!(holding_watcher.decide(
        &AlabNameInput {
            own_label: "m3",
            own_seq: Some(4),
            holders: &[],
            now: start + Duration::from_secs(90),
        },
        |_| None,
    ));
    assert!(
        !holding_watcher.decide(
            &AlabNameInput {
                own_label: "m3",
                own_seq: Some(4),
                holders: std::slice::from_ref(&holder_0145),
                now: start + Duration::from_secs(95),
            },
            probe_loopback,
        ),
        "holding >=0.146 server must withdraw when an answering 0.145 holder returns with seq >= own_seq"
    );

    // 3. If the `0.145` server lags (`seq 4 < own_seq 5`), `holding_watcher` keeps `alab-secrets`
    //    rather than yielding a newer head (`seq 5`) to a stale `0.145` copy (`seq 4`).
    let mut ahead_watcher = AlabNameWatcher::new();
    let _ = ahead_watcher.decide(
        &AlabNameInput {
            own_label: "m3",
            own_seq: Some(5),
            holders: &[],
            now: start,
        },
        |_| None,
    );
    assert!(ahead_watcher.decide(
        &AlabNameInput {
            own_label: "m3",
            own_seq: Some(5),
            holders: &[],
            now: start + Duration::from_secs(90),
        },
        |_| None,
    ));
    assert!(
        ahead_watcher.decide(
            &AlabNameInput {
                own_label: "m3",
                own_seq: Some(5),
                holders: std::slice::from_ref(&holder_0145),
                now: start + Duration::from_secs(95),
            },
            probe_loopback,
        ),
        "must NOT yield own_seq=5 to a lagging 0.145 copy at seq=4"
    );

    srv_0145.stop();
    srv_new.stop();
}

#[test]
fn admin_publish_replicates_to_policy_secondary_servers_immediately() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let s1 = start_loopback_server(&root.path().join("s1"));
    let s2 = start_loopback_server(&root.path().join("s2"));
    let s3 = start_loopback_server(&root.path().join("s3"));
    let url1 = format!("http://{}/v1/team", s1.addr);
    let url2 = format!("http://{}/v1/team", s2.addr);
    let url3 = format!("http://{}/v1/team", s3.addr);

    let st1 = Paths::for_home(&root.path().join("st1"));
    create(&st1, &url1, None, Some("m3")).expect("create on s1");

    // Publish seq 2 configuring `url2` in `aterm-swarm-policy.servers`.
    let policy = SwarmPolicy {
        servers: vec![url2.clone()],
        ..SwarmPolicy::default()
    };
    {
        let admin = admin_open(&st1).expect("open seq 1");
        let mut doc = admin.doc.clone();
        let admins = doc.admins();
        doc.set_entry(
            &Audience::Hosts(admins),
            crate::SWARM_POLICY_ENTRY,
            policy.to_toml().as_bytes(),
        )
        .unwrap();
        doc.set_entry(&Audience::Everyone, "replicated-token", b"v2")
            .unwrap();
        let pub2 = admin_publish(&st1, &admin, &doc).expect("publish seq 2");
        assert_eq!(pub2.seq, 2);
    }

    // Secondary `s2` immediately serves seq 2 without needing PutBack or reconcile, while unlisted
    // `s3` (negative control) remains empty!
    match transport::fetch(
        &Source::Url(url2.clone()),
        None,
        aterm_spec::wait::MUST_HAPPEN,
    )
    .expect("fetch s2 after seq 2")
    {
        Fetched::Body(bytes) => assert_eq!(envelope::parse(&bytes).unwrap().seq, 2),
        other => panic!("expected seq 2 body on policy secondary s2, got {other:?}"),
    }
    assert!(
        matches!(
            transport::fetch(&Source::Url(url3.clone()), None, Duration::from_secs(5),)
                .expect("fetch unlisted s3"),
            Fetched::Missing
        ),
        "negative control: unlisted server s3 must stay empty"
    );

    // Subsequent ordinary publish (seq 3) advances `s2` in lockstep too.
    {
        let admin = admin_open(&st1).expect("open seq 2");
        let mut doc = admin.doc.clone();
        doc.set_entry(&Audience::Everyone, "replicated-token", b"v3")
            .unwrap();
        let pub3 = admin_publish(&st1, &admin, &doc).expect("publish seq 3");
        assert_eq!(pub3.seq, 3);
    }
    match transport::fetch(&Source::Url(url2), None, aterm_spec::wait::MUST_HAPPEN)
        .expect("fetch s2 after seq 3")
    {
        Fetched::Body(bytes) => assert_eq!(envelope::parse(&bytes).unwrap().seq, 3),
        other => panic!("expected seq 3 body on policy secondary s2, got {other:?}"),
    }

    s1.stop();
    s2.stop();
    s3.stop();
}

#[test]
fn sync_with_candidates_escapes_frozen_or_offline_primary_and_rejects_forged_higher_seq() {
    // Every server it reads is its own on loopback, which answers or, stopped, refuses at once.
    let _answering = ServersAnswering::set();
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let s_primary = start_loopback_server(&root.path().join("s_primary"));
    let s_fresh = start_loopback_server(&root.path().join("s_fresh"));
    let s_rogue = start_loopback_server(&root.path().join("s_rogue"));
    let url_primary = format!("http://{}/v1/team", s_primary.addr);
    let url_fresh = format!("http://{}/v1/team", s_fresh.addr);
    let url_rogue = format!("http://{}/v1/team", s_rogue.addr);

    // Rogue server hosts a vault with the same name `/v1/team` signed by an attacker's key at a
    // higher sequence number (`seq = 5`).
    let rogue_admin = Paths::for_home(&root.path().join("rogue_admin"));
    create(&rogue_admin, &url_rogue, None, Some("m99")).expect("create rogue vault");
    for step in 2..=5_u8 {
        let a = admin_open(&rogue_admin).unwrap();
        let mut d = a.doc.clone();
        d.set_entry(&Audience::Everyone, "shared-key", &[b'R', b'0' + step])
            .unwrap();
        admin_publish(&rogue_admin, &a, &d).unwrap();
    }

    // Legitimate steward creates `/v1/team` on `s_primary` (seq 1) and admits `m9` + `st_fresh` at seq 2.
    let st1 = Paths::for_home(&root.path().join("st1"));
    create(&st1, &url_primary, None, Some("m3")).expect("create primary vault");
    let st_fresh = Paths::for_home(&root.path().join("st_fresh"));
    let st_fresh_rec = machine_identity(&st_fresh).unwrap().recipient().to_bech32();
    let member = Paths::for_home(&root.path().join("m9"));
    let m9_rec = machine_identity(&member).unwrap().recipient().to_bech32();
    {
        let admin = admin_open(&st1).unwrap();
        let mut doc = admin.doc.clone();
        doc.add_machine("m5", &st_fresh_rec).unwrap();
        doc.add_admin("m5").unwrap();
        doc.add_machine("m9", &m9_rec).unwrap();
        doc.set_entry(&Audience::Everyone, "shared-key", b"v1")
            .unwrap();
        admin_publish(&st1, &admin, &doc).expect("publish seq 2");
    }

    let seq2_bytes = std::fs::read(st1.cache_file()).unwrap();
    transport::publish(&Source::Url(url_fresh.clone()), &seq2_bytes).expect("seed s_fresh");
    let pin = config(&st1).unwrap().unwrap().signer.unwrap();
    let pin_b64 = crate::b64::encode_padded(&pin);
    join_host(&st_fresh, &format!("{url_fresh}#{pin_b64}"), "m5");
    join_host(&member, &format!("{url_primary}#{pin_b64}"), "m9");
    assert_eq!(
        std::fs::read(member.home.join(".secrets/shared-key")).unwrap(),
        b"v1"
    );

    // Advance `s_fresh` to seq 3 while `s_primary` stays frozen at seq 2!
    {
        let a_fresh = admin_open(&st_fresh).unwrap();
        let mut doc = a_fresh.doc.clone();
        doc.set_entry(&Audience::Everyone, "shared-key", b"v2")
            .unwrap();
        let pub3 = admin_publish(&st_fresh, &a_fresh, &doc).expect("publish seq 3 on s_fresh");
        assert_eq!(pub3.seq, 3);
    }

    // Negative control 1: single-server `sync` stays frozen at `seq = 2` (`"v1"`).
    let frozen_out = sync(&member, None).expect("single-server sync against frozen primary");
    assert!(
        matches!(frozen_out, SyncOutcome::Unchanged { seq: 2 }),
        "expected Unchanged seq 2 on frozen primary, got {frozen_out:?}"
    );
    assert_eq!(
        std::fs::read(member.home.join(".secrets/shared-key")).unwrap(),
        b"v1"
    );

    // Positive check 1: `sync_with_candidates` ignores `s_rogue`'s unverified `seq = 5` and
    // advances `member` from `s_fresh` (`seq = 3`), keeping `source.toml` pinned to `url_primary`!
    let escaped_out = sync_with_candidates(&member, None, &[url_rogue.clone(), url_fresh.clone()])
        .expect("candidate sync escapes frozen primary");
    assert!(
        matches!(escaped_out, SyncOutcome::Applied { seq: 3, .. }),
        "expected Applied seq 3 from verified peer, got {escaped_out:?}"
    );
    assert_eq!(
        std::fs::read(member.home.join(".secrets/shared-key")).unwrap(),
        b"v2"
    );
    assert_eq!(
        config(&member).unwrap().unwrap().source,
        Source::Url(url_primary.clone()),
        "failover read must not rewrite source.toml"
    );

    // Stop `s_primary` completely and advance `s_fresh` to `seq = 4` (`"v3"`).
    s_primary.stop();
    {
        let a_fresh = admin_open(&st_fresh).unwrap();
        let mut doc = a_fresh.doc.clone();
        doc.set_entry(&Audience::Everyone, "shared-key", b"v3")
            .unwrap();
        let pub4 = admin_publish(&st_fresh, &a_fresh, &doc).expect("publish seq 4 on s_fresh");
        assert_eq!(pub4.seq, 4);
    }

    // Negative control 2: single-server `sync` errors when `s_primary` is offline, AND rogue-only
    // candidate list ALSO errors instead of accepting `s_rogue`'s `seq = 5`!
    assert!(
        sync(&member, None).is_err(),
        "negative control: single-server sync fails when primary is offline"
    );
    assert!(
        sync_with_candidates(&member, None, std::slice::from_ref(&url_rogue)).is_err(),
        "negative control: rogue higher-seq server must never satisfy failover"
    );

    // Positive check 2: `sync_with_candidates` fails over cleanly to `s_fresh` (`seq = 4`, `"v3"`).
    let offline_failover = sync_with_candidates(&member, None, &[url_rogue, url_fresh])
        .expect("failover to verified peer while primary is offline");
    assert!(
        matches!(offline_failover, SyncOutcome::Applied { seq: 4, .. }),
        "expected Applied seq 4 on offline failover, got {offline_failover:?}"
    );
    assert_eq!(
        std::fs::read(member.home.join(".secrets/shared-key")).unwrap(),
        b"v3"
    );

    s_fresh.stop();
    s_rogue.stop();
}

/// A member `m9` of `team` at seq 2 (`shared-key` = `v1`) whose source, `s_primary`, has gone,
/// and the servers its standing browse may have found: `s_fresh`, where another admin published
/// seq 3 (`v2`), and `s_rogue`, holding a `team` vault an attacker's key signed at seq 5.
struct DeadSource {
    _root: aterm_tempfile::TempDir,
    member: Paths,
    url_primary: String,
    fresh: crate::discover::DiscoveredVault,
    rogue: crate::discover::DiscoveredVault,
    servers: Vec<crate::server::Running>,
}

fn dead_source() -> DeadSource {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let s_primary = start_loopback_server(&root.path().join("s_primary"));
    let s_fresh = start_loopback_server(&root.path().join("s_fresh"));
    let s_rogue = start_loopback_server(&root.path().join("s_rogue"));
    let url_primary = format!("http://{}/v1/team", s_primary.addr);
    let url_fresh = format!("http://{}/v1/team", s_fresh.addr);
    let url_rogue = format!("http://{}/v1/team", s_rogue.addr);

    let rogue_admin = Paths::for_home(&root.path().join("rogue_admin"));
    create(&rogue_admin, &url_rogue, None, Some("m99")).expect("create rogue vault");
    for step in 2..=5_u8 {
        let a = admin_open(&rogue_admin).unwrap();
        let mut d = a.doc.clone();
        d.set_entry(&Audience::Everyone, "shared-key", &[b'R', b'0' + step])
            .unwrap();
        admin_publish(&rogue_admin, &a, &d).unwrap();
    }

    let st1 = Paths::for_home(&root.path().join("st1"));
    create(&st1, &url_primary, None, Some("m3")).expect("create primary vault");
    let st_fresh = Paths::for_home(&root.path().join("st_fresh"));
    let st_fresh_rec = machine_identity(&st_fresh).unwrap().recipient().to_bech32();
    let member = Paths::for_home(&root.path().join("m9"));
    let m9_rec = machine_identity(&member).unwrap().recipient().to_bech32();
    {
        let admin = admin_open(&st1).unwrap();
        let mut doc = admin.doc.clone();
        doc.add_machine("m5", &st_fresh_rec).unwrap();
        doc.add_admin("m5").unwrap();
        doc.add_machine("m9", &m9_rec).unwrap();
        doc.set_entry(&Audience::Everyone, "shared-key", b"v1")
            .unwrap();
        admin_publish(&st1, &admin, &doc).expect("publish seq 2");
    }
    let seq2_bytes = std::fs::read(st1.cache_file()).unwrap();
    transport::publish(&Source::Url(url_fresh.clone()), &seq2_bytes).expect("seed s_fresh");
    let pin = config(&st1).unwrap().unwrap().signer.unwrap();
    let pin_b64 = crate::b64::encode_padded(&pin);
    join_host(&st_fresh, &format!("{url_fresh}#{pin_b64}"), "m5");
    join_host(&member, &format!("{url_primary}#{pin_b64}"), "m9");
    {
        let a_fresh = admin_open(&st_fresh).unwrap();
        let mut doc = a_fresh.doc.clone();
        doc.set_entry(&Audience::Everyone, "shared-key", b"v2")
            .unwrap();
        assert_eq!(admin_publish(&st_fresh, &a_fresh, &doc).unwrap().seq, 3);
    }
    s_primary.stop();
    assert_eq!(
        std::fs::read(member.home.join(".secrets/shared-key")).unwrap(),
        b"v1"
    );
    // The browse's TXT hints are hints: the rogue's names a higher seq than any.
    let hint = |seq| {
        Some(crate::server::VaultTxt {
            vault: "team".to_owned(),
            signer: envelope::fingerprint(&pin),
            seq,
            head: [0xab; 32],
            label: None,
        })
    };
    DeadSource {
        _root: root,
        member,
        url_primary,
        fresh: crate::discover::DiscoveredVault {
            url: url_fresh,
            txt: hint(3),
        },
        rogue: crate::discover::DiscoveredVault {
            url: url_rogue,
            txt: hint(99),
        },
        servers: vec![s_fresh, s_rogue],
    }
}

/// Something that answers at a gone source's address (`url`'s host and port): each request it
/// reads gets `reply` (an HTTP response) after `delay`, or, with none, is dropped after it. It
/// stops when dropped.
struct StandIn {
    addr: std::net::SocketAddr,
    stop: std::sync::Arc<AtomicBool>,
    asked: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl StandIn {
    fn at(url: &str, delay: Duration, reply: Option<Vec<u8>>) -> Self {
        use std::io::{Read as _, Write as _};
        let host = url
            .strip_prefix("http://")
            .and_then(|rest| rest.split('/').next())
            .expect("an http URL");
        let listener = std::net::TcpListener::bind(host).expect("bind the gone source's address");
        let addr = listener.local_addr().expect("addr");
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let asked = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (stopping, counting) = (std::sync::Arc::clone(&stop), std::sync::Arc::clone(&asked));
        let thread = std::thread::spawn(move || {
            for stream in listener.incoming() {
                if stopping.load(Ordering::SeqCst) {
                    return;
                }
                let Ok(mut stream) = stream else { continue };
                let mut request = Vec::new();
                let mut chunk = [0_u8; 1024];
                while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                    match stream.read(&mut chunk) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => request.extend_from_slice(&chunk[..n]),
                    }
                }
                counting.fetch_add(1, Ordering::SeqCst);
                std::thread::sleep(delay);
                if let Some(reply) = &reply {
                    let _ = stream.write_all(reply);
                }
            }
        });
        Self {
            addr,
            stop,
            asked,
            thread: Some(thread),
        }
    }

    /// How many requests it read.
    fn asked(&self) -> usize {
        self.asked.load(Ordering::SeqCst)
    }
}

impl Drop for StandIn {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = std::net::TcpStream::connect(self.addr);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// An HTTP `200` carrying `body`.
fn ok_with(body: &[u8]) -> Vec<u8> {
    let mut reply = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    reply.extend_from_slice(body);
    reply
}

/// The envelope the server at `url` serves.
fn served(url: &str) -> Vec<u8> {
    match transport::fetch(
        &Source::Url(url.to_owned()),
        None,
        aterm_spec::wait::MUST_HAPPEN,
    ) {
        Ok(Fetched::Body(bytes)) => bytes,
        other => panic!("{url} serves no envelope: {other:?}"),
    }
}

/// A source that answers with a body claiming a newer seq that does not verify (a host that
/// took the source's name or address while its server is away) does not stop the failover: the
/// pass still reads the servers its browse found and applies the highest verified seq.
/// NEGATIVE CONTROL: a pass that takes the source's seq unverified (`envelope::parse` in place
/// of `verify_for_reader`) as news probes no browsed server and errs on the forged signature,
/// leaving seq 2.
#[test]
fn a_forged_newer_seq_at_the_source_does_not_stop_the_failover() {
    // Every server it reads is its own on loopback, which answers; so does the stand-in at its
    // gone source's address.
    let _answering = ServersAnswering::set();
    let at = dead_source();
    let fresh = served(&at.fresh.url);
    let (from, to) = (&b"\nseq: 3\n"[..], &b"\nseq: 999999\n"[..]);
    let at_seq = fresh
        .windows(from.len())
        .position(|w| w == from)
        .expect("the envelope names its seq");
    let forged = [&fresh[..at_seq], to, &fresh[at_seq + from.len()..]].concat();
    assert!(envelope::parse(&forged).is_ok_and(|parsed| parsed.seq == 999_999));
    let source = StandIn::at(&at.url_primary, Duration::ZERO, Some(ok_with(&forged)));
    let browsed = [at.fresh.clone()];
    let found = |_: &Config| browsed.to_vec();
    let (_, outcome) =
        subscriber_pass(&at.member, &mut None, &found).expect("fails over past the forger");
    assert!(
        matches!(outcome, SyncOutcome::Applied { seq: 3, .. }),
        "{outcome:?}"
    );
    assert_eq!(
        std::fs::read(at.member.home.join(".secrets/shared-key")).unwrap(),
        b"v2"
    );
    assert!(source.asked() >= 1, "the pass asked the source first");
    drop(source);
    for server in at.servers {
        server.stop();
    }
}

/// A pass whose source did not answer is no long-poll the source held, however long the source
/// took to fail (an unanswered `.local` lookup gives up after seconds): once this Mac holds what
/// the browsed servers hold, such a pass pauses and is paced as a failing one, not followed by
/// the next at once, so a subscriber does not ask a gone source and every browsed server in a
/// loop for as long as the source is away.
/// NEGATIVE CONTROL: taking a source that failed after [`HELD`] or more as one that held the
/// long-poll (the old reading of a pass's whole time) gives a zero pause.
#[test]
fn a_failover_pass_with_nothing_new_is_not_followed_at_once() {
    // Every server it reads is its own on loopback, which answers; its gone source refuses at
    // once, or its stand-in drops each request.
    let _answering = ServersAnswering::set();
    let at = dead_source();
    let browsed = [at.fresh.clone()];
    let found = |_: &Config| browsed.to_vec();
    let (_, caught_up) = subscriber_pass(&at.member, &mut None, &found).expect("fails over");
    assert!(
        matches!(caught_up, SyncOutcome::Applied { seq: 3, .. }),
        "{caught_up:?}"
    );
    // The source's address takes each request and drops it after more than a second.
    let source = StandIn::at(&at.url_primary, HELD + Duration::from_millis(200), None);
    let said = std::cell::Cell::new(SourceSaid::AtOnce);
    let (config, outcome) = subscriber_pass_saying(&at.member, &mut None, &found, &said)
        .expect("the browsed server serves");
    assert_eq!(outcome, SyncOutcome::Unchanged { seq: 3 });
    assert!(source.asked() >= 1, "the pass asked its source first");
    let pass = Pass {
        source: said.get(),
        moved: false,
    };
    let mut failing = FIRST_RETRY;
    assert_ne!(
        pause_after(&config.source, &outcome, &mut failing, pass),
        Duration::ZERO,
        "not followed at once"
    );
    assert_eq!(pass.source, SourceSaid::Nothing);
    assert!(failed_over_idle(pass), "paced as a failing pass");
    drop(source);
    for server in at.servers {
        server.stop();
    }
}

/// A member's subscriber pass whose source is gone reads the vault servers its standing browse
/// found and applies the highest verified seq among them. `source.toml` still names the gone
/// source, its first choice.
/// NEGATIVE CONTROL: a pass that reads `config.source` only (`sync` in place of `sync_found`)
/// errs here.
#[test]
fn a_dead_source_fails_over_to_a_browsed_server_with_a_newer_seq() {
    // Every server it reads is its own on loopback, which answers; its gone source refuses at
    // once.
    let _answering = ServersAnswering::set();
    let at = dead_source();
    // CONTROL: with nothing browsed, the pass has only its gone source.
    assert!(subscriber_pass(&at.member, &mut None, &found_none).is_err());
    let browsed = [at.fresh.clone()];
    let found = |_: &Config| browsed.to_vec();
    let (config, outcome) =
        subscriber_pass(&at.member, &mut None, &found).expect("fails over to the browsed server");
    assert!(
        matches!(outcome, SyncOutcome::Applied { seq: 3, .. }),
        "{outcome:?}"
    );
    assert_eq!(
        std::fs::read(at.member.home.join(".secrets/shared-key")).unwrap(),
        b"v2"
    );
    assert_eq!(
        config.source,
        Source::Url(at.url_primary.clone()),
        "failover reads, and never rewrites source.toml"
    );
    for server in at.servers {
        server.stop();
    }
}

/// A browsed server whose envelope another key signed is never taken, whatever seq it and its
/// TXT name: the pass applies the highest seq that verifies under this Mac's pin, and with the
/// forger alone it errs and leaves this Mac as it was.
/// NEGATIVE CONTROL: a pass that takes a browsed envelope unverified (`probe_sync_peer` parsing
/// in place of `verify_for_reader`) picks the forger's seq 5 and errs.
#[test]
fn a_forged_higher_seq_on_a_browsed_server_is_ignored() {
    // Every server it reads is its own on loopback, which answers; its gone source refuses at
    // once.
    let _answering = ServersAnswering::set();
    let at = dead_source();
    let alone = [at.rogue.clone()];
    let forger_alone = |_: &Config| alone.to_vec();
    assert!(
        subscriber_pass(&at.member, &mut None, &forger_alone).is_err(),
        "the forger alone is no vault"
    );
    assert_eq!(load_state(&at.member).unwrap().seq, 2);
    let both = [at.rogue.clone(), at.fresh.clone()];
    let found = |_: &Config| both.to_vec();
    let (_, outcome) =
        subscriber_pass(&at.member, &mut None, &found).expect("the verified server serves");
    assert!(
        matches!(outcome, SyncOutcome::Applied { seq: 3, .. }),
        "{outcome:?}"
    );
    assert_eq!(
        std::fs::read(at.member.home.join(".secrets/shared-key")).unwrap(),
        b"v2"
    );
    for server in at.servers {
        server.stop();
    }
}

#[test]
fn a_merge_keeps_a_policy_only_another_branch_set_and_its_own_over_another() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let m3 = Paths::for_home(&root.path().join("m3"));
    let bare = Document::new_vault(
        "m3",
        &machine_identity(&m3).unwrap().recipient().to_bech32(),
    );
    let with = |url: &str| {
        let mut doc = bare.clone();
        let policy = SwarmPolicy {
            servers: vec![url.to_owned()],
            ..SwarmPolicy::default()
        };
        let admins = doc.admins();
        doc.set_entry(
            &Audience::Hosts(admins),
            POLICY_ENTRY,
            policy.to_toml().as_bytes(),
        )
        .unwrap();
        doc
    };
    let servers = |doc: &Document| SwarmPolicy::from_document(doc).unwrap().map(|p| p.servers);
    let island = with("http://127.0.0.1:1/v1/team");

    let mut merged = bare.clone();
    keep_policy(&mut merged, [&bare, &island]).unwrap();
    assert_eq!(servers(&merged), servers(&island));

    let mut own = with("http://127.0.0.1:2/v1/team");
    keep_policy(&mut own, [&island]).unwrap();
    assert_eq!(
        servers(&own),
        Some(vec!["http://127.0.0.1:2/v1/team".to_owned()])
    );
}

#[test]
fn sync_auto_reconciles_diverged_policy_servers_for_admin_and_keeps_member_steady() {
    // Every server it reads is its own on loopback, which answers.
    let _answering = ServersAnswering::set();
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let s1 = start_loopback_server(&root.path().join("s1"));
    let s2 = start_loopback_server(&root.path().join("s2"));
    let url1 = format!("http://{}/v1/team", s1.addr);
    let url2 = format!("http://{}/v1/team", s2.addr);

    let st1 = Paths::for_home(&root.path().join("st1"));
    create(&st1, &url1, None, Some("m3")).expect("create vault on s1");

    let st2 = Paths::for_home(&root.path().join("st2"));
    let st2_rec = machine_identity(&st2).unwrap().recipient().to_bech32();
    let m_doomed = Paths::for_home(&root.path().join("m9"));
    let m9_rec = machine_identity(&m_doomed).unwrap().recipient().to_bech32();
    let member = Paths::for_home(&root.path().join("m10"));
    let m10_rec = machine_identity(&member).unwrap().recipient().to_bech32();

    // Publish seq 2 WITHOUT policy.servers yet so we can simulate an isolated partition first,
    // then add policy.servers on both branches or test `sync_with_candidates` + `sync`!
    {
        let admin = admin_open(&st1).unwrap();
        let mut doc = admin.doc.clone();
        doc.add_machine("m5", &st2_rec).unwrap();
        doc.add_admin("m5").unwrap();
        doc.add_machine("m9", &m9_rec).unwrap();
        doc.add_machine("m10", &m10_rec).unwrap();
        doc.set_entry(&Audience::Everyone, "note", b"base").unwrap();
        admin_publish(&st1, &admin, &doc).expect("publish seq 2");
    }
    let seq2_bytes = std::fs::read(st1.cache_file()).unwrap();
    transport::publish(&Source::Url(url2.clone()), &seq2_bytes).expect("seed s2 at seq 2");

    let pin = config(&st1).unwrap().unwrap().signer.unwrap();
    let pin_b64 = crate::b64::encode_padded(&pin);
    join_host(&st2, &format!("{url2}#{pin_b64}"), "m5");
    join_host(&member, &format!("{url1}#{pin_b64}"), "m10");

    let m11 = Paths::for_home(&root.path().join("m11"));
    let m11_rec = machine_identity(&m11).unwrap().recipient().to_bech32();
    let m12 = Paths::for_home(&root.path().join("m12"));
    let m12_rec = machine_identity(&m12).unwrap().recipient().to_bech32();

    // Partition (under F1 parentless envelopes, both islands advance to seq 4 with distinct
    // digests so s1 refuses s2's best-effort replication with 409 Conflict):
    // - On s1 (st1): revoke m9 at seq 3, then admit m11 at seq 4!
    let a1 = admin_open(&st1).unwrap();
    let rev = admin_revoke(&st1, &a1, "m9", true).expect("revoke m9 on s1");
    assert_eq!(rev.published.seq, 3);
    {
        let a1 = admin_open(&st1).unwrap();
        let mut d1 = a1.doc.clone();
        d1.add_machine("m11", &m11_rec).unwrap();
        admin_publish(&st1, &a1, &d1).expect("st1 seq 4");
    }

    // - On s2 (st2): advance TWICE (seq 3, then seq 4 colliding with s1's seq 4!) admitting m12
    //   and setting `SwarmPolicy { servers: [url1, url2] }`!
    {
        let a2 = admin_open(&st2).unwrap();
        let mut d2 = a2.doc.clone();
        d2.add_machine("m12", &m12_rec).unwrap();
        admin_publish(&st2, &a2, &d2).expect("st2 seq 3");
    }
    {
        let a2 = admin_open(&st2).unwrap();
        let mut d2 = a2.doc.clone();
        let admins = d2.admins();
        let policy = SwarmPolicy {
            servers: vec![url1.clone(), url2.clone()],
            ..SwarmPolicy::default()
        };
        // Setting policy on seq 4 tries best-effort replication to url1, which s1 rejects with
        // 409 (`NotStored`) because s1 already holds a different seq 4 head!
        d2.set_entry(
            &Audience::Hosts(admins),
            crate::SWARM_POLICY_ENTRY,
            policy.to_toml().as_bytes(),
        )
        .unwrap();
        admin_publish(&st2, &a2, &d2).expect("st2 seq 4");
    }

    // Verify the equal-seq split stands (`s1` and `s2` both at seq 4 with distinct digests)!
    let s1_env = match transport::fetch(
        &Source::Url(url1.clone()),
        None,
        aterm_spec::wait::MUST_HAPPEN,
    )
    .unwrap()
    {
        Fetched::Body(b) => envelope::parse(&b).unwrap(),
        other => panic!("expected s1 at seq 4, got {other:?}"),
    };
    let (s2_bytes, s2_env) = match transport::fetch(
        &Source::Url(url2.clone()),
        None,
        aterm_spec::wait::MUST_HAPPEN,
    )
    .unwrap()
    {
        Fetched::Body(b) => {
            let parsed = envelope::parse(&b).unwrap();
            (b, parsed)
        }
        other => panic!("expected s2 at seq 4, got {other:?}"),
    };
    assert_eq!(s1_env.seq, 4);
    assert_eq!(s2_env.seq, 4);
    assert_ne!(s1_env.digest(), s2_env.digest());

    // 1. Non-admin `member` (pinned to `s1`, initially at seq 2) first applies `s2`'s seq 4 (as
    //    when partitioned onto island 2), then encounters the equal-seq split between `s1` and `s2`:
    //    - Negative control: single-server `sync(&member, None)` (no candidate peers) sees `s1`'s
    //      different envelope at seq 4 and refuses the forked vault!
    //    - Positive check: `sync_with_candidates(&member, None, &[url1, url2])` verifies peer `s2`
    //      still serves `member`'s applied seq 4 head and stays steady (`Unchanged { seq: 4 }`)!
    let p1 = super::super::sync_with(&member, s2_bytes).unwrap();
    assert!(matches!(p1, SyncOutcome::Applied { seq: 4, .. }), "{p1:?}");
    let single_err = sync(&member, None)
        .expect_err("negative control: single-server sync refuses equal-seq fork");
    assert!(
        single_err.to_string().contains("forked vault"),
        "expected forked vault refusal, got {single_err}"
    );
    let p2 = sync_with_candidates(&member, None, &[url1.clone(), url2.clone()])
        .expect("non-admin member stays steady on verified peer head during equal-seq split");
    assert!(
        matches!(p2, SyncOutcome::Unchanged { seq: 4 }),
        "expected Unchanged seq 4 while split awaits steward merge, got {p2:?}"
    );

    // 2. Admin `st2`'s plain `sync(&st2, None)` (with NO extra_urls — reading `policy.servers`
    //    from the admin locker automatically!) reconciles the split into `seq = 5`!
    let healed = sync(&st2, None).expect("admin sync auto-reconciles diverged policy servers");
    assert!(
        matches!(healed, SyncOutcome::Applied { seq: 5, .. }),
        "expected admin sync to auto-reconcile and apply merged seq 5, got {healed:?}"
    );

    // Both `s1` and `s2` now serve identical `seq = 5` preserving m9's revocation (island 1),
    // m11's admission (island 1), AND m12's admission (island 2) regardless of digest tie-break!
    let heads = read_server_heads(&st2, &[]).expect("read heads after auto-reconcile");
    assert_eq!(heads.len(), 2);
    assert_eq!(heads[0].envelope.seq, 5);
    assert_eq!(heads[0].envelope.digest(), heads[1].envelope.digest());
    assert!(heads[0].doc.code_only().contains(&m9_rec));
    assert!(
        heads[0]
            .doc
            .machines()
            .iter()
            .any(|m| m.name == "m11" && m.recipient == m11_rec)
    );
    assert!(
        heads[0]
            .doc
            .machines()
            .iter()
            .any(|m| m.name == "m12" && m.recipient == m12_rec)
    );

    // And `member`'s plain `sync(&member, None)` advances cleanly to `seq = 5`!
    let member_healed = sync(&member, None).expect("member syncs merged seq 5");
    assert!(
        matches!(
            member_healed,
            SyncOutcome::Applied { seq: 5, .. } | SyncOutcome::Unchanged { seq: 5 }
        ),
        "{member_healed:?}"
    );
    assert_eq!(
        std::fs::read(member.home.join(".secrets/note")).unwrap(),
        b"base"
    );

    s1.stop();
    s2.stop();
}

/// Exact envelope structure parser frozen from `v0.145.0:crates/aterm-secrets/src/envelope.rs`.
/// After `signer:` and optional `previous-signer:`/`endorsement:` pairs, `v0.145.0` immediately
/// parses `locker: ` blocks and `signature: `; any `parent: ` line fails on line 6 with the
/// expected locker or signature error.
fn parse_v0_145_envelope_grammar(bytes: &[u8]) -> Result<(String, u64, usize), String> {
    if bytes.len() > envelope::MAX_ENVELOPE {
        return Err(format!(
            "envelope: larger than {} bytes",
            envelope::MAX_ENVELOPE
        ));
    }
    if bytes.contains(&b'\r') {
        return Err("envelope: contains a CR; lines end in LF alone".to_owned());
    }
    let text = std::str::from_utf8(bytes).map_err(|_| "envelope: not UTF-8 text".to_owned())?;
    if !text.ends_with('\n') {
        return Err("envelope: does not end with LF".to_owned());
    }
    let body = &text[..text.len() - 1];
    let lines_vec: Vec<&str> = body.split('\n').collect();
    let mut idx = 0usize;
    fn take_line<'a>(lines: &[&'a str], idx: &mut usize) -> Option<(usize, &'a str)> {
        let l = *lines.get(*idx)?;
        *idx += 1;
        Some((*idx, l))
    }
    fn peek_at<'a>(lines: &[&'a str], at: usize) -> Option<(usize, &'a str)> {
        lines.get(at).map(|l| (at + 1, *l))
    }
    fn take_header<'a>(lines: &[&'a str], idx: &mut usize, key: &str) -> Result<&'a str, String> {
        let (number, line) =
            take_line(lines, idx).ok_or_else(|| format!("envelope: ends before `{key}:`"))?;
        line.strip_prefix(key)
            .and_then(|rest| rest.strip_prefix(": "))
            .ok_or_else(|| format!("envelope line {number}: expected `{key}:`"))
    }
    if take_line(&lines_vec, &mut idx).map(|(_, l)| l) != Some(envelope::MAGIC) {
        return Err(format!(
            "envelope: the first line is not `{}`",
            envelope::MAGIC
        ));
    }
    let name = take_header(&lines_vec, &mut idx, "name")?;
    if !envelope::valid_name(name) {
        return Err("envelope: `name` is not a vault name ([a-z0-9][a-z0-9-]{0,62})".to_owned());
    }
    let seq: u64 = take_header(&lines_vec, &mut idx, "seq")?
        .parse()
        .map_err(|_| "envelope: bad seq".to_owned())?;
    let _time: u64 = take_header(&lines_vec, &mut idx, "time")?
        .parse()
        .map_err(|_| "envelope: bad time".to_owned())?;
    let signer = crate::b64::decode_padded(take_header(&lines_vec, &mut idx, "signer")?)
        .and_then(|b| <[u8; 32]>::try_from(b.as_slice()).ok())
        .ok_or_else(|| "envelope: bad signer".to_owned())?;
    loop {
        match peek_at(&lines_vec, idx) {
            Some((_, line)) if line.starts_with("previous-signer:") => {
                let _ = take_header(&lines_vec, &mut idx, "previous-signer")?;
                let _ = take_header(&lines_vec, &mut idx, "endorsement")?;
            }
            Some((number, line)) if line.starts_with("endorsement:") => {
                return Err(format!(
                    "envelope line {number}: `endorsement:` without `previous-signer:`"
                ));
            }
            _ => break,
        }
    }
    let mut locker_count = 0usize;
    while let Some((number, line)) = peek_at(&lines_vec, idx) {
        let Some(label) = line.strip_prefix("locker: ") else {
            break;
        };
        let _ = take_line(&lines_vec, &mut idx);
        if !envelope::valid_label(label) {
            return Err(format!("envelope line {number}: invalid locker label"));
        }
        let Some((_, begin)) = take_line(&lines_vec, &mut idx) else {
            return Err("envelope: missing BEGIN".to_owned());
        };
        if begin != "-----BEGIN AGE ENCRYPTED FILE-----" {
            return Err(format!(
                "envelope line {}: expected the armor's BEGIN line",
                number + 1
            ));
        }
        let mut arm_lines = vec![begin];
        let mut closed = false;
        while let Some((_, arm)) = take_line(&lines_vec, &mut idx) {
            arm_lines.push(arm);
            if arm == "-----END AGE ENCRYPTED FILE-----" {
                closed = true;
                break;
            }
        }
        if !closed {
            return Err(format!(
                "envelope: the armor of the locker on line {number} has no END line"
            ));
        }
        let mut armored = arm_lines.join("\n");
        armored.push('\n');
        envelope::check_armor(&armored).map_err(|e| e.to_string())?;
        locker_count += 1;
    }
    let (number, sig_line) = take_line(&lines_vec, &mut idx)
        .ok_or_else(|| "envelope: no `signature:` line".to_owned())?;
    let Some(sig_b64) = sig_line.strip_prefix("signature: ") else {
        return Err(format!(
            "envelope line {number}: expected `locker:` or `signature:`"
        ));
    };
    let sig = crate::b64::decode_padded(sig_b64)
        .and_then(|b| <[u8; 64]>::try_from(b.as_slice()).ok())
        .ok_or_else(|| "envelope: bad signature".to_owned())?;
    if peek_at(&lines_vec, idx).is_some() {
        return Err("envelope: lines follow `signature:`".to_owned());
    }
    let signed_prefix_len = text.len() - sig_line.len() - 1;
    ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, &signer)
        .verify(&bytes[..signed_prefix_len], &sig)
        .map_err(|_| "envelope: signature verification failed".to_owned())?;
    Ok((name.to_owned(), seq, locker_count))
}

#[test]
fn a_c7_build_seals_no_parent_line() {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let srv_store = dir.path().join("srv");
    std::fs::create_dir_all(&srv_store).unwrap();
    let srv = start_loopback_server(&srv_store);
    let url = format!("http://{}/v1/alab", srv.addr);
    let st = Paths::for_home(&dir.path().join("st"));

    create(&st, &url, None, Some("m22")).expect("init seq 1");
    let seq1_bytes = std::fs::read(st.cache_file()).expect("seq 1 bytes");
    let seq1_text = std::str::from_utf8(&seq1_bytes).expect("utf8");
    assert!(
        !seq1_text.lines().any(|l| l.starts_with("parent:")),
        "seq 1 must carry no parent: line"
    );
    assert!(envelope::parse(&seq1_bytes).unwrap().parents.is_empty());

    // Seq 2 via `admin_publish` (after seq 1 already sits in cache) and seq 3 via direct
    // `seal_with_admissions_and_parents` given explicit non-empty `parents`: neither may emit a
    // `parent:` line under F1 (`DESIGN-fluid-swarm` §4.1).
    let opened = admin_open(&st).expect("admin open");
    let mut doc = opened.doc.clone();
    doc.set_entry(&Audience::Everyone, "note", b"v2").unwrap();
    admin_publish(&st, &opened, &doc).expect("publish seq 2");

    let seq2_bytes = std::fs::read(st.cache_file()).expect("seq 2 bytes");
    let seq2_text = std::str::from_utf8(&seq2_bytes).expect("utf8");
    assert!(
        !seq2_text.lines().any(|l| l.starts_with("parent:")),
        "seq 2 published by C7 admin_publish must carry no parent: line:\n{seq2_text}"
    );
    let seq2_env = envelope::parse(&seq2_bytes).unwrap();
    assert_eq!(seq2_env.seq, 2);
    assert!(seq2_env.parents.is_empty());

    let sealed_explicit = super::seal_with_admissions_and_parents(
        "alab",
        3,
        &opened.seed,
        &opened.commons_seed,
        opened.commons_bound,
        &doc,
        &opened.handoff,
        &[seq2_env.digest()],
        &opened.admissions,
    )
    .expect("seal seq 3");
    let seq3_text = std::str::from_utf8(&sealed_explicit).expect("utf8");
    assert!(
        !seq3_text.lines().any(|l| l.starts_with("parent:")),
        "seal_with_admissions_and_parents must never write parent: lines"
    );
    assert!(
        envelope::parse(&sealed_explicit)
            .unwrap()
            .parents
            .is_empty()
    );

    srv.stop();
}

#[test]
fn the_header_a_c7_build_writes_parses_with_the_v0_145_grammar() {
    // Frozen v0.145.0 header fixture check first.
    let seed = [0x42u8; 32];
    let frozen_unsigned = envelope::Unsigned {
        name: "alab".to_owned(),
        seq: 1,
        time: 1_792_530_001,
        handoff: Vec::new(),
        parents: Vec::new(),
        lockers: vec![envelope::Locker {
            label: "admin".to_owned(),
            armored: envelope::test_armor(b"frozen-v0-145-admin-locker"),
        }],
    };
    let frozen_bytes = envelope::sign(&frozen_unsigned, &seed).expect("sign frozen fixture");
    assert_eq!(
        parse_v0_145_envelope_grammar(&frozen_bytes).expect("frozen v0.145 fixture parses"),
        ("alab".to_owned(), 1, 1)
    );

    // Live C7 build writes seq 1 and seq 2 envelopes; both must parse and signature-verify under
    // the exact v0.145.0 grammar.
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let srv_store = dir.path().join("srv");
    std::fs::create_dir_all(&srv_store).unwrap();
    let srv = start_loopback_server(&srv_store);
    let url = format!("http://{}/v1/alab", srv.addr);
    let st = Paths::for_home(&dir.path().join("st"));

    create(&st, &url, None, Some("m22")).expect("init seq 1");
    let seq1_bytes = std::fs::read(st.cache_file()).expect("seq 1 bytes");
    let (name1, seq1, lockers1) =
        parse_v0_145_envelope_grammar(&seq1_bytes).expect("seq 1 parses under v0.145 grammar");
    assert_eq!((name1.as_str(), seq1), ("alab", 1));
    assert!(lockers1 >= 2);

    let opened = admin_open(&st).expect("admin open");
    let mut doc = opened.doc.clone();
    doc.set_entry(&Audience::Everyone, "shared", b"for-0145-readers")
        .unwrap();
    admin_publish(&st, &opened, &doc).expect("publish seq 2");
    let seq2_bytes = std::fs::read(st.cache_file()).expect("seq 2 bytes");
    let (name2, seq2, lockers2) =
        parse_v0_145_envelope_grammar(&seq2_bytes).expect("seq 2 parses under v0.145 grammar");
    assert_eq!((name2.as_str(), seq2), ("alab", 2));
    assert!(lockers2 >= 2);

    // Negative control (`DESIGN-fluid-swarm` §4.1): write a `parent:` line after seq 1; the
    // v0.145 grammar parse fails with the expected locker or signature error while C7's reader
    // parser (`envelope::parse`) still reads it.
    let seq1_digest = envelope::parse(&seq1_bytes).unwrap().digest();
    let mut with_parent = envelope::Unsigned {
        name: "alab".to_owned(),
        seq: 2,
        time: 1_792_530_002,
        handoff: Vec::new(),
        parents: vec![seq1_digest],
        lockers: vec![envelope::Locker {
            label: "admin".to_owned(),
            armored: envelope::test_armor(b"parented-seq-2"),
        }],
    };
    let parented_bytes = envelope::sign(&with_parent, &seed).expect("sign parented seq 2");
    let parsed_c7 =
        envelope::parse(&parented_bytes).expect("C7 envelope::parse keeps reading parent:");
    assert_eq!(parsed_c7.parents, vec![seq1_digest]);
    let v0_145_err = parse_v0_145_envelope_grammar(&parented_bytes)
        .expect_err("negative control: v0.145 grammar must reject a `parent:` header line");
    assert!(
        v0_145_err.contains("expected `locker:` or `signature:`"),
        "unexpected v0.145 parse error: {v0_145_err}"
    );
    with_parent.parents.clear();
    let cleared_bytes = envelope::sign(&with_parent, &seed).expect("sign cleared seq 2");
    assert!(parse_v0_145_envelope_grammar(&cleared_bytes).is_ok());

    srv.stop();
}

#[test]
fn with_policy_published_and_no_lease_some_server_advertises_alab_secrets() {
    let alab_name = crate::alab_advertised().to_owned();
    let key = [0x5au8; 32];

    // Under F6 + F7 (`DESIGN-fluid-swarm-2026-10-28.md` §4.1), every active ALab member (`m22`
    // steward and `m9` non-steward alike) pins `ALAB_VAULT`, advertises its own label on the
    // primary mDNS slot, and uses lease-free `AlabNameWatcher` on the secondary slot to take
    // `alab-secrets` once nothing answers there for 90 s.
    let mut steward_pins = Vec::new();
    let (steward_adv, steward_held) = crate::cli::alab_policy_role(
        Some(alab_name.clone()),
        None,
        Some((true, Some("m22".to_owned()))),
        &mut steward_pins,
        Some(key),
    );
    let mut member_pins = Vec::new();
    let (member_adv, member_held) = crate::cli::alab_policy_role(
        None,
        Some(alab_name.clone()),
        Some((true, Some("m9".to_owned()))),
        &mut member_pins,
        Some(key),
    );
    assert_eq!(steward_adv.as_deref(), Some("m22"));
    assert!(steward_held.is_none());
    assert_eq!(steward_pins, vec![(crate::ALAB_VAULT.to_owned(), key)]);
    assert_eq!(member_adv.as_deref(), Some("m9"));
    assert!(member_held.is_none());
    assert_eq!(member_pins, vec![(crate::ALAB_VAULT.to_owned(), key)]);

    let start = Instant::now();
    let mut watcher = AlabNameWatcher::new();
    let _ = watcher.decide(
        &AlabNameInput {
            own_label: member_adv.as_deref().unwrap(),
            own_seq: Some(1),
            holders: &[],
            now: start,
        },
        |_| None,
    );
    let secondary_alab = watcher
        .decide(
            &AlabNameInput {
                own_label: member_adv.as_deref().unwrap(),
                own_seq: Some(1),
                holders: &[],
                now: start + Duration::from_secs(90),
            },
            |_| None,
        )
        .then_some(alab_name.as_str());
    assert_eq!(
        secondary_alab,
        Some(alab_name.as_str()),
        "with policy published and no lease, lease-free AlabNameWatcher advertises alab-secrets"
    );

    // Negative control: moving primary to `<own_label>` while keeping the retired lease-required
    // watcher leaves nobody advertising `alab-secrets` when no lease is published.
    let lease_required_secondary: Option<&str> = None;
    let advertised_without_f7: Vec<&str> = [
        steward_adv.as_deref(),
        member_adv.as_deref(),
        lease_required_secondary,
    ]
    .into_iter()
    .flatten()
    .collect();
    assert!(
        !advertised_without_f7.contains(&alab_name.as_str()),
        "negative control: without F7 lease-free watcher, nobody advertises alab-secrets (got {advertised_without_f7:?})"
    );
}

/// Every member server pulls newer heads from `_alab-vault._tcp` peers (`?after=<held seq>` with
/// `X-Aterm-No-Wait: 1`), and an empty server takes `alab` from a peer ONLY when `alab` is pinned
/// (`docs/DESIGN-fluid-swarm-2026-10-28.md` F6).
/// NEGATIVE CONTROL: without peer pull, a lagging server stays on its old `seq`; without the ALab
/// pin, an empty server refuses to adopt an unheld `alab` vault from a peer.
#[test]
fn a_server_pulls_a_newer_head_from_a_browsed_peer() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let store1 = root.path().join("s1");
    let store2 = root.path().join("s2");
    std::fs::create_dir_all(&store1).unwrap();
    std::fs::create_dir_all(&store2).unwrap();

    let s1 = start_loopback_server(&store1);
    let s2 = start_loopback_server(&store2);
    let url1 = format!("http://{}/v1/alab", s1.addr);
    let url2 = format!("http://{}/v1/alab", s2.addr);

    let admin_paths = Paths::for_home(&root.path().join("m3"));
    create(&admin_paths, &url1, None, Some("m3")).expect("create alab on s1");
    let pin = config(&admin_paths)
        .unwrap()
        .unwrap()
        .signer
        .expect("pinned signer");

    // Seed `s2` with `seq 1`, then advance `s2` to `seq 2` and `seq 3` while `s1` is not listed.
    let seq1_bytes = match transport::fetch(
        &Source::Url(url1.clone()),
        None,
        aterm_spec::wait::MUST_HAPPEN,
    )
    .unwrap()
    {
        Fetched::Body(b) => b,
        other => panic!("expected seq 1 on s1, got {other:?}"),
    };
    transport::publish(&Source::Url(url2.clone()), &seq1_bytes).expect("seed s2 seq 1");

    let admin_s2 = Paths::for_home(&root.path().join("m5"));
    // Share `m3` identity onto `s2` URL so `admin_s2` can publish `seq 2` and `seq 3` onto `s2`
    // alone without touching `s1`.
    std::fs::create_dir_all(&admin_s2.dir).unwrap();
    std::fs::copy(
        admin_paths.dir.join("identity"),
        admin_s2.dir.join("identity"),
    )
    .unwrap();
    let pin_b64 = crate::b64::encode_padded(&pin);
    join_host(&admin_s2, &format!("{url2}#{pin_b64}"), "m3");
    for val in [b"v1".as_slice(), b"v2".as_slice()] {
        let opened = admin_open(&admin_s2).unwrap();
        let mut doc = opened.doc.clone();
        doc.set_entry(&Audience::Everyone, "shared-key", val)
            .unwrap();
        admin_publish(&admin_s2, &opened, &doc).unwrap();
    }
    assert_eq!(s2.held_seq("alab"), Some(3));

    // Negative control ("No pull"): before `pull_from_peers`, `s1` still holds `seq 1`.
    assert_eq!(s1.held_seq("alab"), Some(1));

    let browsed = [crate::discover::DiscoveredVault {
        url: url2.clone(),
        txt: Some(crate::server::VaultTxt {
            vault: "alab".to_owned(),
            signer: envelope::fingerprint(&pin),
            seq: 3,
            head: [0x22; 32],
            label: Some("m5".to_owned()),
        }),
    }];
    assert_eq!(s1.pull_from_peers(&browsed), 1);
    assert_eq!(s1.held_seq("alab"), Some(3));

    // Sync `admin_paths` (configured ONLY against `url1`) and verify `v2` arrived through `s1`.
    sync(&admin_paths, None).expect("sync from caught-up s1");
    assert_eq!(
        std::fs::read(admin_paths.home.join(".secrets/shared-key")).unwrap(),
        b"v2"
    );

    // Empty-server rule: an empty server WITHOUT the `alab` pin takes NOTHING from `browsed`,
    // whereas an empty server WITH `--pin alab=<pin>` pulls and stores `seq 3`.
    let empty_unpinned_dir = root.path().join("empty-unpinned");
    let empty_pinned_dir = root.path().join("empty-pinned");
    std::fs::create_dir_all(&empty_unpinned_dir).unwrap();
    std::fs::create_dir_all(&empty_pinned_dir).unwrap();
    let s_empty_unpinned = start_loopback_server(&empty_unpinned_dir);
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let s_empty_pinned = crate::server::start(
        listener,
        &ServerOptions {
            listen: "127.0.0.1:0".parse().expect("addr"),
            store: empty_pinned_dir,
            advertise: None,
            pins: vec![("alab".to_owned(), pin)],
            hold: Duration::from_millis(100),
            keep_awake: false,
        },
    )
    .expect("start empty pinned server");

    assert_eq!(s_empty_unpinned.pull_from_peers(&browsed), 0);
    assert_eq!(s_empty_unpinned.held_seq("alab"), None);
    assert_eq!(s_empty_pinned.pull_from_peers(&browsed), 1);
    assert_eq!(s_empty_pinned.held_seq("alab"), Some(3));

    s1.stop();
    s2.stop();
    s_empty_unpinned.stop();
    s_empty_pinned.stop();
}

/// `publish_as` PUTs every new head to all candidate servers (`config.source`, policy `servers`,
/// and browsed `_alab-vault._tcp` peers) through `publish_to_servers`, and succeeds via reachable
/// candidate servers even when `config.source` itself is offline (`docs/DESIGN-fluid-swarm-2026-10-28.md` F6).
/// NEGATIVE CONTROL: publishing to `config.source` alone leaves peer servers stale and fails when
/// `config.source` is down.
#[test]
fn a_publish_reaches_every_candidate_server() {
    // Every server it reads is its own on loopback, which answers.
    let _answering = ServersAnswering::set();
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let store1 = root.path().join("s1");
    let store2 = root.path().join("s2");
    let store3 = root.path().join("s3");
    for d in [&store1, &store2, &store3] {
        std::fs::create_dir_all(d).unwrap();
    }
    let s1 = start_loopback_server(&store1);
    let s2 = start_loopback_server(&store2);
    let s3 = start_loopback_server(&store3);
    let url1 = format!("http://{}/v1/alab", s1.addr);
    let url2 = format!("http://{}/v1/alab", s2.addr);
    let url3 = format!("http://{}/v1/alab", s3.addr);

    let admin_paths = Paths::for_home(&root.path().join("m3"));
    create(&admin_paths, &url1, None, Some("m3")).expect("create on s1");

    // Negative control ("One URL"): with no policy/browsed candidates yet, `s1` is at `seq 1`
    // while `s2` and `s3` hold nothing.
    assert_eq!(s1.held_seq("alab"), Some(1));
    assert_eq!(s2.held_seq("alab"), None);
    assert_eq!(s3.held_seq("alab"), None);

    // Publish `seq 2` with `s2` in `SwarmPolicy.servers` and `s3` in `found` (standing browse):
    // `publish_as` reaches ALL THREE candidate servers in one publish!
    let browsed_s3 = [crate::discover::DiscoveredVault {
        url: url3.clone(),
        txt: None,
    }];
    let found = |_: &Config| browsed_s3.to_vec();
    {
        let opened = admin_open(&admin_paths).expect("open seq 1");
        let mut doc = opened.doc.clone();
        let admins = doc.admins();
        let policy = SwarmPolicy {
            servers: vec![url2.clone()],
            ..SwarmPolicy::default()
        };
        doc.set_entry(
            &Audience::Hosts(admins),
            crate::SWARM_POLICY_ENTRY,
            policy.to_toml().as_bytes(),
        )
        .unwrap();
        doc.set_entry(&Audience::Everyone, "token", b"t-seq2")
            .unwrap();
        admin_publish_found(&admin_paths, &opened, &doc, &found).expect("publish seq 2");
    }
    assert_eq!(s1.held_seq("alab"), Some(2));
    assert_eq!(s2.held_seq("alab"), Some(2));
    assert_eq!(s3.held_seq("alab"), Some(2));

    // Stop `s1` (`config.source` is now dead, as when `m22` is away!):
    // - Negative control: one-URL `transport::publish(&Source::Url(url1), ...)` fails.
    // - Positive check: `admin_open_with_candidates` + `admin_publish_found` opens from `s2`/`s3`,
    //   publishes `seq 3` to `s2` and `s3`, and syncs `admin_paths` locally (`t-seq3`)!
    s1.stop();
    let seq2_bytes = match transport::fetch(
        &Source::Url(url2.clone()),
        None,
        aterm_spec::wait::MUST_HAPPEN,
    )
    .unwrap()
    {
        Fetched::Body(b) => b,
        other => panic!("expected seq 2 on s2, got {other:?}"),
    };
    assert!(
        transport::publish(&Source::Url(url1.clone()), &seq2_bytes).is_err(),
        "negative control: one-URL publish to dead config.source fails"
    );

    {
        let opened = admin_open_with_candidates(&admin_paths, std::slice::from_ref(&url3))
            .expect("open via s2/s3");
        assert_eq!(opened.seq, 2);
        let mut doc = opened.doc.clone();
        doc.set_entry(&Audience::Everyone, "token", b"t-seq3")
            .unwrap();
        let published = admin_publish_found(&admin_paths, &opened, &doc, &found)
            .expect("publish seq 3 with s1 down");
        assert_eq!(published.seq, 3);
        assert_eq!(
            published.local, None,
            "post-publish sync must succeed via candidate servers"
        );
    }
    assert_eq!(s2.held_seq("alab"), Some(3));
    assert_eq!(s3.held_seq("alab"), Some(3));
    assert_eq!(
        std::fs::read(admin_paths.home.join(".secrets/token")).unwrap(),
        b"t-seq3"
    );

    s2.stop();
    s3.stop();
}

/// A listener on loopback that takes connections and answers none: a browsed server that
/// accepts a PUT and never replies. It sends its index on `heard` for each connection it takes
/// and holds them all until `release`, then closes them and the next one it takes, and ends.
fn silent_server(
    index: usize,
    heard: std::sync::mpsc::Sender<usize>,
    release: std::sync::Arc<std::sync::atomic::AtomicBool>,
    held: std::sync::Arc<std::sync::Mutex<Vec<std::net::TcpStream>>>,
) -> (String, std::thread::JoinHandle<()>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let url = format!("http://{}/v1/alab", listener.local_addr().expect("addr"));
    let thread = std::thread::spawn(move || {
        for stream in listener.incoming() {
            if release.load(std::sync::atomic::Ordering::SeqCst) {
                return;
            }
            if let Ok(stream) = stream {
                held.lock().expect("held").push(stream);
                let _ = heard.send(index);
            }
        }
    });
    (url, thread)
}

/// A publish PUTs to every candidate server at once. A browsed server that accepts the PUT and
/// never answers holds the publish for one exchange deadline, not one per server, so a LAN full
/// of such adverts cannot hold an admin's publish for half an hour
/// (`docs/DESIGN-fluid-swarm-2026-10-28.md` F6, from the review).
/// NEGATIVE CONTROL: PUT one after another, the second silent server hears nothing until the
/// first one's 120 s deadline ends, past this test's hang detector.
#[test]
fn a_publish_puts_to_every_server_at_once() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let s1 = start_loopback_server(&root.path().join("s1"));
    let url1 = format!("http://{}/v1/alab", s1.addr);
    let admin_paths = Paths::for_home(&root.path().join("m3"));
    create(&admin_paths, &url1, None, Some("m3")).expect("create on s1");

    let (heard_tx, heard) = std::sync::mpsc::channel();
    let release = Arc::new(AtomicBool::new(false));
    let held = Arc::new(Mutex::new(Vec::new()));
    let silent: Vec<(String, std::thread::JoinHandle<()>)> = (0..2)
        .map(|index| {
            silent_server(
                index,
                heard_tx.clone(),
                Arc::clone(&release),
                Arc::clone(&held),
            )
        })
        .collect();
    let browsed: Vec<crate::discover::DiscoveredVault> = silent
        .iter()
        .map(|(url, _)| crate::discover::DiscoveredVault {
            url: url.clone(),
            txt: None,
        })
        .collect();
    let publisher = std::thread::spawn({
        let admin_paths = admin_paths.clone();
        move || {
            let found = move |_: &Config| browsed.clone();
            let opened = admin_open(&admin_paths).expect("open seq 1");
            let mut doc = opened.doc.clone();
            doc.set_entry(&Audience::Everyone, "token", b"t-seq2")
                .expect("set");
            admin_publish_found(&admin_paths, &opened, &doc, &found)
        }
    });

    let mut reached = BTreeSet::new();
    while reached.len() < silent.len() {
        let index = heard
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .unwrap_or_else(|_| panic!("never: a PUT reached every silent server ({reached:?})"));
        reached.insert(index);
    }
    // Both PUTs are under way while neither silent server has answered: let them fail now.
    release.store(true, Ordering::SeqCst);
    held.lock().expect("held").clear();
    let published = publisher
        .join()
        .expect("the publish ran")
        .expect("the source took the publish");
    assert_eq!(published.seq, 2);
    assert_eq!(s1.held_seq("alab"), Some(2));
    for (url, thread) in silent {
        // Ends a listener that took nothing since the release.
        let _ = std::net::TcpStream::connect(
            url.trim_start_matches("http://")
                .trim_end_matches("/v1/alab"),
        );
        thread.join().expect("the silent server ended");
    }
    s1.stop();
}

/// A publish holds this Mac's state lock only to check that the vault configuration is the one
/// its envelope was sealed for, not across its PUTs: while a browsed server that took the PUT
/// stays silent, another secrets command takes the state lock at once. Before, every command
/// that takes it (a sync's materialize, a draft's adopt, the next publish) waited out that
/// PUT's 120 s exchange deadline.
/// NEGATIVE CONTROL: holding `configured_admin`'s lock across the PUTs (main before this fix)
/// keeps the state lock from the other command until the silent server is let go, and the test
/// fails "never: the state lock while a PUT waits on a silent server".
#[test]
fn a_publish_leaves_the_state_lock_while_a_silent_server_holds_its_put() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let s1 = start_loopback_server(&root.path().join("s1"));
    let url1 = format!("http://{}/v1/alab", s1.addr);
    let admin_paths = Paths::for_home(&root.path().join("m3"));
    create(&admin_paths, &url1, None, Some("m3")).expect("create on s1");

    let (heard_tx, heard) = std::sync::mpsc::channel();
    let release = Arc::new(AtomicBool::new(false));
    let held = Arc::new(Mutex::new(Vec::new()));
    let (silent_url, silent_thread) =
        silent_server(0, heard_tx, Arc::clone(&release), Arc::clone(&held));
    let browsed = vec![crate::discover::DiscoveredVault {
        url: silent_url.clone(),
        txt: None,
    }];
    let publisher = std::thread::spawn({
        let admin_paths = admin_paths.clone();
        move || {
            let found = move |_: &Config| browsed.clone();
            let opened = admin_open(&admin_paths).expect("open seq 1");
            let mut doc = opened.doc.clone();
            doc.set_entry(&Audience::Everyone, "token", b"t-seq2")
                .expect("set");
            admin_publish_found(&admin_paths, &opened, &doc, &found)
        }
    });
    heard
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("never: the PUT reached the silent server");

    let (taken_tx, taken) = std::sync::mpsc::channel();
    let other = std::thread::spawn({
        let admin_paths = admin_paths.clone();
        move || {
            let lock = crate::client::StateLock::take(&admin_paths).map(drop);
            let _ = taken_tx.send(lock.is_ok());
        }
    });
    let took = taken.recv_timeout(aterm_spec::wait::MUST_HAPPEN);
    // Let the silent PUT fail now, whatever the other command saw, so every thread ends.
    release.store(true, Ordering::SeqCst);
    held.lock().expect("held").clear();
    assert_eq!(
        took.ok(),
        Some(true),
        "never: the state lock while a PUT waits on a silent server"
    );
    other.join().expect("the other command ran");
    let published = publisher
        .join()
        .expect("the publish ran")
        .expect("the source took the publish");
    assert_eq!(published.seq, 2);
    assert_eq!(s1.held_seq("alab"), Some(2));
    // Ends a listener that took nothing since the release.
    let _ = std::net::TcpStream::connect(
        silent_url
            .trim_start_matches("http://")
            .trim_end_matches("/v1/alab"),
    );
    silent_thread.join().expect("the silent server ended");
    s1.stop();
}

/// A server never stores an envelope that its pinned signer (or held key lineage) refuses, whether
/// pushed directly by HTTP `PUT` or pulled from a browsed peer (`docs/DESIGN-fluid-swarm-2026-10-28.md` F6).
/// NEGATIVE CONTROL: skipping pin verification accepts the higher-`seq` forged envelope.
#[test]
fn a_server_never_stores_an_envelope_the_pin_refuses() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let honest_dir = root.path().join("honest");
    let forger_dir = root.path().join("forger");
    std::fs::create_dir_all(&honest_dir).unwrap();
    std::fs::create_dir_all(&forger_dir).unwrap();

    let honest_seed = [0x11_u8; 32];
    let honest_pub = envelope::signer_public(&honest_seed).unwrap();
    let forger_seed = [0x99_u8; 32];
    let _forger_pub = envelope::signer_public(&forger_seed).unwrap();

    let honest_env = envelope::Unsigned {
        name: "alab".to_owned(),
        seq: 1,
        time: 1_800_000_000,
        handoff: Vec::new(),
        parents: Vec::new(),
        lockers: Vec::new(),
    };
    let honest_bytes = envelope::sign(&honest_env, &honest_seed).unwrap();
    std::fs::write(honest_dir.join("alab.vault"), &honest_bytes).unwrap();

    let forged_env = envelope::Unsigned {
        name: "alab".to_owned(),
        seq: 9,
        time: 1_800_000_010,
        handoff: Vec::new(),
        parents: Vec::new(),
        lockers: Vec::new(),
    };
    let forged_bytes = envelope::sign(&forged_env, &forger_seed).unwrap();
    std::fs::write(forger_dir.join("alab.vault"), &forged_bytes).unwrap();

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let s_honest = crate::server::start(
        listener,
        &ServerOptions {
            listen: "127.0.0.1:0".parse().expect("addr"),
            store: honest_dir,
            advertise: None,
            pins: vec![("alab".to_owned(), honest_pub)],
            hold: Duration::from_millis(100),
            keep_awake: false,
        },
    )
    .expect("start honest server");
    let s_forger = start_loopback_server(&forger_dir);

    let honest_url = format!("http://{}/v1/alab", s_honest.addr);
    let forger_url = format!("http://{}/v1/alab", s_forger.addr);

    // 1. Direct HTTP PUT of `forged_bytes` onto `s_honest` is refused (`403`), leaving `seq 1`.
    assert!(transport::publish(&Source::Url(honest_url.clone()), &forged_bytes).is_err());
    assert_eq!(s_honest.held_seq("alab"), Some(1));

    // 2. Peer pull from `s_forger` (even with TXT advertising `seq = 9` and Honest's fingerprint!)
    //    is refused by `s_honest`'s `put_vault` pin check, leaving `seq 1`.
    let forger_peer = [crate::discover::DiscoveredVault {
        url: forger_url,
        txt: Some(crate::server::VaultTxt {
            vault: "alab".to_owned(),
            signer: envelope::fingerprint(&honest_pub),
            seq: 9,
            head: [0xee; 32],
            label: Some("m99".to_owned()),
        }),
    }];
    assert_eq!(s_honest.pull_from_peers(&forger_peer), 0);
    assert_eq!(s_honest.held_seq("alab"), Some(1));

    // Negative control ("Skip verification"): parsing `forged_bytes` and checking `seq > 1` without
    // verifying against `honest_pub` accepts `seq 9`.
    let unverified_parsed = envelope::parse(&forged_bytes).expect("syntactically valid");
    assert!(
        unverified_parsed.verify_signature().is_ok() && unverified_parsed.seq > 1,
        "negative control: skipping pin verification accepts forged seq 9"
    );

    s_honest.stop();
    s_forger.stop();
}

/// Converge (`steward_ensure_standby_server`) moves a held `0.145.0` server job (`m22`'s
/// `~/.local/share/aterm-secrets/aterm secrets serve --advertise alab-secrets`) onto the new
/// `>= 0.146.0` build once that build starts and serves the same store, rolling back if the new
/// build fails to start or serves nothing (`docs/DESIGN-fluid-swarm-2026-10-28.md` F6 & §4.3
/// step 1), and leaves a job whose store holds no ALab vault this build verifies where it is.
/// NEGATIVE CONTROL: `service::repair_server_with` alone keeps the job on the `0.145.0` snapshot.
#[test]
fn a_held_0_145_server_job_moves_once_the_new_build_serves_its_store() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let srv_seed_dir = root.path().join("seed-srv");
    std::fs::create_dir_all(&srv_seed_dir).unwrap();
    let srv_seed = start_loopback_server(&srv_seed_dir);
    let url = format!("http://{}/v1/alab", srv_seed.addr);

    let m22 = Paths::for_home(&root.path().join("m22"));
    create(&m22, &url, None, Some("m22")).expect("create alab");
    let pin = config(&m22).unwrap().unwrap().signer.unwrap();
    // Populate `m22`'s local server store (`m22.served()`) with the verified `alab.vault` copy.
    std::fs::create_dir_all(m22.served()).unwrap();
    let cached_bytes = std::fs::read(m22.cache_file()).expect("cached alab.vault");
    std::fs::write(m22.served().join("alab.vault"), &cached_bytes).unwrap();
    srv_seed.stop();

    struct RetiredGuard;
    impl Drop for RetiredGuard {
        fn drop(&mut self) {
            envelope::TEST_RETIRED.with(|cell| *cell.borrow_mut() = None);
        }
    }
    envelope::TEST_RETIRED.with(|cell| {
        *cell.borrow_mut() = Some(Retired {
            keys: Vec::new(),
            current: Some(pin),
        });
    });
    let _retired_guard = RetiredGuard;

    // Verify `served_by` accepts both `m22`'s `--advertise alab-secrets` args and newly
    // installed member server args (catching any unknown CLI flag such as a bogus `--keep-awake`).
    // A free loopback port, held until the new build's server takes it, stands in for 7484, so
    // the move's serve check reaches only this test's server.
    let reserved = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let listen = reserved.local_addr().expect("addr");
    let m22_args: Vec<String> = [
        "secrets",
        "serve",
        "--advertise",
        "alab-secrets",
        "--listen",
        &listen.to_string(),
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    assert_eq!(
        crate::cli::served_by(&m22, &m22_args).map(|(store, _)| store),
        Some(m22.served())
    );

    #[cfg(target_os = "macos")]
    {
        // Use real signed system executables outside `$TMPDIR` (`/bin/sh` as the held `0.145.0`
        // snapshot and `/usr/bin/true` as the new `0.146.0` build) so no Mach-O in `$TMPDIR` is
        // ever copied or unlinked (`AGENTS.md` macOS 27.0.1 `syspolicyd` rule).
        let old_snapshot_buf = std::fs::canonicalize("/bin/sh").unwrap();
        let new_build_buf = std::fs::canonicalize("/usr/bin/true").unwrap();
        let old_snapshot = old_snapshot_buf.as_path();
        let new_build = new_build_buf.as_path();

        // Write `m22`'s unpinned held `0.145.0` server LaunchAgent plist via `write_plist`.
        let plist_path = crate::service::write_plist(
            &m22.home,
            crate::service::Job::Server,
            old_snapshot,
            &m22_args,
        )
        .expect("write initial server plist");

        let held_before =
            crate::service::held_at(&m22.home, crate::service::Job::Server, Some(new_build))
                .expect("held 0.145 server detected");
        assert_eq!(held_before.program, old_snapshot);
        assert!(!held_before.pinned());

        // Negative control ("A repair alone: the job stays on the snapshot"): `repair_server_with`
        // sees `old_snapshot` still exists on disk and leaves `/bin/sh` untouched in the plist!
        let repaired = crate::service::repair_server_with(
            &m22.home,
            &|p| p.exists(),
            &|| Ok(crate::service::State::Absent),
            |_args| panic!("repair must not reinstall while the held snapshot exists"),
        )
        .expect("repair_server_with");
        assert_eq!(repaired, None);
        let after_repair = std::fs::read_to_string(&plist_path).unwrap();
        assert!(
            after_repair.contains(old_snapshot.to_str().unwrap())
                && !after_repair.contains(new_build.to_str().unwrap()),
            "negative control: repair alone must keep the held 0.145 snapshot ({after_repair})"
        );

        let own_0146 = locker_skew::Own {
            version: (0, 146, 0),
            release: true,
            program: Some(new_build.to_path_buf()),
        };
        let release_of = |p: &Path| -> Result<locker_skew::Version, String> {
            if p == old_snapshot {
                Ok((0, 145, 0))
            } else if p == new_build {
                Ok((0, 146, 0))
            } else {
                Err(format!("unexpected program {}", p.display()))
            }
        };
        let not_installed = || Ok(crate::service::State::Absent);
        let running = || {
            Ok(crate::service::State::Installed {
                pid: Some(42),
                unit: plist_path.clone(),
            })
        };

        let stale_destination = Path::new("/Applications/aterm.app/Contents/MacOS/aterm");
        let caller_0147 = locker_skew::Own {
            version: (0, 147, 0),
            ..own_0146.clone()
        };
        let restarted = steward::steward_ensure_standby_server_with(
            &m22,
            None,
            &caller_0147,
            stale_destination,
            &not_installed,
            &not_installed,
            &|_| Some(held_before.clone()),
            &|program| {
                Ok(if program == stale_destination {
                    (0, 91, 0)
                } else {
                    (0, 146, 0)
                })
            },
            &|_| panic!("a stale destination must not start"),
            &|_| panic!("a stale destination must not serve"),
            &mut |_, _, _| panic!("a held job must not move to a stale destination"),
            &mut |program, args| {
                assert_eq!(program, held_before.program);
                assert_eq!(args, held_before.args);
                Ok(Some("restarted the retained program".to_owned()))
            },
        );
        assert!(matches!(restarted.unwrap(), StewardStandbyServer::Done(_)));

        // 0. A store whose ALab vault this build does not verify (another key's) could never be
        //    seen served after a move, so the job stays where it is.
        let store_vault = m22.served().join("alab.vault");
        let strangers = envelope::sign(
            &envelope::Unsigned {
                name: crate::ALAB_VAULT.to_owned(),
                seq: 9,
                time: 1_792_530_009,
                handoff: Vec::new(),
                parents: Vec::new(),
                lockers: vec![envelope::Locker {
                    label: "admin".to_owned(),
                    armored: envelope::test_armor(&[9; 40]),
                }],
            },
            &[7; 32],
        )
        .expect("another key's ALab envelope");
        std::fs::write(&store_vault, strangers).unwrap();
        let check_stranger = steward::steward_standby_server_check_with(
            &m22,
            None,
            &own_0146,
            Some(new_build),
            &not_installed,
            &running,
            &|np| crate::service::held_at(&m22.home, crate::service::Job::Server, np),
            &release_of,
            &|_| Ok(()),
        )
        .expect("check with another key's store");
        assert!(
            matches!(check_stranger, StewardStandbyServer::Current(_)),
            "a store with no ALab vault this build verifies is no move: {check_stranger:?}"
        );
        std::fs::write(&store_vault, &cached_bytes).unwrap();

        // 1. Check reports `StewardStandbyServer::Due` naming the `0.145.0 -> 0.146.0` move.
        let check_due = steward::steward_standby_server_check_with(
            &m22,
            None,
            &own_0146,
            Some(new_build),
            &not_installed,
            &running,
            &|np| crate::service::held_at(&m22.home, crate::service::Job::Server, np),
            &release_of,
            &|_| Ok(()),
        )
        .expect("check due");
        assert!(
            matches!(
                &check_due,
                StewardStandbyServer::Due { found, makes }
                    if found.contains("0.145.0") && makes.contains("0.146.0")
            ),
            "expected Due for held 0.145 server, got {check_due:?}"
        );

        let write_plist_for = |prog: &Path, args: &[String]| {
            crate::service::write_plist(&m22.home, crate::service::Job::Server, prog, args)
                .expect("write plist");
        };

        // 2. If the new build fails when installing/starting (`move_held_with`'s installer fails
        //    after writing the candidate plist), `moved_or_put_back` rolls the plist back to
        //    `old_snapshot` and returns an error.
        let failed_move = steward::steward_ensure_standby_server_with(
            &m22,
            None,
            &own_0146,
            new_build,
            &not_installed,
            &running,
            &|np| crate::service::held_at(&m22.home, crate::service::Job::Server, np),
            &release_of,
            &|_| Ok(()),
            &|_| panic!("a build that did not start is not asked to serve"),
            &mut |held, prog, serves| {
                crate::service::move_held_serving_with(
                    &m22.home,
                    held,
                    prog,
                    &not_installed,
                    &mut |p| {
                        write_plist_for(p, &held.args);
                        if p == new_build {
                            Err(Error::new("new build failed to start serving store"))
                        } else {
                            Ok("put back".to_owned())
                        }
                    },
                    serves,
                )
            },
            &mut |_prog, _args| panic!("must move_held, not fresh-install"),
        );
        assert!(failed_move.is_err(), "failed start must refuse move");
        assert!(
            std::fs::read_to_string(&plist_path)
                .unwrap()
                .contains(old_snapshot.to_str().unwrap()),
            "failed start must restore the 0.145 snapshot in plist"
        );

        // 2b. A new build that starts but serves nothing at the job's address is no move either:
        //     the job goes back on the snapshot. (Nothing accepts on the reserved port yet.)
        let mut installed_on = Vec::new();
        let silent_move = steward::steward_ensure_standby_server_with(
            &m22,
            None,
            &own_0146,
            new_build,
            &not_installed,
            &running,
            &|np| crate::service::held_at(&m22.home, crate::service::Job::Server, np),
            &release_of,
            &|_| Ok(()),
            &|url| steward::serves_alab(url, Duration::from_millis(500)),
            &mut |held, prog, serves| {
                crate::service::move_held_serving_with(
                    &m22.home,
                    held,
                    prog,
                    &not_installed,
                    &mut |p| {
                        write_plist_for(p, &held.args);
                        installed_on.push(p.to_path_buf());
                        Ok(format!("installed on {}", p.display()))
                    },
                    serves,
                )
            },
            &mut |_prog, _args| panic!("must move_held, not fresh-install"),
        );
        assert!(silent_move.is_err(), "{silent_move:?}");
        assert_eq!(installed_on, [new_build, old_snapshot]);
        assert!(
            std::fs::read_to_string(&plist_path)
                .unwrap()
                .contains(old_snapshot.to_str().unwrap()),
            "a build that serves nothing must restore the 0.145 snapshot in plist"
        );

        let mut reserved = Some(reserved);
        let mut started = None;
        // 3. Once the new build starts and serves the same store, the held `0.145.0` server moves
        //    onto `new_build` (`/usr/bin/true`) keeping its arguments, and a subsequent check
        //    reports `Current`!
        let moved = steward::steward_ensure_standby_server_with(
            &m22,
            None,
            &own_0146,
            new_build,
            &not_installed,
            &running,
            &|np| crate::service::held_at(&m22.home, crate::service::Job::Server, np),
            &release_of,
            &|_| Ok(()),
            // A one-minute hang detector: the server below answers at once.
            &|url| steward::serves_alab(url, Duration::from_secs(60)),
            &mut |held, prog, serves| {
                crate::service::move_held_serving_with(
                    &m22.home,
                    held,
                    prog,
                    &not_installed,
                    &mut |p| {
                        write_plist_for(p, &held.args);
                        if p == new_build {
                            // The new build's job starts and serves the store its arguments name.
                            started = Some(
                                crate::server::start(
                                    reserved.take().expect("one start"),
                                    &ServerOptions {
                                        listen,
                                        store: m22.served(),
                                        advertise: None,
                                        pins: Vec::new(),
                                        hold: Duration::from_millis(100),
                                        keep_awake: false,
                                    },
                                )
                                .expect("serving"),
                            );
                        }
                        Ok(format!("moved to {}", p.display()))
                    },
                    serves,
                )
            },
            &mut |_prog, _args| panic!("must move_held, not fresh-install"),
        )
        .expect("move held 0.145 server");
        assert!(matches!(moved, StewardStandbyServer::Done(_)), "{moved:?}");
        let after_move = std::fs::read_to_string(&plist_path).unwrap();
        assert!(
            after_move.contains(new_build.to_str().unwrap())
                && after_move.contains("alab-secrets")
                && !after_move.contains(old_snapshot.to_str().unwrap()),
            "expected moved plist on new_build keeping --advertise alab-secrets: {after_move}"
        );
        assert!(
            crate::service::held_at(&m22.home, crate::service::Job::Server, Some(new_build))
                .is_none(),
            "job now runs new_build so it is no longer held"
        );
        let check_after = steward::steward_standby_server_check_with(
            &m22,
            None,
            &own_0146,
            Some(new_build),
            &not_installed,
            &running,
            &|np| crate::service::held_at(&m22.home, crate::service::Job::Server, np),
            &release_of,
            &|_| Ok(()),
        )
        .expect("check after move");
        assert!(matches!(check_after, StewardStandbyServer::Current(_)));
        started.expect("the new build's server started").stop();

        // 4. Also verify a non-admin member (`m9`) with no server installed gets a valid `serve`
        //    command line installed, and `record_server_off` (`aterm secrets server off`) suppresses it.
        let mut installed_args: Option<Vec<String>> = None;
        let fresh_install = steward::steward_ensure_standby_server_with(
            &m22,
            None,
            &own_0146,
            new_build,
            &not_installed,
            &not_installed,
            &|_| None,
            &release_of,
            &|_| Ok(()),
            &|_| unreachable!(),
            &mut |_, _, _| unreachable!(),
            &mut |_prog, args| {
                installed_args = Some(args.to_vec());
                Ok(Some("installed".to_owned()))
            },
        )
        .expect("fresh member install");
        assert!(matches!(fresh_install, StewardStandbyServer::Done(_)));
        let args = installed_args.expect("installer invoked");
        assert_eq!(
            crate::cli::served_by(&m22, &args).map(|(store, _)| store),
            Some(m22.served()),
            "installed member server args must parse cleanly: {args:?}"
        );
        assert!(
            args.iter().any(|arg| arg == "--no-keep-awake"),
            "a member server leaves its Mac free to idle-sleep: {args:?}"
        );

        crate::service::record_server_off(&m22.home).expect("record server-off");
        let check_off = steward::steward_standby_server_check_with(
            &m22,
            None,
            &own_0146,
            Some(new_build),
            &not_installed,
            &not_installed,
            &|_| None,
            &release_of,
            &|_| Ok(()),
        )
        .expect("check when turned off");
        assert!(matches!(check_off, StewardStandbyServer::Idle(_)));
    }
    #[cfg(not(target_os = "macos"))]
    drop(reserved);
}

/// A moved server counts as serving (`steward::serves_alab`) only once what it answers is the
/// ALab vault this build names: one server, one envelope, and only the build's ALab key differs.
#[test]
fn a_moved_server_serves_only_once_it_answers_with_the_alab_vault() {
    struct RetiredGuard;
    impl Drop for RetiredGuard {
        fn drop(&mut self) {
            envelope::TEST_RETIRED.with(|cell| *cell.borrow_mut() = None);
        }
    }
    let _retired_guard = RetiredGuard;
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let store = root.path().join("served");
    std::fs::create_dir_all(&store).unwrap();
    let seed = [7; 32];
    let signed = envelope::sign(
        &envelope::Unsigned {
            name: crate::ALAB_VAULT.to_owned(),
            seq: 3,
            time: 1_792_530_003,
            handoff: Vec::new(),
            parents: Vec::new(),
            lockers: vec![envelope::Locker {
                label: "admin".to_owned(),
                armored: envelope::test_armor(&[3; 40]),
            }],
        },
        &seed,
    )
    .expect("signed");
    std::fs::write(store.join("alab.vault"), signed).unwrap();
    let server = start_loopback_server(&store);
    let url = format!("http://{}/v1/alab", server.addr);

    // This build names another key for the ALab vault: what the server answers is not it.
    envelope::TEST_RETIRED.with(|cell| {
        *cell.borrow_mut() = Some(Retired {
            keys: Vec::new(),
            current: Some([8; 32]),
        });
    });
    assert!(steward::serves_alab(&url, Duration::from_millis(500)).is_err());

    // The same answer from a build that names its signer.
    envelope::TEST_RETIRED.with(|cell| {
        *cell.borrow_mut() = Some(Retired {
            keys: Vec::new(),
            current: Some(envelope::signer_public(&seed).expect("public")),
        });
    });
    // A one-minute hang detector: the server answers at once.
    steward::serves_alab(&url, Duration::from_secs(60)).expect("serves the ALab vault");
    server.stop();
}

/// The window's mend moves a held `0.145.0` ALab server (`m22`'s `--advertise alab-secrets` job)
/// as the converge step does (`DESIGN-fluid-swarm` §4.3 step 1): only where its store holds the
/// ALab vault this build verifies, handing the move the address its arguments serve, so
/// `mend_held` counts it moved only once that answers with the vault
/// (`service::move_held_serving`). One whose store holds another key's vault is not this move.
/// NEGATIVE CONTROLS: the store-directory rule moves the other key's store; a mend that hands
/// over no address moves the job unwatched.
#[test]
fn the_windows_mend_moves_a_held_alab_server_only_watching_it_serve() {
    use crate::service::{Held, Job};
    struct RetiredGuard;
    impl Drop for RetiredGuard {
        fn drop(&mut self) {
            envelope::TEST_RETIRED.with(|cell| *cell.borrow_mut() = None);
        }
    }
    let _retired_guard = RetiredGuard;
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let m22 = Paths::for_home(&root.path().join("m22"));
    std::fs::create_dir_all(m22.served()).unwrap();
    let seed = [7; 32];
    let signed = envelope::sign(
        &envelope::Unsigned {
            name: crate::ALAB_VAULT.to_owned(),
            seq: 81,
            time: 1_792_530_081,
            handoff: Vec::new(),
            parents: Vec::new(),
            lockers: vec![envelope::Locker {
                label: "admin".to_owned(),
                armored: envelope::test_armor(&[81; 40]),
            }],
        },
        &seed,
    )
    .expect("signed");
    std::fs::write(m22.served().join("alab.vault"), signed).unwrap();
    envelope::TEST_RETIRED.with(|cell| {
        *cell.borrow_mut() = Some(Retired {
            keys: Vec::new(),
            current: Some(envelope::signer_public(&seed).expect("public")),
        });
    });
    let held = Held {
        job: Job::Server,
        program: PathBuf::from("/Users//x/.local/share/aterm-secrets/aterm"),
        args: [
            "secrets",
            "serve",
            "--advertise",
            "alab-secrets",
            "--listen",
            "0.0.0.0:7999",
        ]
        .map(String::from)
        .into(),
        pin: None,
    };
    let release = locker_skew::Own {
        version: (0, 146, 0),
        release: true,
        program: None,
    };
    let watched = std::cell::RefCell::new(Vec::new());
    let mend = || {
        mend_held_with(
            &m22,
            &release,
            &held,
            Some(Path::new("/installed/aterm")),
            &|program| {
                Ok(if program == Path::new("/installed/aterm") {
                    release.version
                } else {
                    (0, 145, 0)
                })
            },
            &mut |watch| {
                watched.borrow_mut().push(watch.map(str::to_owned));
                Ok(Some("moved".to_owned()))
            },
        )
        .expect("judged")
    };
    assert_eq!(mend(), HeldMend::Moved("moved".to_owned()));
    assert_eq!(
        *watched.borrow(),
        [Some("http://127.0.0.1:7999/v1/alab".to_owned())],
        "the move watches the address the job serves"
    );

    let installed = locker_skew::Own {
        version: (0, 147, 0),
        ..release.clone()
    };
    for version in [(0, 146, 0), (0, 147, 0)] {
        let moved = mend_held_with(
            &m22,
            &installed,
            &held,
            Some(Path::new("/installed/aterm")),
            &|program| {
                Ok(if program == Path::new("/installed/aterm") {
                    installed.version
                } else {
                    version
                })
            },
            &mut |watch| {
                assert_eq!(watch, Some("http://127.0.0.1:7999/v1/alab"));
                Ok(Some("moved".into()))
            },
        )
        .unwrap();
        assert_eq!(moved, HeldMend::Moved("moved".into()));
        assert!(
            steward::held_server_move_due(
                &m22,
                &installed,
                &held,
                Path::new("/installed/aterm"),
                &|program| Ok(if program == Path::new("/installed/aterm") {
                    installed.version
                } else {
                    version
                }),
                &|_| Ok(())
            )
            .is_some()
        );
    }
    let destination = Path::new("/Applications/aterm.app/Contents/MacOS/aterm");
    let starts = std::cell::Cell::new(0);
    assert!(
        steward::held_server_move_due(
            &m22,
            &installed,
            &held,
            destination,
            &|program| Ok(if program == destination {
                (0, 91, 0)
            } else {
                (0, 146, 0)
            }),
            &|_| {
                starts.set(starts.get() + 1);
                Ok(())
            }
        )
        .is_none()
    );
    assert_eq!(
        starts.get(),
        0,
        "the stale destination is refused before startup or mutation"
    );
    assert!(
        steward::held_server_move_due(
            &m22,
            &installed,
            &held,
            Path::new("/installed/aterm"),
            &|_| Ok((0, 148, 0)),
            &|_| Ok(())
        )
        .is_none()
    );

    // A build that names another key for the ALab vault: the store's vault is not one it could
    // see served, and this Mac administers no vault, so the job stays.
    envelope::TEST_RETIRED.with(|cell| {
        *cell.borrow_mut() = Some(Retired {
            keys: Vec::new(),
            current: Some([8; 32]),
        });
    });
    assert_eq!(mend(), HeldMend::Kept(HeldKept::NoAdmin));
    assert_eq!(watched.borrow().len(), 1, "nothing more moved");
}

/// Members follow verified membership records across server failover (`F5` + `F13`): when the
/// configured primary server goes down after a peer replicated new records, `sync_records_with_candidates`
/// pulls the missing records from the surviving peer and merges the union into `paths.records_file()`.
#[test]
fn a_member_follows_records_across_failover_when_primary_dies() {
    // Every server it reads is its own on loopback, which answers or, stopped, refuses at once.
    let _answering = ServersAnswering::set();
    use crate::membership_log::{Body, Entrant, Evidence, Log, Record, encode_records};

    let root = aterm_tempfile::tempdir().expect("tempdir");
    let srv1_dir = root.path().join("srv1");
    let srv2_dir = root.path().join("srv2");
    std::fs::create_dir_all(&srv1_dir).unwrap();
    std::fs::create_dir_all(&srv2_dir).unwrap();
    let s1 = start_loopback_server(&srv1_dir);
    let s2 = start_loopback_server(&srv2_dir);
    let url1 = format!("http://{}/v1/team", s1.addr);
    let url2 = format!("http://{}/v1/team", s2.addr);

    let admin = Paths::for_home(&root.path().join("admin"));
    let member = Paths::for_home(&root.path().join("member"));
    create(&admin, &url1, None, Some("m1")).expect("create team");

    // Member joins `url1` as its configured primary.
    let member_id = machine_identity(&member).expect("member identity");
    {
        let opened = admin_open(&admin).unwrap();
        let mut doc = opened.doc.clone();
        doc.add_machine("m2", &member_id.recipient().to_bech32())
            .unwrap();
        admin_publish(&admin, &opened, &doc).unwrap();
    }
    let env_v2 = match transport::fetch(
        &Source::Url(url1.clone()),
        None,
        aterm_spec::wait::MUST_HAPPEN,
    )
    .unwrap()
    {
        Fetched::Body(b) => b,
        other => panic!("expected envelope body, got {other:?}"),
    };
    transport::publish(&Source::Url(url2.clone()), &env_v2).unwrap();
    let pin = config(&admin).unwrap().unwrap().signer.unwrap();
    let pinned_url1 = format!("{url1}#{}", crate::b64::encode_padded(&pin));
    join_host(&member, &pinned_url1, "m2");

    let mk_entrant = |tag: u8, label: &str| {
        let writer = [tag; 32];
        let machine = [tag.wrapping_add(0x20); 16];
        let inc = [tag; 16];
        let identity = crate::age::Identity::from_secret_bytes([tag.wrapping_add(0x40); 32]);
        let req = crate::commons::binding::Request::create(
            [0x31; 16], [0x42; 32], machine, inc, &identity, &writer,
        )
        .unwrap();
        let actor = req.binding().actor();
        (
            writer,
            inc,
            actor,
            Entrant {
                request: req,
                node_key: [tag.wrapping_add(0x80); 32],
                name: format!("mac-{tag}"),
                label: label.to_owned(),
            },
        )
    };
    let (w1, inc1, m1_actor, e1) = mk_entrant(1, "m1");
    let (_, _, _, e2) = mk_entrant(2, "m2");
    let signer_seed = admin_open(&admin).expect("admin key").seed;
    let genesis = Record::found(&signer_seed, vec![e1], 1_000).expect("genesis");
    let admit_two = Log::new(genesis.clone())
        .unwrap()
        .draft(
            &m1_actor,
            1_001,
            Body::Admit {
                entrant: e2,
                evidence: Evidence::Phrase,
            },
        )
        .sign(&w1, inc1)
        .unwrap();

    transport::post_records(&url1, genesis.bytes()).unwrap();
    assert_eq!(sync_records(&member, None).expect("sync genesis"), 1);
    assert_eq!(
        load_records(&member).unwrap().inventory(),
        std::collections::BTreeSet::from([*genesis.id()])
    );

    // Primary `s1` dies while peer `s2` holds `{genesis, admit_two}`.
    transport::post_records(&url2, &encode_records([&genesis, &admit_two])).unwrap();
    s1.stop();

    // Negative control: without failover candidates, `sync_records` fails once `s1` is dead.
    assert!(sync_records(&member, None).is_err());

    // With candidate `url2`, `sync_records_with_candidates` fails over and merges `admit_two`.
    assert_eq!(
        sync_records_with_candidates(&member, None, std::slice::from_ref(&url2))
            .expect("failover record sync"),
        1
    );
    assert_eq!(
        load_records(&member).unwrap().inventory(),
        std::collections::BTreeSet::from([*genesis.id(), *admit_two.id()])
    );

    s2.stop();
}

/// A member's `records` file that this build refuses as a whole (a newer build can refuse what an
/// older one stored) keeps what this build still takes, and the next write replaces it. Before,
/// `load_records` failed on it every time, so `sync_records`, `save_records` and Commons record
/// repair stopped on that Mac for good, while a server skips such a file and rewrites it.
#[test]
fn a_records_file_this_build_refuses_keeps_what_it_takes_and_records_go_on() {
    use crate::membership_log::{Body, Entrant, Evidence, Log, Record, Store};

    let root = aterm_tempfile::tempdir().expect("tempdir");
    let member = Paths::for_home(root.path());
    std::fs::create_dir_all(&member.dir).expect("secrets dir");
    let mk_entrant = |tag: u8, label: &str| {
        let writer = [tag; 32];
        let machine = [tag.wrapping_add(0x20); 16];
        let inc = [tag; 16];
        let identity = crate::age::Identity::from_secret_bytes([tag.wrapping_add(0x40); 32]);
        let req = crate::commons::binding::Request::create(
            [0x31; 16], [0x42; 32], machine, inc, &identity, &writer,
        )
        .unwrap();
        let actor = req.binding().actor();
        (
            writer,
            inc,
            actor,
            Entrant {
                request: req,
                node_key: [tag.wrapping_add(0x80); 32],
                name: format!("mac-{tag}"),
                label: label.to_owned(),
            },
        )
    };
    let (w1, inc1, m1_actor, e1) = mk_entrant(1, "m1");
    let (_, _, _, e2) = mk_entrant(2, "m2");
    let genesis = Record::found(&[7; 32], vec![e1], 1_000).expect("genesis");
    let admit_two = Log::new(genesis.clone())
        .unwrap()
        .draft(
            &m1_actor,
            1_001,
            Body::Admit {
                entrant: e2,
                evidence: Evidence::Phrase,
            },
        )
        .sign(&w1, inc1)
        .unwrap();

    // The genesis, then a record whose signature no longer verifies.
    let mut refused = genesis.bytes().to_vec();
    let mut broken = admit_two.bytes().to_vec();
    *broken.last_mut().expect("non-empty") ^= 0x01;
    refused.extend_from_slice(&broken);
    crate::fsx::write_atomic(&member.records_file(), &refused, 0o600).unwrap();
    assert!(
        Store::decode("", &refused).is_err(),
        "a stream refused whole"
    );

    let held = load_records(&member).expect("a refused file never stops this Mac's records");
    assert_eq!(
        held.inventory(),
        std::collections::BTreeSet::from([*genesis.id()])
    );
    assert_eq!(
        save_records(&member, admit_two.bytes()).expect("records go on"),
        1
    );
    let rewritten = std::fs::read(member.records_file()).unwrap();
    assert_eq!(
        Store::decode("", &rewritten)
            .expect("the file reads whole again")
            .inventory(),
        std::collections::BTreeSet::from([*genesis.id(), *admit_two.id()])
    );
}

/// A member's `records` file past `MAX_RECORDS_STREAM` (a build that bounded its store by count
/// alone wrote one once about 64 records each carried a 1 MiB `Put`) made `load_records` fail
/// before `Store::recover` ran, so record sync, saves, Commons repair and every envelope apply
/// that reads the log stopped on that Mac for good: nothing truncates the file, and every write
/// starts by loading it. It now keeps the records the file's first `MAX_RECORDS_STREAM` bytes
/// hold, and the next write replaces it.
/// NEGATIVE CONTROL: `fsx::read_owned`'s refusal of a file past its cap in `load_records` (main
/// before this fix) fails both the load and the save.
#[test]
fn a_records_file_past_the_stream_cap_keeps_what_it_holds_and_records_go_on() {
    use crate::membership_log::{
        Body, Draft, Entrant, Evidence, Log, MAX_RECORDS_STREAM, Record, Store, encode_records,
    };

    let root = aterm_tempfile::tempdir().expect("tempdir");
    let member = Paths::for_home(root.path());
    std::fs::create_dir_all(&member.dir).expect("secrets dir");
    let mk_entrant = |tag: u8, label: &str| {
        let writer = [tag; 32];
        let machine = [tag.wrapping_add(0x20); 16];
        let inc = [tag; 16];
        let identity = crate::age::Identity::from_secret_bytes([tag.wrapping_add(0x40); 32]);
        let req = crate::commons::binding::Request::create(
            [0x31; 16], [0x42; 32], machine, inc, &identity, &writer,
        )
        .unwrap();
        let actor = req.binding().actor();
        (
            writer,
            inc,
            actor,
            Entrant {
                request: req,
                node_key: [tag.wrapping_add(0x80); 32],
                name: format!("mac-{tag}"),
                label: label.to_owned(),
            },
        )
    };
    let (w1, inc1, m1_actor, e1) = mk_entrant(1, "m1");
    let (_, _, _, e2) = mk_entrant(2, "m2");
    let (w9, inc9, _, e9) = mk_entrant(9, "m9");
    let genesis = Record::found(&[7; 32], vec![e1, e9], 1_000).expect("genesis");
    let puts: Vec<Record> = (0..66u64)
        .map(|i| {
            let mut parent = [0x66; 32];
            parent[..8].copy_from_slice(&i.to_le_bytes());
            Draft {
                network: *genesis.id(),
                sequence: 1,
                previous: None,
                parents: vec![parent],
                authored_ms: 3_000 + i,
                body: Body::Put {
                    audience: crate::membership_log::EpochAudience::Everyone,
                    epoch: [0x44; 32],
                    salt: [0x33; 32],
                    ciphertext: vec![0x5a; crate::membership_log::entries::MAX_PUT_CIPHERTEXT],
                },
            }
            .sign(&w9, inc9)
            .unwrap()
        })
        .collect();
    let mut file = genesis.bytes().to_vec();
    file.extend_from_slice(&encode_records(&puts));
    assert!(file.len() > MAX_RECORDS_STREAM);
    crate::fsx::write_atomic(&member.records_file(), &file, 0o600).unwrap();
    drop(file);

    let held = load_records(&member).expect("a file past the cap never stops this Mac's records");
    assert!(held.contains(genesis.id()));
    assert!(held.encode().len() <= MAX_RECORDS_STREAM);
    drop(held);

    let admit_two = Log::new(genesis.clone())
        .unwrap()
        .draft(
            &m1_actor,
            1_001,
            Body::Admit {
                entrant: e2,
                evidence: Evidence::Phrase,
            },
        )
        .sign(&w1, inc1)
        .unwrap();
    assert_eq!(
        save_records(&member, admit_two.bytes()).expect("records go on"),
        1
    );
    let rewritten = std::fs::read(member.records_file()).unwrap();
    assert!(rewritten.len() <= MAX_RECORDS_STREAM);
    let reread = Store::decode("", &rewritten).expect("the file reads whole again");
    assert!(reread.contains(genesis.id()) && reread.contains(admit_two.id()));
}

/// A Mac one release behind a rotation of the ALab vault's key names only the old key, and
/// follows the new one through the envelope's handoff (`followed`). When Genesis v2 comes after
/// the rotation the new key signs it, and that Mac's dormant store, keyed to the build's
/// `ALAB_SIGNER` alone, refused it on every `load_records`: the Mac stayed rootless, and every
/// sync pass downloaded and verified the whole log for nothing, for the days the fleet takes to
/// update. `load_records` now also takes a genesis signed by the key this Mac's verified config
/// pins or any key it reached that pin through, and its cache of decoded files (82fe1dc915)
/// matches those keys too: one keyed on the file alone kept the store read before the re-pin.
/// NEGATIVE CONTROL: `load_records` building the store from `Store::for_vault` alone (main
/// before this fix) roots nothing; its cache matching the vault and the file's digest alone
/// returns the store read under the old pin.
#[test]
fn a_mac_a_release_behind_a_rotation_roots_the_genesis_the_new_key_signed() {
    use crate::membership_log::{Body, Entrant, Evidence, Log, Record, encode_records};

    struct ResetRetired;
    impl Drop for ResetRetired {
        fn drop(&mut self) {
            envelope::TEST_RETIRED.with(|cell| *cell.borrow_mut() = None);
        }
    }
    let old_seed = [0x57; 32];
    let new_seed = [0x58; 32];
    let old = envelope::signer_public(&old_seed).unwrap();
    let new = envelope::signer_public(&new_seed).unwrap();
    // This build names only the old key.
    envelope::TEST_RETIRED.with(|cell| {
        *cell.borrow_mut() = Some(Retired {
            keys: Vec::new(),
            current: Some(old),
        });
    });
    let _reset = ResetRetired;

    let root = aterm_tempfile::tempdir().expect("tempdir");
    let member = Paths::for_home(root.path());
    let pin = |signer: [u8; 32], followed: Vec<[u8; 32]>| {
        save_config(
            &member,
            &Config {
                source: Source::Url("http://alab-secrets.local:7484/v1/alab".to_owned()),
                name: crate::ALAB_VAULT.to_owned(),
                signer: Some(signer),
                followed,
                host: "m5".to_owned(),
            },
        )
        .unwrap();
        assert!(alab_vault(&config(&member).unwrap().unwrap()));
    };
    // Pinned to the old key, before this Mac follows the rotation.
    pin(old, Vec::new());

    let mk_entrant = |tag: u8, label: &str| {
        let writer = [tag; 32];
        let inc = [tag; 16];
        let identity = crate::age::Identity::from_secret_bytes([tag.wrapping_add(0x40); 32]);
        let req = crate::commons::binding::Request::create(
            [0x31; 16],
            [0x42; 32],
            [tag.wrapping_add(0x20); 16],
            inc,
            &identity,
            &writer,
        )
        .unwrap();
        let actor = req.binding().actor();
        (
            writer,
            inc,
            actor,
            Entrant {
                request: req,
                node_key: [tag.wrapping_add(0x80); 32],
                name: format!("mac-{tag}"),
                label: label.to_owned(),
            },
        )
    };
    let (w1, inc1, m1, e1) = mk_entrant(1, "m1");
    let (_, _, _, e2) = mk_entrant(2, "m2");
    let genesis = Record::found(&new_seed, vec![e1], 1_000).expect("genesis");
    let admit = Log::new(genesis.clone())
        .unwrap()
        .draft(
            &m1,
            1_001,
            Body::Admit {
                entrant: e2,
                evidence: Evidence::Phrase,
            },
        )
        .sign(&w1, inc1)
        .unwrap();
    std::fs::create_dir_all(&member.dir).expect("secrets dir");
    crate::fsx::write_atomic(
        &member.records_file(),
        &encode_records([&genesis, &admit]),
        0o600,
    )
    .unwrap();

    assert!(
        load_records(&member).expect("load").log().is_none(),
        "a genesis the old pin never named rooted the store"
    );
    // The envelope's handoff moves the pin to the new key.
    pin(new, vec![old]);
    let held = load_records(&member).expect("load");
    assert!(
        held.log().is_some(),
        "a Mac one release behind refused the genesis its pinned key signed"
    );
    assert_eq!(held.complete_inventory().len(), 2);

    // A genesis signed by a key outside this Mac's line still binds nothing.
    let (_, _, _, e3) = mk_entrant(3, "m3");
    let made_up = Record::found(&[0x66; 32], vec![e3], 1_000).expect("genesis");
    std::fs::remove_file(member.records_file()).unwrap();
    assert_eq!(save_records(&member, made_up.bytes()).ok(), None);
    assert!(load_records(&member).unwrap().log().is_none());
}

#[test]
fn an_unequal_seq_parentless_partition_heals_preserving_lower_seq_entry_and_member() {
    // Every server it reads is its own on loopback, which answers.
    let _answering = ServersAnswering::set();
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let s1 = start_loopback_server(&root.path().join("s1"));
    let s2 = start_loopback_server(&root.path().join("s2"));
    let url1 = format!("http://{}/v1/team", s1.addr);
    let url2 = format!("http://{}/v1/team", s2.addr);

    let a1 = Paths::for_home(&root.path().join("a1"));
    create(&a1, &url1, None, Some("a1")).expect("create vault on s1");

    let b1 = Paths::for_home(&root.path().join("b1"));
    let b1_rec = machine_identity(&b1).unwrap().recipient().to_bech32();
    let b2 = Paths::for_home(&root.path().join("b2"));
    let b2_rec = machine_identity(&b2).unwrap().recipient().to_bech32();

    {
        let admin = admin_open(&a1).unwrap();
        let mut doc = admin.doc.clone();
        doc.add_machine("b1", &b1_rec).unwrap();
        doc.add_admin("b1").unwrap();
        doc.add_machine("b2", &b2_rec).unwrap();
        doc.set_entry(&Audience::Everyone, "base", b"v0").unwrap();
        admin_publish(&a1, &admin, &doc).expect("publish seq 2");
    }
    let seq2_bytes = std::fs::read(a1.cache_file()).unwrap();
    transport::publish(&Source::Url(url2.clone()), &seq2_bytes).expect("seed s2 at seq 2");

    let pin = config(&a1).unwrap().unwrap().signer.unwrap();
    let pin_b64 = crate::b64::encode_padded(&pin);
    join_host(&b1, &format!("{url2}#{pin_b64}"), "b1");
    join_host(&b2, &format!("{url2}#{pin_b64}"), "b2");

    let a3 = Paths::for_home(&root.path().join("a3"));
    let a3_rec = machine_identity(&a3).unwrap().recipient().to_bech32();
    let b3 = Paths::for_home(&root.path().join("b3"));
    let b3_rec = machine_identity(&b3).unwrap().recipient().to_bech32();

    // During the cut:
    // - Island A (`a1` on `s1`) advances to seq 5 (`f8-a-1`, `f8-a-2`, and admits `a3`).
    for (k, v) in [("f8-a-1", b"from-a-1".as_slice()), ("f8-a-2", b"from-a-2")] {
        let open = admin_open(&a1).unwrap();
        let mut doc = open.doc.clone();
        doc.set_entry(&Audience::Everyone, k, v).unwrap();
        admin_publish(&a1, &open, &doc).unwrap();
    }
    {
        let open = admin_open(&a1).unwrap();
        let mut doc = open.doc.clone();
        doc.add_machine("a3", &a3_rec).unwrap();
        let pub_a = admin_publish(&a1, &open, &doc).unwrap();
        assert_eq!(pub_a.seq, 5);
    }

    // - Island B (`b1` on `s2`) advances only to seq 4 (`f8-b-1` and admits `b3`).
    {
        let open = admin_open(&b1).unwrap();
        let mut doc = open.doc.clone();
        doc.set_entry(&Audience::Everyone, "f8-b-1", b"from-b-1")
            .unwrap();
        admin_publish(&b1, &open, &doc).unwrap();
    }
    {
        let open = admin_open(&b1).unwrap();
        let mut doc = open.doc.clone();
        doc.add_machine("b3", &b3_rec).unwrap();
        let pub_b = admin_publish(&b1, &open, &doc).unwrap();
        assert_eq!(pub_b.seq, 4);
    }

    join_host(&b3, &format!("{url2}#{pin_b64}"), "b3");
    assert!(matches!(
        sync(&b2, None).unwrap(),
        SyncOutcome::Applied { seq: 4, .. }
    ));
    assert_eq!(load_state(&b2).unwrap().membership, Stored::In);
    assert_eq!(load_state(&b3).unwrap().membership, Stored::In);

    // The cut heals, and non-admin subscribers `b2` and `b3` pass BEFORE any admin syncs:
    // - `b3` must not adopt `s1`'s unmerged seq 5 (which omits `b3`) and mark itself `Revoked`.
    // - `b2` must not adopt `s1`'s unmerged seq 5 (which omits `f8-b-1`) or `PutBack` seq 5 onto `s2`.
    let candidates = vec![url1.clone(), url2.clone()];
    for _ in 0..2 {
        let out_b3 = sync_with_candidates(&b3, None, &candidates).unwrap();
        assert!(
            matches!(out_b3, SyncOutcome::Unchanged { seq: 4 }),
            "b3 must stay on seq 4 until admin merges both islands, got {out_b3:?}"
        );
        assert_eq!(load_state(&b3).unwrap().membership, Stored::In);

        let out_b2 = sync_with_candidates(&b2, None, &candidates).unwrap();
        assert!(
            matches!(out_b2, SyncOutcome::Unchanged { seq: 4 }),
            "b2 must not adopt unmerged peer fork or overwrite s2, got {out_b2:?}"
        );
    }
    let s2_before_admin = match transport::fetch(
        &Source::Url(url2.clone()),
        None,
        aterm_spec::wait::MUST_HAPPEN,
    )
    .unwrap()
    {
        Fetched::Body(b) => envelope::parse(&b).unwrap(),
        other => panic!("expected s2 body, got {other:?}"),
    };
    assert_eq!(
        s2_before_admin.seq, 4,
        "s2 must still hold island B's seq 4 head before admin merge"
    );

    // Admin `b1` (or `a1`) syncs with candidates and merges the unequal-seq parentless split into seq 6.
    let healed = sync_with_candidates(&b1, None, &candidates).expect("admin heals unequal split");
    assert!(
        matches!(healed, SyncOutcome::Applied { seq: 6, .. }),
        "expected merged seq 6, got {healed:?}"
    );

    let heads = read_server_heads(&b1, &candidates).expect("read heads after heal");
    assert_eq!(heads.len(), 2);
    assert_eq!(heads[0].envelope.seq, 6);
    assert_eq!(heads[1].envelope.seq, 6);
    assert_eq!(heads[0].envelope.digest(), heads[1].envelope.digest());

    // Both `b2` and `b3` now adopt merged seq 6, remain `Stored::In`, and have all entries from both islands.
    assert!(matches!(
        sync_with_candidates(&b2, None, &candidates).unwrap(),
        SyncOutcome::Applied { seq: 6, .. }
    ));
    assert!(matches!(
        sync_with_candidates(&b3, None, &candidates).unwrap(),
        SyncOutcome::Applied { seq: 6, .. }
    ));
    assert_eq!(load_state(&b3).unwrap().membership, Stored::In);
    for host in [&b1, &b2, &b3] {
        assert_eq!(
            std::fs::read(host.home.join(".secrets/f8-a-1")).unwrap(),
            b"from-a-1"
        );
        assert_eq!(
            std::fs::read(host.home.join(".secrets/f8-a-2")).unwrap(),
            b"from-a-2"
        );
        assert_eq!(
            std::fs::read(host.home.join(".secrets/f8-b-1")).unwrap(),
            b"from-b-1"
        );
    }

    s1.stop();
    s2.stop();
}

/// When a server lacks records a Mac already holds (for example, a `POST /records` that failed
/// after a `PUT`, or a newly started `0.146` server with no `.records` yet), the server replies
/// with `X-Aterm-Records-Need: 1` on `GET /records?have=…` and `sync_records` re-posts the Mac's
/// local store without waiting for another publish or mirror.
#[test]
fn a_member_re_posts_held_records_when_a_server_lacks_its_frontier() {
    use crate::membership_log::{Body, Entrant, Evidence, Log, Record, encode_records};

    let root = aterm_tempfile::tempdir().expect("tempdir");
    let srv_dir = root.path().join("srv");
    std::fs::create_dir_all(&srv_dir).unwrap();
    let s1 = start_loopback_server(&srv_dir);
    let url = format!("http://{}/v1/team", s1.addr);

    let admin = Paths::for_home(&root.path().join("admin"));
    let member = Paths::for_home(&root.path().join("member"));
    create(&admin, &url, None, Some("m1")).expect("create team");

    let member_id = machine_identity(&member).expect("member identity");
    {
        let opened = admin_open(&admin).unwrap();
        let mut doc = opened.doc.clone();
        doc.add_machine("m2", &member_id.recipient().to_bech32())
            .unwrap();
        admin_publish(&admin, &opened, &doc).unwrap();
    }
    let pin = config(&admin).unwrap().unwrap().signer.unwrap();
    let pinned_url = format!("{url}#{}", crate::b64::encode_padded(&pin));
    join_host(&member, &pinned_url, "m2");

    let mk_entrant = |tag: u8, label: &str| {
        let writer = [tag; 32];
        let machine = [tag.wrapping_add(0x20); 16];
        let inc = [tag; 16];
        let identity = crate::age::Identity::from_secret_bytes([tag.wrapping_add(0x40); 32]);
        let req = crate::commons::binding::Request::create(
            [0x31; 16], [0x42; 32], machine, inc, &identity, &writer,
        )
        .unwrap();
        let actor = req.binding().actor();
        (
            writer,
            inc,
            actor,
            Entrant {
                request: req,
                node_key: [tag.wrapping_add(0x80); 32],
                name: format!("mac-{tag}"),
                label: label.to_owned(),
            },
        )
    };
    let (w1, inc1, m1_actor, e1) = mk_entrant(1, "m1");
    let (_, _, _, e2) = mk_entrant(2, "m2");
    let signer_seed = admin_open(&admin).expect("admin key").seed;
    let genesis = Record::found(&signer_seed, vec![e1], 1_000).expect("genesis");
    let admit_two = Log::new(genesis.clone())
        .unwrap()
        .draft(
            &m1_actor,
            1_001,
            Body::Admit {
                entrant: e2,
                evidence: Evidence::Phrase,
            },
        )
        .sign(&w1, inc1)
        .unwrap();

    // `admin` holds `{genesis, admit_two}` locally while `s1` holds only `{genesis}` (simulating a
    // failed `POST /records` after a publish).
    save_records(&admin, &encode_records([&genesis, &admit_two])).unwrap();
    transport::post_records(&url, genesis.bytes()).unwrap();
    assert_eq!(sync_records(&member, None).unwrap(), 1);
    assert!(!load_records(&member).unwrap().contains(admit_two.id()));

    // A regular `sync_records` pass on `admin` sees `X-Aterm-Records-Need: 1` and re-posts
    // `{genesis, admit_two}` to `s1`, so `member` receives `admit_two` on its next pass.
    assert_eq!(sync_records(&admin, None).unwrap(), 0);
    assert_eq!(sync_records(&member, None).unwrap(), 1);
    assert!(load_records(&member).unwrap().contains(admit_two.id()));

    s1.stop();
}
