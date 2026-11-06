// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::commons::binding::Pending;
use crate::commons::bootstrap::{Bootstrap, Device};
use crate::commons::journal::{Journal, State as JournalState, Trust};
use crate::membership_log::{
    Entrant, EpochAudience, Evidence, Kind, MemberId, Record, encode_records,
};
use crate::server::Options as ServerOptions;
use std::collections::VecDeque;

#[path = "coalesce_tests.rs"]
mod coalesce;

pub(super) fn join_host(paths: &Paths, source: &str, host: &str) {
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

/// The vault pass's ranked wrap-gap closer and epoch tender on `paths` alone
/// ([`remove::vault_pass_at`]), at first sight and then a thousand gap steps on, past every grace
/// and rank: what a Commons exchange's repair once signed at once.
pub(super) fn vault_passes(paths: &Paths) -> Vec<Record> {
    let now = aterm_time::unix_millis().unwrap_or(0);
    let mut signed = Vec::new();
    for at in [now, now + 1_000 * log_entries::GAP_STEP_MS] {
        signed.extend(remove::vault_pass_at(paths, at).expect("the vault pass looks"));
    }
    signed
}

pub(super) fn start_loopback_server(store: &Path) -> crate::server::Running {
    start_loopback_server_at(store, None)
}

fn start_loopback_server_at(store: &Path, clock: Option<fn() -> u64>) -> crate::server::Running {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let options = ServerOptions {
        listen: "127.0.0.1:0".parse().expect("addr"),
        store: store.to_path_buf(),
        advertise: None,
        pins: Vec::new(),
        hold: Duration::from_millis(100),
        keep_awake: false,
    };
    match clock {
        Some(clock) => crate::server::start_with_clock(listener, &options, clock),
        None => crate::server::start(listener, &options),
    }
    .expect("start server")
}

pub(super) fn enroll_commons(owner_paths: &Paths, member_paths: &Paths) {
    sync(member_paths, None).expect("sync member before commons enroll");
    let owner = Bootstrap::required(owner_paths).expect("owner bootstrap");
    let peer = Bootstrap::required(member_paths).expect("peer bootstrap");
    let request = peer.request().expect("peer request");
    let (mut pending, challenge) =
        Pending::issue(&request, owner.trust().network, owner.trust().authority)
            .expect("issue challenge");
    let answer = peer.device().answer(&request, &challenge).expect("answer");
    let mut journal = Journal::open_existing(owner.journal_dir(), owner.trust()).expect("journal");
    admin_cached(owner_paths)
        .expect("admin cached")
        .enroll_commons_member(
            &owner,
            &mut journal,
            pending.verify(&answer).expect("verify"),
        )
        .expect("enroll member");
}

pub(super) fn clone_identity_to(src: &Paths, dst: &Paths) {
    fsx::ensure_private_dir(&dst.dir).expect("dst dir");
    let id_bytes = std::fs::read(src.identity_file()).expect("read src identity");
    fsx::write_atomic(&dst.identity_file(), &id_bytes, 0o600).expect("write dst identity");
}

fn snapshot_materialized_secrets(home: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    let secrets_dir = home.join(".secrets");
    if let Ok(entries) = std::fs::read_dir(&secrets_dir) {
        for entry in entries.flatten() {
            if entry.file_type().is_ok_and(|t| t.is_file()) {
                let name = entry.file_name().to_string_lossy().into_owned();
                out.insert(name, std::fs::read(entry.path()).expect("read secret file"));
            }
        }
    }
    out
}

#[test]
fn a_mac_writes_the_same_files_from_the_log_as_from_the_envelope() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let m22 = Paths::for_home(&root.path().join("m22"));
    let vault_file = root.path().join("alab.vault");
    let created =
        test_create_unbound(&m22, vault_file.to_str().unwrap(), None, Some("m22")).unwrap();
    admin_keys::legacy::publish_legacy(&m22, &admin_open(&m22).unwrap());
    admin_commons_init(&m22).expect("commons init");

    let pinned = format!("{}#{}", vault_file.display(), created.signer);
    let lineage = decode_key32(&created.signer).expect("lineage");

    let m7_env = Paths::for_home(&root.path().join("m7-env"));
    let m7_rec = machine_identity(&m7_env).unwrap().recipient().to_bech32();
    {
        let admin = admin_open(&m22).unwrap();
        let mut doc = admin.doc.clone();
        doc.add_machine("m7", &m7_rec).unwrap();
        doc.set_entry(&Audience::Everyone, "shared-api-key", b"sk-shared-01")
            .unwrap();
        doc.set_entry(&Audience::Everyone, "override-me", b"from-everyone")
            .unwrap();
        doc.set_entry_with(
            &Audience::Everyone,
            "seed-cfg",
            b"default-seed\n",
            true,
            None,
        )
        .unwrap();
        doc.set_entry_with(
            &Audience::Everyone,
            "anchored-cfg",
            b"linked-to-seed\n",
            true,
            Some("seed-cfg"),
        )
        .unwrap();
        doc.set_entry(
            &Audience::Hosts(vec!["m7".to_owned()]),
            "override-me",
            b"from-m7-host",
        )
        .unwrap();
        doc.set_entry(
            &Audience::Hosts(vec!["m7".to_owned()]),
            "m7-only",
            b"private-m7",
        )
        .unwrap();
        doc.set_entry(
            &Audience::Hosts(vec!["m22".to_owned()]),
            "m22-only",
            b"private-m22",
        )
        .unwrap();
        admin_publish_approved(&m22, &admin, &doc, std::slice::from_ref(&m7_rec)).unwrap();
    }

    join_host(&m7_env, &pinned, "m7");
    enroll_commons(&m22, &m7_env);

    // Record everything `m7_env` materialized from the envelope BEFORE Genesis v2 seeds the log.
    let env_files = snapshot_materialized_secrets(&m7_env.home);
    let env_state = load_state(&m7_env).unwrap();
    let env_sight = sight(&m7_env).unwrap().expect("envelope sight");
    let env_commons = match observe_commons(&m7_env).unwrap() {
        crate::commons::Observation::Visible(snap) => snap.control().to_vec(),
        other => panic!("expected visible commons on envelope reader, got {other:?}"),
    };
    let env_cfg = config(&m7_env).unwrap().unwrap();
    let env_verified = read_verified_entry(&m7_env, &env_cfg, "override-me")
        .unwrap()
        .expect("verified override-me");
    assert_eq!(env_verified.value().as_slice(), b"from-m7-host");
    assert!(env_state.records_digest.is_none());

    // Sign Genesis v2 on m22: `alab.genesis` holds the single Genesis record, while `m22`'s
    // `.records` store holds the seeded Epochs, Wraps and Puts.
    let genesis_out = root.path().join("alab.genesis");
    let signed = genesis::sign_genesis(&m22, &genesis_out, &lineage, true).expect("sign genesis");
    let genesis_rec = Record::decode(&std::fs::read(&genesis_out).unwrap()).unwrap();
    assert_eq!(*genesis_rec.id(), signed.digest);

    // m22 publishes the same text again: a version stamped with the seeded log's heads, which a
    // Mac whose log holds them reads from the log.
    {
        let admin = admin_open(&m22).unwrap();
        admin_publish(&m22, &admin, &admin.doc.clone()).expect("stamped version");
    }
    let full_store = load_records(&m22).expect("load seeded store");
    let full_log = full_store.log().expect("seeded log");
    assert!(full_log.has_epochs());

    // Clone m7's identity onto a fresh Mac (`m7_log`), give it the seeded `.records` store, and
    // join: it materializes from the log and produces byte-identical files, manifest, sight,
    // Commons observation, and `read_verified_entry`!
    let m7_log = Paths::for_home(&root.path().join("m7-log"));
    clone_identity_to(&m7_env, &m7_log);
    fsx::write_atomic(&m7_log.records_file(), &full_store.encode(), 0o600).unwrap();
    join_host(&m7_log, &pinned, "m7");

    let log_files = snapshot_materialized_secrets(&m7_log.home);
    let log_state = load_state(&m7_log).unwrap();
    let log_sight = sight(&m7_log).unwrap().expect("log sight");
    let log_commons = match observe_commons(&m7_log).unwrap() {
        crate::commons::Observation::Visible(snap) => snap.control().to_vec(),
        other => panic!("expected visible commons on log reader, got {other:?}"),
    };
    let log_cfg = config(&m7_log).unwrap().unwrap();
    let log_verified = read_verified_entry(&m7_log, &log_cfg, "override-me")
        .unwrap()
        .expect("verified override-me from log");

    assert!(
        log_state.records_digest.is_some(),
        "log-backed sync must record records_digest"
    );
    assert_eq!(log_files, env_files);
    assert_eq!(log_state.manifest.files, env_state.manifest.files);
    assert_eq!(log_state.own, env_state.own);
    assert_eq!(log_sight.held, env_sight.held);
    assert_eq!(log_sight.hidden, env_sight.hidden);
    assert_eq!(log_commons, env_commons);
    assert_eq!(
        log_verified.value().as_slice(),
        env_verified.value().as_slice()
    );

    // Negative control: a Put only the log holds (drafted on m22 after the stamped version, never
    // published in an envelope). A Mac whose log holds it writes its value, which the envelope
    // never carried: the files above came from the log.
    let mut edited = admin_open(&m22).unwrap().doc.clone();
    edited
        .set_entry(&Audience::Everyone, "shared-api-key", b"sk-log-only-02")
        .unwrap();
    let cfg = config(&m22).unwrap().unwrap();
    let cached = envelope::parse(&std::fs::read(m22.cache_file()).unwrap()).unwrap();
    let drafted = log_entries::draft_document_diff(
        &m22,
        &cfg,
        &cached.signer,
        &cached.handoff,
        &read_identity(&m22).unwrap().unwrap(),
        &edited,
        log_entries::Base::default(),
    )
    .unwrap()
    .expect("m22 writes the log");
    assert!(!drafted.records.is_empty());
    let mut log_only = load_records(&m22).unwrap();
    log_only
        .ingest(&encode_records(&drafted.records))
        .expect("the log-only Put joins m22's records");
    let m7_ahead = Paths::for_home(&root.path().join("m7-ahead"));
    clone_identity_to(&m7_env, &m7_ahead);
    fsx::write_atomic(&m7_ahead.records_file(), &log_only.encode(), 0o600).unwrap();
    join_host(&m7_ahead, &pinned, "m7");
    let ahead_files = snapshot_materialized_secrets(&m7_ahead.home);
    assert_eq!(
        ahead_files.get("shared-api-key").map(Vec::as_slice),
        Some(b"sk-log-only-02".as_slice())
    );
}

#[test]
fn concurrent_puts_both_survive_and_every_mac_writes_the_same_one() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let s1 = start_loopback_server(&root.path().join("s1"));
    let url = format!("http://{}/v1/alab", s1.addr);

    let m22 = Paths::for_home(&root.path().join("m22"));
    let created = test_create_unbound(&m22, &url, None, Some("m22")).unwrap();
    admin_keys::legacy::publish_legacy(&m22, &admin_open(&m22).unwrap());
    admin_commons_init(&m22).expect("commons init");
    let lineage = decode_key32(&created.signer).unwrap();
    let pinned = format!("{url}#{}", created.signer);

    let m7 = Paths::for_home(&root.path().join("m7"));
    let m7_rec = machine_identity(&m7).unwrap().recipient().to_bech32();
    {
        let admin = admin_open(&m22).unwrap();
        let mut doc = admin.doc.clone();
        doc.add_machine("m7", &m7_rec).unwrap();
        doc.set_entry(&Audience::Everyone, "race-token", b"initial")
            .unwrap();
        admin_publish_approved(&m22, &admin, &doc, std::slice::from_ref(&m7_rec)).unwrap();
    }
    join_host(&m7, &pinned, "m7");
    enroll_commons(&m22, &m7);

    let genesis_out = root.path().join("alab.genesis");
    genesis::sign_genesis(&m22, &genesis_out, &lineage, true).expect("sign genesis");
    sync(&m7, None).expect("m7 syncs initial seeded log");
    // A version stamped with the seeded log's heads, which a Mac whose log holds them reads from
    // the log (the version before Genesis v2 names none, and is read from its lockers).
    {
        let admin = admin_open(&m22).unwrap();
        admin_publish(&m22, &admin, &admin.doc.clone()).expect("stamped version");
    }
    sync(&m7, None).expect("m7 applies the stamped version");

    let base_store = load_records(&m22).unwrap();
    let base_log = base_store.log().unwrap().clone();
    let m22_id = read_identity(&m22).unwrap().unwrap();

    let opened_m22 = base_log.open_audience_entries(&m22_id).unwrap().unwrap();
    let everyone_ep = opened_m22.maximal_epochs[&EpochAudience::Everyone][0];
    let everyone_key: [u8; 32] = *opened_m22.unwrapped_keys[&everyone_ep];

    let b22 = Bootstrap::required(&m22).unwrap();
    let r22 = b22.request().unwrap();
    let a22 = r22.binding().actor();
    let i22 = *r22.binding().incarnation();
    let put_m22 = b22
        .device()
        .with_seed(|seed| {
            base_log
                .draft_put(
                    &a22,
                    2_000,
                    everyone_ep,
                    &everyone_key,
                    &Entry {
                        name: "race-token".to_owned(),
                        value: SecretBytes::from(b"from-m22".as_slice()),
                        audience: Audience::Everyone,
                        if_absent: false,
                        anchor: None,
                    },
                )
                .unwrap()
                .sign(seed, i22)
        })
        .unwrap();

    let b7 = Bootstrap::required(&m7).unwrap();
    let r7 = b7.request().unwrap();
    let a7 = r7.binding().actor();
    let i7 = *r7.binding().incarnation();
    let put_m7 = b7
        .device()
        .with_seed(|seed| {
            base_log
                .draft_put(
                    &a7,
                    2_001,
                    everyone_ep,
                    &everyone_key,
                    &Entry {
                        name: "race-token".to_owned(),
                        value: SecretBytes::from(b"from-m7".as_slice()),
                        audience: Audience::Everyone,
                        if_absent: false,
                        anchor: None,
                    },
                )
                .unwrap()
                .sign(seed, i7)
        })
        .unwrap();

    // Negative control ("Order by arrival"): choosing the last-arrived write makes two Macs that
    // hear `[put_m22, put_m7]` vs `[put_m7, put_m22]` materialize different values!
    let arrival_last = |order: &[(&Record, &[u8])]| -> Vec<u8> { order.last().unwrap().1.to_vec() };
    let by_arrival_on_m22 = arrival_last(&[(&put_m22, b"from-m22"), (&put_m7, b"from-m7")]);
    let by_arrival_on_m7 = arrival_last(&[(&put_m7, b"from-m7"), (&put_m22, b"from-m22")]);
    assert_ne!(
        by_arrival_on_m22, by_arrival_on_m7,
        "negative control: arrival ordering diverges across opposite delivery orders"
    );

    // Deliver in opposite orders locally BEFORE talking to the server, then materialize on each:
    save_records(&m22, put_m22.bytes()).unwrap();
    save_records(&m22, put_m7.bytes()).unwrap();
    save_records(&m7, put_m7.bytes()).unwrap();
    save_records(&m7, put_m22.bytes()).unwrap();

    // Post both to server so envelope fetch sees no conflict and both Macs materialize from log.
    transport::post_records(&url, &encode_records([&put_m22, &put_m7])).unwrap();

    // Both sibling Puts survive in each Mac's store and `conflicts` flags `race-token`.
    for paths in [&m22, &m7] {
        let store = load_records(paths).unwrap();
        assert!(store.contains(put_m22.id()));
        assert!(store.contains(put_m7.id()));
        let opened = store
            .log()
            .unwrap()
            .open_entry_set(&read_identity(paths).unwrap().unwrap())
            .unwrap()
            .unwrap();
        assert!(
            opened
                .conflicts
                .contains(&(Audience::Everyone, "race-token".to_owned()))
        );
    }

    // BOTH Macs write the exact value of the lower `RecordId`: m7 from the log, and m22, an admin
    // whose log moved, by mirroring the merged log (which leaves both values candidates).
    let expected_winner: &[u8] = if put_m22.id() < put_m7.id() {
        b"from-m22"
    } else {
        b"from-m7"
    };
    sync(&m7, None).expect("m7 materializes concurrent puts");
    sync(&m22, None).expect("m22 materializes concurrent puts");
    sync(&m7, None).expect("m7 applies m22's mirror");
    for paths in [&m22, &m7] {
        let disk = std::fs::read(paths.home.join(".secrets/race-token")).unwrap();
        assert_eq!(disk.as_slice(), expected_winner);
    }

    // One subsequent save whose parents include both concurrent heads resolves the conflict!
    let admin = admin_open(&m22).unwrap();
    let mut resolved_doc = admin.doc.clone();
    resolved_doc
        .set_entry(&Audience::Everyone, "race-token", b"resolved-once")
        .unwrap();
    admin_publish(&m22, &admin, &resolved_doc).expect("publish conflict resolution");
    sync(&m7, None).expect("m7 syncs resolved value");

    for paths in [&m22, &m7] {
        let store = load_records(paths).unwrap();
        let opened = store
            .log()
            .unwrap()
            .open_entry_set(&read_identity(paths).unwrap().unwrap())
            .unwrap()
            .unwrap();
        assert!(opened.conflicts.is_empty());
        let disk = std::fs::read(paths.home.join(".secrets/race-token")).unwrap();
        assert_eq!(disk.as_slice(), b"resolved-once");
    }

    s1.stop();
}

#[test]
fn an_entry_edited_on_the_lower_seq_side_of_a_split_survives_the_heal() {
    // Every server it reads is its own on loopback, which answers.
    let _answering = ServersAnswering::set();
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let s1 = start_loopback_server(&root.path().join("s1"));
    let s2 = start_loopback_server(&root.path().join("s2"));
    let url1 = format!("http://{}/v1/alab", s1.addr);
    let url2 = format!("http://{}/v1/alab", s2.addr);

    let st1 = Paths::for_home(&root.path().join("st1"));
    let created = test_create_unbound(&st1, &url1, None, Some("m22")).unwrap();
    admin_keys::legacy::publish_legacy(&st1, &admin_open(&st1).unwrap());
    admin_commons_init(&st1).expect("commons init on st1");
    let lineage = decode_key32(&created.signer).unwrap();
    let pin_b64 = created.signer.clone();

    let st2 = Paths::for_home(&root.path().join("st2"));
    let st2_rec = machine_identity(&st2).unwrap().recipient().to_bech32();
    let m_v2 = Paths::for_home(&root.path().join("m8"));
    let m_v2_rec = machine_identity(&m_v2).unwrap().recipient().to_bech32();
    let m_0145_id = Identity::generate().unwrap();
    let m_0145_rec = m_0145_id.recipient().to_bech32();

    {
        let a1 = admin_open(&st1).unwrap();
        let mut doc = a1.doc.clone();
        doc.add_machine("m7", &st2_rec).unwrap();
        doc.add_admin("m7").unwrap();
        doc.add_machine("m8", &m_v2_rec).unwrap();
        doc.add_machine("m9", &m_0145_rec).unwrap();
        doc.set_entry(&Audience::Everyone, "split-key", b"v0")
            .unwrap();
        doc.set_entry(&Audience::Everyone, "other-key", b"v0")
            .unwrap();
        admin_publish_admin(&st1, &a1, &doc, "m7").unwrap();
    }
    {
        let a1 = admin_open(&st1).unwrap();
        admin_publish_approved(&st1, &a1, &a1.doc, std::slice::from_ref(&m_v2_rec)).unwrap();
    }

    // Replicate pre-split envelope to s2, join st2 (on s2) and m_v2 (on s1), enroll Commons, and
    // sign Genesis v2!
    let pre_split_env = std::fs::read(st1.cache_file()).unwrap();
    transport::publish(&Source::Url(url2.clone()), &pre_split_env).expect("seed s2 envelope");
    join_host(&st2, &format!("{url2}#{pin_b64}"), "m7");
    join_host(&m_v2, &format!("{url1}#{pin_b64}"), "m8");
    enroll_commons(&st1, &st2);
    enroll_commons(&st1, &m_v2);

    let genesis_out = root.path().join("alab.genesis");
    genesis::sign_genesis(&st1, &genesis_out, &lineage, true).expect("sign genesis");
    let seeded_wire = load_records(&st1).unwrap().encode();
    transport::post_records(&url2, &seeded_wire).expect("seed s2 records");
    sync(&st2, None).expect("st2 syncs seeded log");
    sync(&m_v2, None).expect("m_v2 syncs seeded log");

    let base_seq = load_state(&st1).unwrap().seq;
    assert_eq!(load_state(&st2).unwrap().seq, base_seq);

    // --- PARTITION ---
    // Island 1 (s1, lower seq = base_seq + 1): `st1` edits `split-key` -> `v1-from-lower-seq`
    // and advances `aterm-swarm-lease` to epoch 99.
    let admin_aud = Audience::Hosts(vec!["m22".to_owned(), "m7".to_owned()]);
    let lease_high = crate::swarm_roster::SwarmLease {
        holder: "m22".to_owned(),
        epoch: 99,
        until: 9_000,
    };
    let lease_low = crate::swarm_roster::SwarmLease {
        holder: "m7".to_owned(),
        epoch: 10,
        until: 5_000,
    };
    let s1_seq = {
        let a1 = admin_open(&st1).unwrap();
        let mut d1 = a1.doc.clone();
        d1.set_entry(&Audience::Everyone, "split-key", b"v1-from-lower-seq")
            .unwrap();
        d1.set_entry(
            &admin_aud,
            crate::SWARM_LEASE_ENTRY,
            lease_high.to_toml().as_bytes(),
        )
        .unwrap();
        let p = admin_publish(&st1, &a1, &d1).expect("st1 edits split-key on s1");
        assert_eq!(p.seq, base_seq + 1);
        p.seq
    };

    // Island 2 (s2, higher seq = base_seq + 3): `st2` edits `other-key` three times (and writes a
    // lower lease epoch 10) so `s2.seq > s1.seq`!
    for step in [
        b"v1".as_slice(),
        b"v2".as_slice(),
        b"v3-from-higher-seq".as_slice(),
    ] {
        let a2 = admin_open(&st2).unwrap();
        let mut d2 = a2.doc.clone();
        d2.set_entry(&Audience::Everyone, "other-key", step)
            .unwrap();
        d2.set_entry(
            &admin_aud,
            crate::SWARM_LEASE_ENTRY,
            lease_low.to_toml().as_bytes(),
        )
        .unwrap();
        admin_publish(&st2, &a2, &d2).expect("st2 edits other-key on s2");
    }
    let s2_seq = load_state(&st2).unwrap().seq;
    assert_eq!(s2_seq, base_seq + 3);
    assert!(s2_seq > s1_seq);

    // Negative control ("C7's merge drops the lower-seq edit"): C7's envelope-only
    // `merge_diverged_heads` chooses `s2` (`seq = 6`) as primary and drops `s1`'s edit of
    // `split-key`, reverting it to `v0`!
    let diverged_heads = read_server_heads(&st1, &[url1.clone(), url2.clone()]).unwrap();
    assert_eq!(diverged_heads.len(), 2);
    let c7_merged = merge_diverged_heads(&diverged_heads).unwrap();
    assert_eq!(
        c7_merged
            .doc
            .entry(&Audience::Everyone, "split-key")
            .unwrap()
            .value
            .as_slice(),
        b"v0",
        "negative control: C7 envelope merge drops lower-seq edit to an existing entry"
    );

    // --- HEAL ---
    // 1. Even before any admin rebuilds the envelope mirror, v2 member `m_v2` unions `.records`
    //    across `[url1, url2]` and materializes BOTH islands' edits!
    sync_with_candidates(&m_v2, None, &[url1.clone(), url2.clone()])
        .expect("m_v2 syncs unioned log across heal");
    assert_eq!(
        std::fs::read(m_v2.home.join(".secrets/split-key")).unwrap(),
        b"v1-from-lower-seq"
    );
    assert_eq!(
        std::fs::read(m_v2.home.join(".secrets/other-key")).unwrap(),
        b"v3-from-higher-seq"
    );
    // An F10 handover bundle built by `m_v2` before any envelope mirror reads `[everyone]`
    // entries from the membership log (returning `other-key = v3-from-higher-seq` even though
    // `m_v2`'s cached envelope from `s1` still holds `other-key = v0`).
    let handover_paths = Paths::for_home(&root.path().join("m-handover"));
    machine_identity(&handover_paths).unwrap();
    let newcomer_req = crate::commons::bootstrap::Device::open(&handover_paths)
        .unwrap()
        .unwrap()
        .request([1; 16], [2; 32])
        .unwrap();
    let sample_allowed = Bootstrap::required(&m_v2)
        .unwrap()
        .device()
        .with_seed(|seed| crate::members::Allowed::sign(newcomer_req.binding(), "m10", seed))
        .unwrap();
    let ho_bundle = handover_bundle(&m_v2, &sample_allowed).expect("handover bundle from log");
    let ho_entries: BTreeMap<String, Vec<u8>> =
        crate::text::parse_locker(ho_bundle.files().expect("files").as_slice())
            .unwrap()
            .into_iter()
            .map(|e| (e.name, e.value.as_slice().to_vec()))
            .collect();
    assert_eq!(
        ho_entries.get("split-key").map(Vec::as_slice),
        Some(b"v1-from-lower-seq".as_slice())
    );
    assert_eq!(
        ho_entries.get("other-key").map(Vec::as_slice),
        Some(b"v3-from-higher-seq".as_slice())
    );

    // 2. Admin `st2` mirrors the merged log back into a parentless (`F1`) envelope at `s2_seq + 1`
    //    on both servers so a `0.145` Mac reading ONLY the envelope also receives the lower-seq edit!
    let mirrored_seq = admin_mirror_log(&st2, &[url1.clone(), url2.clone()])
        .expect("admin_mirror_log")
        .expect("mirror published");
    assert_eq!(mirrored_seq, s2_seq + 1);

    for url in [&url1, &url2] {
        let Fetched::Body(served) = transport::fetch(
            &Source::Url(url.clone()),
            None,
            aterm_spec::wait::MUST_HAPPEN,
        )
        .unwrap() else {
            panic!("expected served mirror envelope on {url}");
        };
        let raw_text = std::str::from_utf8(&served).expect("envelope text");
        assert!(
            !raw_text.contains("\nparent:"),
            "F1: mirrored envelope must carry no parent: header line"
        );
        let parsed = envelope::parse(&served).unwrap();
        assert_eq!(parsed.seq, s2_seq + 1);
        assert!(parsed.parents.is_empty());

        let opened_0145 = open_lockers(&parsed, &m_0145_id)
            .unwrap()
            .expect("0.145 member opens envelope lockers");
        let by_name: BTreeMap<String, Vec<u8>> = opened_0145
            .into_iter()
            .map(|e| (e.name, e.value.as_slice().to_vec()))
            .collect();
        assert_eq!(
            by_name.get("split-key").map(Vec::as_slice),
            Some(b"v1-from-lower-seq".as_slice())
        );
        assert_eq!(
            by_name.get("other-key").map(Vec::as_slice),
            Some(b"v3-from-higher-seq".as_slice())
        );
    }

    // Verify all records on both servers are Kind::Genesis/Epoch/Wrap/Put/Delete, converge with no
    // remaining conflicts, and preserve the higher steward lease epoch (99) after mirror heal.
    let healed_admin = admin_open(&st2).unwrap();
    let healed_lease = crate::swarm_roster::SwarmLease::from_document(&healed_admin.doc)
        .unwrap()
        .expect("healed lease");
    assert_eq!(healed_lease.epoch, 99);
    let s1_rec =
        transport::fetch_records(&url1, &[], None, aterm_spec::wait::MUST_HAPPEN, None).unwrap();
    let s2_rec =
        transport::fetch_records(&url2, &[], None, aterm_spec::wait::MUST_HAPPEN, None).unwrap();
    let st_s1 = crate::membership_log::Store::decode("custom", &s1_rec).unwrap();
    let st_s2 = crate::membership_log::Store::decode("custom", &s2_rec).unwrap();
    assert_eq!(st_s1.inventory(), st_s2.inventory());
    let healed_opened = st_s1
        .log()
        .unwrap()
        .open_entry_set(&read_identity(&st2).unwrap().unwrap())
        .unwrap()
        .unwrap();
    assert!(healed_opened.conflicts.is_empty());
    assert!(
        st_s1
            .log()
            .unwrap()
            .records()
            .any(|r| r.kind() == Kind::Put)
    );

    s1.stop();
    s2.stop();
}

/// Copy the tree at `src` to `dst`, each directory private and each file `0600`: a home or a
/// journal a replay starts from.
pub(super) fn copy_private_tree(src: &Path, dst: &Path) {
    fsx::ensure_private_dir(dst).expect("ensure dst dir");
    for entry in std::fs::read_dir(src).expect("read src dir") {
        let entry = entry.expect("dir entry");
        let ty = entry.file_type().expect("file type");
        let target = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_private_tree(&entry.path(), &target);
        } else if ty.is_file() {
            let bytes = std::fs::read(entry.path()).expect("read file");
            fsx::write_atomic(&target, &bytes, 0o600).expect("write file");
        }
    }
}

struct CommonsLogFixture {
    template: PathBuf,
    trust: Trust,
    actor_b: MemberId,
    actor_c: MemberId,
    actor_d: MemberId,
    rec_d: Recipient,
    entrant_d: Entrant,
    fork_right: Vec<u8>,
}

impl CommonsLogFixture {
    fn build(base: &Path) -> Self {
        let template = base.join("template");
        fsx::ensure_private_dir(&template).unwrap();
        let vault_file = base.join("commons-model.vault");

        let m1 = Paths::for_home(&template.join("m1"));
        let created =
            test_create_unbound(&m1, vault_file.to_str().unwrap(), None, Some("m1")).unwrap();
        admin_keys::legacy::publish_legacy(&m1, &admin_open(&m1).unwrap());
        admin_commons_init(&m1).expect("commons init");
        let pinned = format!("{}#{}", vault_file.display(), created.signer);
        let lineage = decode_key32(&created.signer).expect("lineage");

        let m2 = Paths::for_home(&template.join("m2"));
        let m3 = Paths::for_home(&template.join("m3"));
        let m4 = Paths::for_home(&template.join("m4"));
        let r2 = machine_identity(&m2).unwrap().recipient().to_bech32();
        let r3 = machine_identity(&m3).unwrap().recipient().to_bech32();
        let r4 = machine_identity(&m4).unwrap().recipient().to_bech32();
        {
            let admin = admin_open(&m1).unwrap();
            let mut doc = admin.doc.clone();
            doc.add_machine("m2", &r2).unwrap();
            doc.add_machine("m3", &r3).unwrap();
            doc.add_machine("m4", &r4).unwrap();
            doc.set_entry(&Audience::Everyone, "shared", b"v0").unwrap();
            admin_publish_approved(&m1, &admin, &doc, &[r2.clone(), r3.clone(), r4.clone()])
                .unwrap();
        }
        for (p, h) in [(&m2, "m2"), (&m3, "m3"), (&m4, "m4")] {
            join_host(p, &pinned, h);
        }
        enroll_commons(&m1, &m2);
        enroll_commons(&m1, &m3);

        // Mint an alternate v1 authority update branching off `[genesis, m2, m3]` before `m4` is
        // enrolled on `m1`'s main journal so `fork_right` conflicts with `m4`'s enrollment node.
        let b1 = Bootstrap::required(&m1).unwrap();
        let trust = b1.trust();
        let alt_j_dir = base.join("alt-journal");
        copy_private_tree(b1.journal_dir(), &alt_j_dir);
        let fork_right = {
            let b4 = Bootstrap::required(&m4).unwrap();
            let req4 = b4.request().unwrap();
            let (mut pending, ch) = Pending::issue(&req4, trust.network, trust.authority).unwrap();
            let ans = b4.device().answer(&req4, &ch).unwrap();
            let mut alt_j = Journal::open_existing(&alt_j_dir, trust).unwrap();
            admin_cached(&m1)
                .unwrap()
                .enroll_commons_member(&b1, &mut alt_j, pending.verify(&ans).unwrap())
                .unwrap()
                .bytes()
                .to_vec()
        };

        enroll_commons(&m1, &m4);
        for p in [&m2, &m3, &m4] {
            sync(p, None).unwrap();
        }
        let j1_nodes: Vec<Vec<u8>> = {
            let j1 = Journal::read_existing(b1.journal_dir(), trust).unwrap();
            j1.inventory()
                .filter_map(|id| j1.node(id).map(|n| n.bytes().to_vec()))
                .collect()
        };
        for p in [&m2, &m3, &m4] {
            let bp = Bootstrap::required(p).unwrap();
            let mut jp = Journal::open_existing(bp.journal_dir(), trust).unwrap();
            for wire in &j1_nodes {
                let _ = jp.ingest(wire).unwrap();
            }
        }

        let genesis_out = base.join("commons-model.genesis");
        genesis::sign_genesis(&m1, &genesis_out, &lineage, true).expect("sign genesis");
        let seeded_wire = load_records(&m1).unwrap().encode();
        for p in [&m2, &m3, &m4] {
            save_records(p, &seeded_wire).unwrap();
            assert!(Bootstrap::required(p).unwrap().access().unwrap().is_some());
        }

        let actor_b = Bootstrap::required(&m1)
            .unwrap()
            .request()
            .unwrap()
            .binding()
            .actor();
        let actor_c = Bootstrap::required(&m3)
            .unwrap()
            .request()
            .unwrap()
            .binding()
            .actor();

        let m5 = Paths::for_home(&template.join("m5"));
        super::log_entries::configure_log_member(&m5, &pinned, "m5").unwrap();
        let id_d = machine_identity(&m5).unwrap();
        let rec_d = id_d.recipient();
        let dev_d = Device::open(&m5).unwrap().unwrap();
        let req_d = dev_d.request(trust.network, trust.authority).unwrap();
        let actor_d = req_d.binding().actor();
        let entrant_d = Entrant {
            request: req_d,
            node_key: [0x55; 32],
            name: "m5".to_owned(),
            label: "m5".to_owned(),
        };

        Self {
            template,
            trust,
            actor_b,
            actor_c,
            actor_d,
            rec_d,
            entrant_d,
            fork_right,
        }
    }
}

struct CommonsLogHarness<'a> {
    fixture: &'a CommonsLogFixture,
    m1: Paths,
    m2: Paths,
    m3: Paths,
    m4: Paths,
    m5: Paths,
    merged: bool,
    reconciled: bool,
}

impl<'a> CommonsLogHarness<'a> {
    fn replay(fixture: &'a CommonsLogFixture, run_dir: &Path, path: &[&str]) -> Self {
        copy_private_tree(&fixture.template, run_dir);
        let mut h = Self {
            fixture,
            m1: Paths::for_home(&run_dir.join("m1")),
            m2: Paths::for_home(&run_dir.join("m2")),
            m3: Paths::for_home(&run_dir.join("m3")),
            m4: Paths::for_home(&run_dir.join("m4")),
            m5: Paths::for_home(&run_dir.join("m5")),
            merged: false,
            reconciled: false,
        };
        for action in path {
            h.apply(action);
        }
        h
    }

    fn apply(&mut self, action: &str) {
        match action {
            "AdmitDOnIslandA" => {
                admit_member_in_log(&self.m2, self.fixture.entrant_d.clone(), Evidence::Phrase)
                    .expect("m2 admits m5");
                let wire = load_records(&self.m2).unwrap().encode();
                save_records(&self.m1, &wire).unwrap();
                save_records(&self.m5, &wire).unwrap();
            }
            "RemoveBOnIslandA" => {
                remove_member_in_log(&self.m2, self.fixture.actor_b).expect("m2 removes m1");
                let wire = load_records(&self.m2).unwrap().encode();
                if load_records(&self.m2)
                    .unwrap()
                    .log()
                    .unwrap()
                    .membership()
                    .members()
                    .contains_key(&self.fixture.actor_d)
                {
                    save_records(&self.m5, &wire).unwrap();
                }
            }
            "RemoveCOnIslandB" => {
                remove_member_in_log(&self.m4, self.fixture.actor_c).expect("m4 removes m3");
                let wire = load_records(&self.m4).unwrap().encode();
                save_records(&self.m3, &wire).unwrap();
            }
            "ForkLegacyJournal" => {
                for p in [&self.m2, &self.m4] {
                    let bp = Bootstrap::required(p).unwrap();
                    let mut jp = Journal::open_existing(bp.journal_dir(), self.fixture.trust)
                        .expect("open journal");
                    let _ = jp.ingest(&self.fixture.fork_right).unwrap();
                    assert!(matches!(jp.state(), JournalState::Conflict { .. }));
                }
            }
            "MergeIslands" => {
                let wire_a = load_records(&self.m2).unwrap().encode();
                let wire_b = load_records(&self.m4).unwrap().encode();
                for p in [&self.m2, &self.m3, &self.m4] {
                    save_records(p, &wire_a).unwrap();
                    save_records(p, &wire_b).unwrap();
                }
                if load_records(&self.m2)
                    .unwrap()
                    .log()
                    .unwrap()
                    .membership()
                    .members()
                    .contains_key(&self.fixture.actor_d)
                {
                    save_records(&self.m5, &wire_a).unwrap();
                    save_records(&self.m5, &wire_b).unwrap();
                }
                self.merged = true;
            }
            "Reconcile" => {
                // The members' vault passes after the heal (a Commons exchange signs nothing):
                // each Mac's ranked closer and tender at first sight, then past every grace and
                // rank, every Mac's records carried to every other between passes.
                let macs = [&self.m1, &self.m2, &self.m3, &self.m4, &self.m5];
                let carry = || {
                    for from in macs {
                        let wire = load_records(from).unwrap().encode();
                        for to in macs {
                            if to.home != from.home && load_records(to).unwrap().log().is_some() {
                                save_records(to, &wire).unwrap();
                            }
                        }
                    }
                };
                let now = aterm_time::unix_millis().unwrap_or(0);
                for at in [now, now + 1_000 * log_entries::GAP_STEP_MS] {
                    for p in macs {
                        remove::vault_pass_at(p, at).expect("the vault pass looks");
                        carry();
                    }
                }
                self.reconciled = true;
            }
            other => panic!("unexpected CommonsLog action: {other}"),
        }
    }

    fn observe(&self) -> aterm_spec::interp::State {
        let store_a = load_records(&self.m2).unwrap();
        let log_a = store_a.log().unwrap();
        let store_b = load_records(&self.m4).unwrap();
        let log_b = store_b.log().unwrap();

        let admitted_d = log_a
            .membership()
            .members()
            .contains_key(&self.fixture.actor_d);
        let removed_b = log_a.membership().removal(&self.fixture.actor_b).is_some();
        let removed_c = log_b.membership().removal(&self.fixture.actor_c).is_some();

        let b4 = Bootstrap::required(&self.m4).unwrap();
        let j4 = Journal::read_existing(b4.journal_dir(), self.fixture.trust).unwrap();
        let journal_fork = matches!(j4.state(), JournalState::Conflict { .. });

        let id_m2 = read_identity(&self.m2).unwrap().unwrap();
        let opened_m2 = log_a.open_audience_entries(&id_m2).unwrap().unwrap();
        let max_eps = opened_m2
            .maximal_epochs
            .get(&EpochAudience::Commons)
            .cloned()
            .unwrap_or_default();
        let epoch_heads = max_eps.len() as i64;

        let d_wrapped_current = admitted_d
            && !max_eps.is_empty()
            && max_eps.iter().all(|ep| {
                opened_m2
                    .wrapped_recipients
                    .get(ep)
                    .is_some_and(|s| s.contains(&self.fixture.rec_d))
            });
        if d_wrapped_current {
            assert!(
                Bootstrap::required(&self.m5)
                    .unwrap()
                    .access()
                    .unwrap()
                    .is_some(),
                "admitted D with current wrap must open Commons Access"
            );
        }

        let c_has_current = Bootstrap::cached(&self.m3)
            .unwrap()
            .and_then(|b| b.access().unwrap())
            .is_some();
        let m2_ready = Bootstrap::required(&self.m2)
            .unwrap()
            .access()
            .unwrap()
            .is_some();
        let m4_ready = b4.access_in(&j4).unwrap().is_some();
        let d_ready = !self.reconciled
            || !admitted_d
            || Bootstrap::required(&self.m5)
                .unwrap()
                .access()
                .unwrap()
                .is_some();
        let ready = m2_ready && m4_ready && d_ready;

        aterm_spec::interp::State::from([
            ("admitted_d", i64::from(admitted_d)),
            ("removed_b", i64::from(removed_b)),
            ("removed_c", i64::from(removed_c)),
            ("journal_fork", i64::from(journal_fork)),
            ("merged", i64::from(self.merged)),
            ("reconciled", i64::from(self.reconciled)),
            ("epoch_heads", epoch_heads),
            ("d_wrapped_current", i64::from(d_wrapped_current)),
            ("c_has_current", i64::from(c_has_current)),
            ("ready", i64::from(ready)),
        ])
    }
}

#[test]
fn commons_log_model_conforms_and_catches_its_mutants() {
    let model = aterm_models_swarm::commons_log_model();
    aterm_spec::interp::prove_and_catch(&model);
    let fired = aterm_spec::interp::fired_actions(&model);
    assert_eq!(fired.len(), 6, "fired actions: {fired:?}");

    let mut paths: BTreeMap<aterm_spec::interp::State, Vec<&'static str>> = BTreeMap::new();
    let mut queue = VecDeque::from([model.init_state()]);
    paths.insert(model.init_state(), Vec::new());
    while let Some(state) = queue.pop_front() {
        for action in &model.actions {
            for next in model.successors(action.name, &state) {
                if !paths.contains_key(&next) {
                    let mut path = paths[&state].clone();
                    path.push(action.name);
                    paths.insert(next.clone(), path);
                    queue.push_back(next);
                }
            }
        }
    }
    assert_eq!(
        paths.len(),
        aterm_spec::interp::reachable_states(&model).len()
    );

    // Every state but the start was found by one action from a state before it, so its path is
    // that state's path and that action: the replay of the action there is the replay of the
    // state's own path, and is judged against the state itself. Only the start is replayed alone.
    let found_by: BTreeMap<&[&str], &aterm_spec::interp::State> = paths
        .iter()
        .map(|(state, path)| (path.as_slice(), state))
        .collect();
    assert_eq!(
        found_by.len(),
        paths.len(),
        "every state has a path of its own"
    );

    let root = aterm_tempfile::tempdir().expect("tempdir");
    let fixture = CommonsLogFixture::build(root.path());
    let mut run_id = 0usize;

    let start = model.init_state();
    let observed = CommonsLogHarness::replay(&fixture, &root.path().join("s0"), &[]).observe();
    assert_eq!(observed, start, "state mismatch after []");
    let mut judged = BTreeSet::from([&start]);
    for (state, path) in &paths {
        for action in &model.actions {
            if model.action_enabled(action.name, state) {
                run_id += 1;
                let mut h = CommonsLogHarness::replay(
                    &fixture,
                    &root.path().join(format!("t{run_id}")),
                    path,
                );
                h.apply(action.name);
                let next_obs = h.observe();
                let next_path = [path.as_slice(), &[action.name]].concat();
                match found_by.get(next_path.as_slice()) {
                    // The walk found `next` among this action's successors here.
                    Some(&next) => {
                        assert_eq!(&next_obs, next, "state mismatch after {next_path:?}");
                        judged.insert(next);
                    }
                    None => assert!(
                        model.successors(action.name, state).contains(&next_obs),
                        "{} after {path:?} gave {next_obs:?}",
                        action.name
                    ),
                }
            }
        }
    }
    assert_eq!(
        judged.len(),
        paths.len(),
        "every state was replayed and judged"
    );

    // Negative control: claiming reconciliation finished without running the members' vault
    // passes after concurrent removals + newcomer admission violates both
    // `WrapGapClosedAfterReconcile` and `ConcurrentEpochsCoalesced`.
    let mut bad = CommonsLogHarness::replay(
        &fixture,
        &root.path().join("neg-skip-reconcile"),
        &[
            "AdmitDOnIslandA",
            "RemoveBOnIslandA",
            "RemoveCOnIslandB",
            "MergeIslands",
        ],
    );
    bad.reconciled = true;
    let bad_state = bad.observe();
    assert_eq!(bad_state["d_wrapped_current"], 0);
    assert_eq!(bad_state["epoch_heads"], 2);
    let broken: Vec<&str> = model
        .invariants
        .iter()
        .filter(|inv| !model.check_invariant(inv.name, &bad_state))
        .map(|inv| inv.name)
        .collect();
    assert!(
        broken.contains(&"WrapGapClosedAfterReconcile"),
        "expected WrapGapClosedAfterReconcile in {broken:?}"
    );
    assert!(
        broken.contains(&"ConcurrentEpochsCoalesced"),
        "expected ConcurrentEpochsCoalesced in {broken:?}"
    );
}

/// The writer `actor`, `incarnation` and bootstrap of the Mac at `paths` (synthetic test keys).
pub(super) fn member_key(paths: &Paths) -> (Bootstrap, crate::membership_log::MemberId, [u8; 16]) {
    let bootstrap = Bootstrap::required(paths).expect("bootstrap");
    let request = bootstrap.request().expect("request");
    let actor = request.binding().actor();
    let incarnation = *request.binding().incarnation();
    (bootstrap, actor, incarnation)
}

/// A one-admin vault on a loopback server: `m22` (admin) and `m7` (member), Commons enrolled,
/// `entries` published, Genesis v2 signed and synced to `m7`.
pub(super) struct TwoMacs {
    pub(super) root: aterm_tempfile::TempDir,
    pub(super) server: crate::server::Running,
    pub(super) url: String,
    /// The vault's signer, as a join source names it after `#`.
    pub(super) signer: String,
    pub(super) m22: Paths,
    pub(super) m7: Paths,
}

pub(super) fn two_macs(entries: &[(Audience, &str, &[u8])]) -> TwoMacs {
    two_macs_with(entries, false)
}

/// [`two_macs`], with `m7` an admin too where `m7_admin`.
pub(super) fn two_macs_with(entries: &[(Audience, &str, &[u8])], m7_admin: bool) -> TwoMacs {
    two_macs_at(entries, m7_admin, None)
}

/// [`two_macs_with`], its server started by [`crate::server::start_with_clock`] on `clock` (unix
/// seconds) where one is given.
pub(super) fn two_macs_at(
    entries: &[(Audience, &str, &[u8])],
    m7_admin: bool,
    clock: Option<fn() -> u64>,
) -> TwoMacs {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let server = start_loopback_server_at(&root.path().join("s1"), clock);
    let url = format!("http://{}/v1/alab", server.addr);
    let m22 = Paths::for_home(&root.path().join("m22"));
    let created = test_create_unbound(&m22, &url, None, Some("m22")).unwrap();
    admin_keys::legacy::publish_legacy(&m22, &admin_open(&m22).unwrap());
    admin_commons_init(&m22).expect("commons init");
    let lineage = decode_key32(&created.signer).unwrap();
    let pinned = format!("{url}#{}", created.signer);
    let m7 = Paths::for_home(&root.path().join("m7"));
    let m7_rec = machine_identity(&m7).unwrap().recipient().to_bech32();
    {
        let admin = admin_open(&m22).unwrap();
        let mut doc = admin.doc.clone();
        doc.add_machine("m7", &m7_rec).unwrap();
        for (audience, name, value) in entries {
            doc.set_entry(audience, name, value).unwrap();
        }
        admin_publish_approved(&m22, &admin, &doc, std::slice::from_ref(&m7_rec)).unwrap();
    }
    if m7_admin {
        let admin = admin_open(&m22).unwrap();
        let mut doc = admin.doc.clone();
        doc.add_admin("m7").unwrap();
        admin_publish_admin(&m22, &admin, &doc, "m7").unwrap();
    }
    join_host(&m7, &pinned, "m7");
    enroll_commons(&m22, &m7);
    genesis::sign_genesis(&m22, &root.path().join("alab.genesis"), &lineage, true)
        .expect("sign genesis");
    sync(&m7, None).expect("m7 syncs the seeded log");
    TwoMacs {
        root,
        server,
        url,
        signer: created.signer,
        m22,
        m7,
    }
}

#[test]
fn a_log_behind_the_envelope_never_writes_an_older_value() {
    let macs = two_macs(&[(Audience::Everyone, "gh-token", b"token-v1".as_slice())]);
    let (m22, m7) = (&macs.m22, &macs.m7);
    let genesis_era = load_records(m22).unwrap().encode();

    // m22 rotates the token.
    let admin = admin_open(m22).unwrap();
    let mut doc = admin.doc.clone();
    doc.set_entry(&Audience::Everyone, "gh-token", b"token-v2")
        .unwrap();
    admin_publish(m22, &admin, &doc).expect("rotate gh-token");
    let rotated = std::fs::read(m22.cache_file()).unwrap();

    // s2 holds the rotated version and none of its records (the held 0.145 server takes no
    // records, or the POST failed): a Mac on s2 with no log yet writes v2 from the envelope.
    let s2 = start_loopback_server(&macs.root.path().join("s2"));
    let url2 = format!("http://{}/v1/alab", s2.addr);
    transport::publish(&Source::Url(url2.clone()), &rotated).expect("seed s2 envelope");
    let p = Paths::for_home(&macs.root.path().join("p"));
    clone_identity_to(m7, &p);
    join_host(&p, &format!("{url2}#{}", macs.signer), "m7");
    assert_eq!(
        std::fs::read(p.home.join(".secrets/gh-token")).unwrap(),
        b"token-v2"
    );

    // The genesis-era records reach s2 (a peer pull, or Commons repair): p's log is behind the
    // version it applied, and p keeps v2.
    transport::post_records(&url2, &genesis_era).expect("genesis-era records reach s2");
    sync(&p, None).expect("p syncs the genesis-era log");
    assert!(load_records(&p).unwrap().log().is_some());
    assert_eq!(
        std::fs::read(p.home.join(".secrets/gh-token")).unwrap(),
        b"token-v2",
        "a log behind the envelope wrote the older value over the newer file"
    );

    // Once the rotation's records reach s2 the log holds the version, and p reads it from there.
    transport::post_records(&url2, &load_records(m22).unwrap().encode()).unwrap();
    sync(&p, None).expect("p syncs the whole log");
    assert_eq!(
        std::fs::read(p.home.join(".secrets/gh-token")).unwrap(),
        b"token-v2"
    );
    assert!(load_state(&p).unwrap().records_digest.is_some());

    s2.stop();
    macs.server.stop();
    drop(macs.root);
}

#[test]
fn an_edit_by_an_admin_with_no_seat_in_the_log_survives_the_mirror() {
    no_seat_edit_survives_mirror(false);
}

#[test]
fn a_parented_edit_survives_the_automatic_log_mirror() {
    no_seat_edit_survives_mirror(true);
}

fn no_seat_edit_survives_mirror(parented: bool) {
    // Every server it reads is its own on loopback, which answers.
    let _answering = ServersAnswering::set();
    let macs = two_macs(&[(Audience::Everyone, "shared", b"v1".as_slice())]);
    let (m22, m7) = (&macs.m22, &macs.m7);

    // m9 is approved and made an admin after Genesis v2: no record admits it, so it has no seat
    // in the log, as an admin still on 0.145 has none.
    let m9 = Paths::for_home(&macs.root.path().join("m9"));
    let m9_rec = machine_identity(&m9).unwrap().recipient().to_bech32();
    {
        let admin = admin_open(m22).unwrap();
        let mut doc = admin.doc.clone();
        doc.add_machine("m9", &m9_rec).unwrap();
        admin_publish_approved(m22, &admin, &doc, std::slice::from_ref(&m9_rec)).unwrap();
        let admin = admin_open(m22).unwrap();
        let mut doc = admin.doc.clone();
        doc.add_admin("m9").unwrap();
        admin_publish_admin(m22, &admin, &doc, "m9").unwrap();
    }
    join_host(&m9, &format!("{}#{}", macs.url, macs.signer), "m9");
    if parented {
        let admin = admin_open(m22).unwrap();
        admin_publish_conditional_until(
            m22,
            &admin,
            &admin.doc,
            Instant::now() + Duration::from_secs(60),
        )
        .unwrap();
    }
    {
        let admin = admin_open(&m9).unwrap();
        let mut doc = admin.doc.clone();
        doc.set_entry(&Audience::Everyone, "shared", b"v2-from-m9")
            .unwrap();
        admin_publish(&m9, &admin, &doc).expect("m9 edits shared");
    }

    let before_mirror = envelope::parse(&std::fs::read(m9.cache_file()).unwrap())
        .unwrap()
        .digest();
    // m22, the seated admin, mirrors: the edit stays in the envelope, reaches
    // the log, and m7 writes it. An explicit upgrade keeps the parent guard.
    let _ = admin_mirror_log(m22, &[]).expect("admin_mirror_log");
    if parented {
        let mirrored = envelope::parse(&std::fs::read(m22.cache_file()).unwrap()).unwrap();
        assert!(mirrored.parents.contains(&before_mirror));
    }
    let m7_id = read_identity(m7).unwrap().unwrap();
    let (_, view) = envelope_view(&macs.url, &m7_id);
    assert_eq!(
        view.get("shared").map(Vec::as_slice),
        Some(b"v2-from-m9".as_slice()),
        "the mirror reverted an edit the log never held"
    );
    sync(m7, None).expect("m7 syncs");
    assert_eq!(
        std::fs::read(m7.home.join(".secrets/shared")).unwrap(),
        b"v2-from-m9"
    );
    let m7_cfg = config(m7).unwrap().unwrap();
    let m7_cached = envelope::parse(&std::fs::read(m7.cache_file()).unwrap()).unwrap();
    assert!(
        log_entries::readable_log(m7, &m7_cfg, &m7_cached, &m7_id)
            .unwrap()
            .is_some(),
        "m7 reads from the log after the mirror"
    );
    let admin = admin_open(m22).unwrap();
    assert_eq!(
        admin
            .doc
            .entry(&Audience::Everyone, "shared")
            .unwrap()
            .value
            .as_slice(),
        b"v2-from-m9"
    );

    // An unstamped admin publish that drafts no new log records (e.g. granting admin to m7) is
    // also stamped by the seated admin's next mirror so readers return to reading the log.
    {
        let admin = admin_open(&m9).unwrap();
        let mut doc = admin.doc.clone();
        doc.add_admin("m7").unwrap();
        admin_publish_admin(&m9, &admin, &doc, "m7").expect("m9 grants admin to m7 unstamped");
    }
    sync(m7, None).expect("m7 syncs unstamped version");
    let m7_cached = envelope::parse(&std::fs::read(m7.cache_file()).unwrap()).unwrap();
    assert!(
        log_entries::readable_log(m7, &m7_cfg, &m7_cached, &m7_id)
            .unwrap()
            .is_none(),
        "unstamped version falls back to lockers until mirrored"
    );
    assert!(
        admin_mirror_log(m22, &[])
            .expect("admin_mirror_log")
            .is_some(),
        "mirror stamps an unstamped version even when no new log records are needed"
    );
    assert_eq!(admin_mirror_log(m22, &[]).expect("idempotent mirror"), None);
    sync(m7, None).expect("m7 syncs stamped mirror");
    let m7_cached = envelope::parse(&std::fs::read(m7.cache_file()).unwrap()).unwrap();
    assert!(
        log_entries::readable_log(m7, &m7_cfg, &m7_cached, &m7_id)
            .unwrap()
            .is_some(),
        "m7 resumes reading the log after the stamped mirror"
    );

    macs.server.stop();
    drop(macs.root);
}

#[test]
fn an_admin_whose_log_is_behind_never_mirrors_an_older_value() {
    let TwoMacs {
        root,
        server,
        m22,
        m7,
        ..
    } = two_macs_with(&[(Audience::Everyone, "shared", b"v1".as_slice())], true);

    // m22 edits `shared`; its records stay on s1. s2 then holds the edited envelope and only the
    // genesis-era records.
    let genesis_era = load_records(&m22).unwrap().encode();
    {
        let admin = admin_open(&m22).unwrap();
        let mut doc = admin.doc.clone();
        doc.set_entry(&Audience::Everyone, "shared", b"v2").unwrap();
        admin_publish(&m22, &admin, &doc).expect("m22 edits shared");
    }
    let s2 = start_loopback_server(&root.path().join("s2"));
    let url2 = format!("http://{}/v1/alab", s2.addr);
    transport::publish(
        &Source::Url(url2.clone()),
        &std::fs::read(m22.cache_file()).unwrap(),
    )
    .expect("seed s2 envelope");
    transport::post_records(&url2, &genesis_era).expect("seed s2 records");

    // s1 goes away. Admin m7 reaches only s2, so its log is behind the version it reads there,
    // and its mirror must not publish the log's older value over the edit.
    server.stop();
    let _ = admin_mirror_log(&m7, std::slice::from_ref(&url2));
    let m7_id = read_identity(&m7).unwrap().unwrap();
    let (_, view) = envelope_view(&url2, &m7_id);
    assert_eq!(
        view.get("shared").map(Vec::as_slice),
        Some(b"v2".as_slice()),
        "an admin whose log is behind mirrored an older value"
    );

    s2.stop();
    drop(root);
}

#[test]
fn an_epoch_minted_outside_a_host_list_never_seals_that_lists_next_write() {
    let ring = Audience::Hosts(vec!["m22".to_owned()]);
    let macs = two_macs(&[
        (Audience::Everyone, "shared", b"everyone-v1".as_slice()),
        (ring.clone(), "release-cred", b"ring-v1".as_slice()),
    ]);
    let (m22, m7) = (&macs.m22, &macs.m7);
    let m22_recipient = read_identity(m22).unwrap().unwrap().recipient();
    let m7_id = read_identity(m7).unwrap().unwrap();

    // m7 is in `everyone` but not in `[m22]`. It mints an epoch for `[m22]` on top of the heads,
    // wraps its key to m22 alone, and every server takes both records.
    let mut log = load_records(m22).unwrap().log().unwrap().clone();
    let (b7, a7, i7) = member_key(m7);
    let (epoch, key_x) = b7.device().with_seed(|seed| {
        let (draft, key) = log.draft_epoch(&a7, 3_000, ring.clone(), 7).unwrap();
        (draft.sign(seed, i7).unwrap(), key)
    });
    let epoch_id = *epoch.id();
    log.insert(epoch.clone()).unwrap();
    let wrap = b7.device().with_seed(|seed| {
        log.draft_wrap(&a7, 3_001, epoch_id, &key_x, m22_recipient)
            .unwrap()
            .sign(seed, i7)
            .unwrap()
    });
    log.insert(wrap.clone()).unwrap();
    transport::post_records(&macs.url, &encode_records([&epoch, &wrap])).unwrap();
    save_records(m22, &encode_records([&epoch, &wrap])).unwrap();

    // The owner rotates the ring's credential on m22.
    let admin = admin_open(m22).unwrap();
    let mut doc = admin.doc.clone();
    doc.set_entry(&ring, "release-cred", b"ring-v2-rotated")
        .unwrap();
    admin_publish(m22, &admin, &doc).expect("rotate release-cred");
    assert_eq!(
        std::fs::read(m22.home.join(".secrets/release-cred")).unwrap(),
        b"ring-v2-rotated"
    );

    // m7 holds K_X (it minted it): a Wrap to itself in its own copy is all it needs to open
    // whatever was sealed under K_X. The rotated value must be sealed under an epoch it never
    // held.
    let mut theirs = load_records(m22).unwrap().log().unwrap().clone();
    let self_wrap = b7.device().with_seed(|seed| {
        theirs
            .draft_wrap(&a7, 3_002, epoch_id, &key_x, m7_id.recipient())
            .unwrap()
            .sign(seed, i7)
            .unwrap()
    });
    theirs.insert(self_wrap).unwrap();
    let seen = theirs.open_audience_entries(&m7_id).unwrap().unwrap();
    assert_eq!(
        seen.by_audience
            .get(&ring)
            .and_then(|map| map.get("release-cred"))
            .map(|entry| entry.value.as_slice().to_vec()),
        None,
        "a member outside [m22] that minted an epoch read the ring's rotated credential"
    );
    let ours = load_records(m22).unwrap();
    let opened = ours
        .log()
        .unwrap()
        .open_audience_entries(&read_identity(m22).unwrap().unwrap())
        .unwrap()
        .unwrap();
    assert!(!opened.maximal_epochs[&EpochAudience::from(&ring)].contains(&epoch_id));

    macs.server.stop();
    drop(macs.root);
}

#[test]
fn a_mac_the_envelope_admits_stays_in_while_the_log_holds_no_wrap_for_it() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let m22 = Paths::for_home(&root.path().join("m22"));
    let vault_file = root.path().join("alab.vault");
    let created =
        test_create_unbound(&m22, vault_file.to_str().unwrap(), None, Some("m22")).unwrap();
    admin_keys::legacy::publish_legacy(&m22, &admin_open(&m22).unwrap());
    admin_commons_init(&m22).expect("commons init");
    let pinned = format!("{}#{}", vault_file.display(), created.signer);
    let lineage = decode_key32(&created.signer).expect("lineage");
    {
        let admin = admin_open(&m22).unwrap();
        let mut doc = admin.doc.clone();
        doc.set_entry(&Audience::Everyone, "shared", b"everyone-v1")
            .unwrap();
        admin_publish(&m22, &admin, &doc).unwrap();
    }
    genesis::sign_genesis(&m22, &root.path().join("alab.genesis"), &lineage, true)
        .expect("sign genesis");
    let seeded = load_records(&m22).unwrap().encode();

    // An admin that writes no records (a 0.145 admin, or one with no seat in the log) approves
    // m9: the envelope lets it in, and the log holds no Wrap for it.
    let m9 = Paths::for_home(&root.path().join("m9"));
    let m9_rec = machine_identity(&m9).unwrap().recipient().to_bech32();
    let aside = root.path().join("m22.records.aside");
    std::fs::rename(m22.records_file(), &aside).unwrap();
    // The simulated 0.145 admin predates retained-history enrollment as well as records.
    let frontier_files = [
        "records.frontiers",
        "frontiers.anchor",
        "frontiers.astream",
        "frontiers.checkpoint",
    ];
    for name in frontier_files {
        std::fs::rename(
            m22.dir.join(name),
            m22.dir.join(format!("{name}.legacy-test")),
        )
        .unwrap();
    }
    {
        let admin = admin_open(&m22).unwrap();
        let mut doc = admin.doc.clone();
        doc.add_machine("m9", &m9_rec).unwrap();
        admin_publish_approved(&m22, &admin, &doc, std::slice::from_ref(&m9_rec)).unwrap();
    }
    std::fs::rename(&aside, m22.records_file()).unwrap();
    for name in frontier_files {
        std::fs::rename(
            m22.dir.join(format!("{name}.legacy-test")),
            m22.dir.join(name),
        )
        .unwrap();
    }

    // m9 joins before any record reaches it, and reads the envelope.
    join_host(&m9, &pinned, "m9");
    assert_eq!(load_state(&m9).unwrap().membership, Stored::In);
    assert_eq!(
        std::fs::read(m9.home.join(".secrets/shared")).unwrap(),
        b"everyone-v1"
    );

    // The genesis-era log reaches it. The log never removed m9, and the envelope still lets it
    // in: m9 stays in and keeps its files.
    fsx::write_atomic(&m9.records_file(), &seeded, 0o600).unwrap();
    sync(&m9, None).expect("m9 syncs with the log");
    assert_eq!(
        load_state(&m9).unwrap().membership,
        Stored::In,
        "a Mac the envelope admits was marked removed because the log holds no Wrap for it"
    );
    let cfg = config(&m9).unwrap().unwrap();
    assert_eq!(
        read_verified_entry(&m9, &cfg, "shared")
            .unwrap()
            .expect("m9 reads shared")
            .value()
            .as_slice(),
        b"everyone-v1"
    );
    assert!(!matches!(
        observe_commons(&m9).unwrap(),
        crate::commons::Observation::Unavailable(crate::commons::Unavailable::Revoked)
    ));
}

/// Two admins on two loopback servers, before a split: `st1` (`m22`, on `s1`) and `st2` (`m7`,
/// on `s2`), with member `m8` (`m_v2`, on `s1`), a `0.145` member `m9` that reads only the
/// envelope, `split-key` and `other-key` at `v0`, Commons enrolled and Genesis v2 synced.
pub(super) struct SplitAdmins {
    pub(super) root: aterm_tempfile::TempDir,
    pub(super) s1: crate::server::Running,
    pub(super) s2: crate::server::Running,
    pub(super) url1: String,
    pub(super) url2: String,
    pub(super) st1: Paths,
    pub(super) st2: Paths,
    pub(super) m_v2: Paths,
    pub(super) m_0145_id: Identity,
}

pub(super) fn split_admins() -> SplitAdmins {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let s1 = start_loopback_server(&root.path().join("s1"));
    let s2 = start_loopback_server(&root.path().join("s2"));
    let url1 = format!("http://{}/v1/alab", s1.addr);
    let url2 = format!("http://{}/v1/alab", s2.addr);

    let st1 = Paths::for_home(&root.path().join("st1"));
    let created = test_create_unbound(&st1, &url1, None, Some("m22")).unwrap();
    admin_keys::legacy::publish_legacy(&st1, &admin_open(&st1).unwrap());
    admin_commons_init(&st1).expect("commons init on st1");
    let lineage = decode_key32(&created.signer).unwrap();
    let pin_b64 = created.signer.clone();

    let st2 = Paths::for_home(&root.path().join("st2"));
    let st2_rec = machine_identity(&st2).unwrap().recipient().to_bech32();
    let m_v2 = Paths::for_home(&root.path().join("m8"));
    let m_v2_rec = machine_identity(&m_v2).unwrap().recipient().to_bech32();
    let m_0145_id = Identity::generate().unwrap();
    let m_0145_rec = m_0145_id.recipient().to_bech32();
    {
        let a1 = admin_open(&st1).unwrap();
        let mut doc = a1.doc.clone();
        doc.add_machine("m7", &st2_rec).unwrap();
        doc.add_admin("m7").unwrap();
        doc.add_machine("m8", &m_v2_rec).unwrap();
        doc.add_machine("m9", &m_0145_rec).unwrap();
        doc.set_entry(&Audience::Everyone, "split-key", b"v0")
            .unwrap();
        doc.set_entry(&Audience::Everyone, "other-key", b"v0")
            .unwrap();
        admin_publish_admin(&st1, &a1, &doc, "m7").unwrap();
    }
    {
        let a1 = admin_open(&st1).unwrap();
        admin_publish_approved(&st1, &a1, &a1.doc, std::slice::from_ref(&m_v2_rec)).unwrap();
    }
    let pre_split_env = std::fs::read(st1.cache_file()).unwrap();
    transport::publish(&Source::Url(url2.clone()), &pre_split_env).expect("seed s2 envelope");
    join_host(&st2, &format!("{url2}#{pin_b64}"), "m7");
    join_host(&m_v2, &format!("{url1}#{pin_b64}"), "m8");
    enroll_commons(&st1, &st2);
    enroll_commons(&st1, &m_v2);

    genesis::sign_genesis(&st1, &root.path().join("alab.genesis"), &lineage, true)
        .expect("sign genesis");
    // Both servers hold every record and the same envelope before the split.
    let seeded_wire = load_records(&st1).unwrap().encode();
    transport::post_records(&url2, &seeded_wire).expect("seed s2 records");
    let seeded_env = std::fs::read(st1.cache_file()).unwrap();
    let _ = transport::publish(&Source::Url(url2.clone()), &seeded_env);
    sync(&st2, None).expect("st2 syncs seeded log");
    sync(&m_v2, None).expect("m_v2 syncs seeded log");
    assert_eq!(load_state(&st2).unwrap().seq, load_state(&st1).unwrap().seq);
    SplitAdmins {
        root,
        s1,
        s2,
        url1,
        url2,
        st1,
        st2,
        m_v2,
        m_0145_id,
    }
}

/// What a `0.145` Mac holding `id` reads from the envelope `url` serves: entry name to value.
pub(super) fn envelope_view(url: &str, id: &Identity) -> (u64, BTreeMap<String, Vec<u8>>) {
    envelope_view_as(url, id, None)
}

fn envelope_view_as(
    url: &str,
    id: &Identity,
    subscriber: Option<&str>,
) -> (u64, BTreeMap<String, Vec<u8>>) {
    let Fetched::Body(served) = transport::fetch_as(
        &Source::Url(url.to_owned()),
        None,
        aterm_spec::wait::MUST_HAPPEN,
        subscriber,
    )
    .unwrap() else {
        panic!("expected a served envelope on {url}");
    };
    let parsed = envelope::parse(&served).unwrap();
    let opened = open_lockers(&parsed, id)
        .unwrap()
        .expect("the Mac opens the envelope's lockers");
    (
        parsed.seq,
        opened
            .into_iter()
            .map(|e| (e.name, e.value.as_slice().to_vec()))
            .collect(),
    )
}

#[test]
fn after_a_heal_an_admins_own_pass_mirrors_the_lower_seq_sides_edit() {
    // Every server it reads is its own on loopback, which answers.
    let _answering = ServersAnswering::set();
    let lab = split_admins();
    let urls = [lab.url1.clone(), lab.url2.clone()];
    let base_seq = load_state(&lab.st1).unwrap().seq;

    // --- PARTITION --- st1 edits `split-key` once on s1 (the lower seq); st2 edits `other-key`
    // three times on s2.
    {
        let a1 = admin_open(&lab.st1).unwrap();
        let mut d1 = a1.doc.clone();
        d1.set_entry(&Audience::Everyone, "split-key", b"v1-from-lower-seq")
            .unwrap();
        assert_eq!(admin_publish(&lab.st1, &a1, &d1).unwrap().seq, base_seq + 1);
    }
    for step in [
        b"v1".as_slice(),
        b"v2".as_slice(),
        b"v3-from-higher-seq".as_slice(),
    ] {
        let a2 = admin_open(&lab.st2).unwrap();
        let mut d2 = a2.doc.clone();
        d2.set_entry(&Audience::Everyone, "other-key", step)
            .unwrap();
        admin_publish(&lab.st2, &a2, &d2).unwrap();
    }
    assert_eq!(load_state(&lab.st2).unwrap().seq, base_seq + 3);

    // --- HEAL --- nobody mirrors by hand: each admin's own subscriber passes run over both
    // servers, and a 0.145 Mac on either server then reads both islands' edits.
    for _ in 0..2 {
        for admin in [&lab.st1, &lab.st2] {
            let _ = sync_with_candidates(admin, None, &urls);
        }
    }
    for url in &urls {
        let (seq, view) = envelope_view(url, &lab.m_0145_id);
        assert_eq!(
            view.get("split-key").map(Vec::as_slice),
            Some(b"v1-from-lower-seq".as_slice()),
            "{url} serves seq {seq} without the lower-seq side's edit"
        );
        assert_eq!(
            view.get("other-key").map(Vec::as_slice),
            Some(b"v3-from-higher-seq".as_slice())
        );
    }

    lab.s1.stop();
    lab.s2.stop();
}

/// `admin` approves the machine `name` holding `id`, which moves the Commons control entry into
/// a host list that adds it.
fn approve(admin_paths: &Paths, name: &str, id: &Identity) {
    let recipient = id.recipient().to_bech32();
    let admin = admin_open(admin_paths).unwrap();
    let mut doc = admin.doc.clone();
    doc.add_machine(name, &recipient).unwrap();
    admin_publish_approved(admin_paths, &admin, &doc, std::slice::from_ref(&recipient))
        .expect("approve on one side of the split");
}

/// Both `ids` read the Commons control entry from the envelope every one of `urls` serves, and
/// from `admin_paths`' log.
pub(super) fn assert_in_commons_control(
    admin_paths: &Paths,
    urls: &[String],
    ids: &[(&str, &Identity)],
) {
    for url in urls {
        for (name, id) in ids {
            let (_, view) = envelope_view(url, id);
            assert!(
                view.contains_key(crate::commons::ENTRY),
                "{name} lost Commons control in the envelope {url} serves"
            );
        }
    }
    let store = load_records(admin_paths).unwrap();
    let log = store.log().unwrap();
    for (name, id) in ids {
        let opened = log
            .open_entry_set(id)
            .unwrap()
            .unwrap_or_else(|| panic!("{name} reads the log"));
        assert!(
            opened
                .entries
                .iter()
                .any(|e| e.name == crate::commons::ENTRY),
            "{name} lost Commons control in the log"
        );
    }
}

#[test]
fn approvals_on_both_sides_of_a_split_keep_both_macs_in_commons_control() {
    // Every server it reads is its own on loopback, which answers.
    let _answering = ServersAnswering::set();
    let lab = split_admins();
    let urls = [lab.url1.clone(), lab.url2.clone()];
    let ma_id = machine_identity(&Paths::for_home(&lab.root.path().join("ma"))).unwrap();
    let mb_id = machine_identity(&Paths::for_home(&lab.root.path().join("mb"))).unwrap();

    // --- PARTITION --- st1 approves ma on s1; st2 approves mb on s2. Each approval moves the
    // Commons control entry into a new host list.
    approve(&lab.st1, "ma", &ma_id);
    approve(&lab.st2, "mb", &mb_id);

    // --- HEAL --- m8 sits in the old Commons host list and unions both islands' records: its
    // pass completes and it still sees Commons control.
    sync_with_candidates(&lab.m_v2, None, &urls).expect("m8 syncs both islands' approvals");
    sync_with_candidates(&lab.m_v2, None, &urls).expect("m8's next pass");
    assert!(matches!(
        observe_commons(&lab.m_v2).unwrap(),
        crate::commons::Observation::Visible(_)
    ));

    // An admin mirrors the merged log: both approved Macs keep the control entry, in the log
    // and in the envelope a 0.145 Mac reads.
    admin_mirror_log(&lab.st2, &urls)
        .expect("admin_mirror_log")
        .expect("mirror published");
    assert_in_commons_control(&lab.st2, &urls, &[("ma", &ma_id), ("mb", &mb_id)]);

    lab.s1.stop();
    lab.s2.stop();
}

/// How many `Epoch` records `paths`' log holds for `audience`.
fn epochs_for(paths: &Paths, audience: &Audience) -> usize {
    let audience = crate::membership_log::EpochAudience::from(audience);
    load_records(paths)
        .unwrap()
        .log()
        .unwrap()
        .records()
        .filter(|r| matches!(r.body(), crate::membership_log::Body::Epoch { audience: a, .. } if *a == audience))
        .count()
}

/// An admin outside a host list holds none of its epochs (they are wrapped to the list's machines
/// alone), so its unset there is written against the version it opened: a `Delete` the list's
/// new-build Macs read, and a publish that leaves the list alone writes nothing there.
///
/// NEGATIVE CONTROL: write an audience this Mac cannot read as before (drop the `sealed_away`
/// branch of `draft_document_diff`): the unset writes no `Delete`, m7 keeps `m7-cred` from the
/// log, and every publish mints a fresh `[m7]` epoch.
#[test]
fn an_admin_outside_a_host_list_deletes_an_entry_there_for_every_mac_in_it() {
    let ring = Audience::Hosts(vec!["m7".to_owned()]);
    let macs = two_macs(&[
        (Audience::Everyone, "shared", b"everyone-v1".as_slice()),
        (ring.clone(), "m7-cred", b"ring-v1".as_slice()),
        (ring.clone(), "m7-keep", b"keep-v1".as_slice()),
    ]);
    let (m22, m7) = (&macs.m22, &macs.m7);
    {
        let admin = admin_open(m22).unwrap();
        admin_publish(m22, &admin, &admin.doc.clone()).expect("a stamped version");
    }
    sync(m7, None).expect("m7 applies the stamped version");
    assert!(
        load_state(m7).unwrap().records_digest.is_some(),
        "m7 reads the log"
    );
    let m22_id = read_identity(m22).unwrap().unwrap();
    let opened = load_records(m22)
        .unwrap()
        .log()
        .unwrap()
        .open_audience_entries(&m22_id)
        .unwrap()
        .unwrap();
    assert!(
        !opened.by_audience.contains_key(&ring),
        "m22 holds no [m7] key"
    );

    // A publish that leaves `[m7]` alone writes nothing there.
    let ring_epochs = epochs_for(m22, &ring);
    {
        let admin = admin_open(m22).unwrap();
        let mut doc = admin.doc.clone();
        doc.set_entry(&Audience::Everyone, "shared", b"everyone-v2")
            .unwrap();
        admin_publish(m22, &admin, &doc).expect("an edit elsewhere");
    }
    assert_eq!(
        epochs_for(m22, &ring),
        ring_epochs,
        "a publish that left [m7] alone minted an epoch there"
    );

    // The owner runs `unset m7-cred --for m7` on m22.
    let admin = admin_open(m22).unwrap();
    let mut doc = admin.doc.clone();
    assert!(doc.unset_entry(&ring, "m7-cred").unwrap());
    admin_publish(m22, &admin, &doc).expect("unset m7-cred");
    let m7_id = read_identity(m7).unwrap().unwrap();
    let (_, view) = envelope_view(&macs.url, &m7_id);
    assert!(!view.contains_key("m7-cred"), "the envelope dropped it");

    sync(m7, None).expect("m7 applies the unset");
    assert!(
        load_state(m7).unwrap().records_digest.is_some(),
        "m7 still reads the log"
    );
    assert!(
        !m7.home.join(".secrets/m7-cred").exists(),
        "a Mac in [m7] kept an entry an admin outside it deleted"
    );
    assert_eq!(
        std::fs::read(m7.home.join(".secrets/m7-keep")).unwrap(),
        b"keep-v1"
    );

    macs.server.stop();
    drop(macs.root);
}

/// An admin inside a host list mirrors its log whenever the log moved; the `Delete` an outside
/// admin now writes keeps it from putting the deleted entry back in the envelope.
///
/// NEGATIVE CONTROL: as above (no `Delete` under the outside admin's epoch): m7's mirror overlays
/// the still-live `m7-cred` and publishes it to every Mac again.
#[test]
fn an_admin_inside_a_host_list_never_restores_an_entry_an_outside_admin_deleted() {
    let ring = Audience::Hosts(vec!["m7".to_owned()]);
    let TwoMacs {
        root,
        server,
        url,
        m22,
        m7,
        ..
    } = two_macs_with(
        &[
            (Audience::Everyone, "shared", b"everyone-v1".as_slice()),
            (ring.clone(), "m7-cred", b"ring-v1".as_slice()),
            (ring.clone(), "m7-keep", b"keep-v1".as_slice()),
        ],
        true,
    );
    {
        let admin = admin_open(&m22).unwrap();
        admin_publish(&m22, &admin, &admin.doc.clone()).expect("a stamped version");
    }
    let _ = sync(&m7, None);
    let _ = sync(&m7, None);
    let m7_id = read_identity(&m7).unwrap().unwrap();

    let admin = admin_open(&m22).unwrap();
    let mut doc = admin.doc.clone();
    assert!(doc.unset_entry(&ring, "m7-cred").unwrap());
    admin_publish(&m22, &admin, &doc).expect("unset m7-cred");
    let (_, view) = envelope_view(&url, &m7_id);
    assert!(!view.contains_key("m7-cred"));

    // Nobody acts: m7's own sync passes.
    let _ = sync(&m7, None);
    let _ = sync(&m7, None);
    let (seq, view) = envelope_view(&url, &m7_id);
    assert!(
        !view.contains_key("m7-cred"),
        "the inside admin's pass put the deleted entry back in the envelope (seq {seq})"
    );
    assert!(
        admin_open(&m22)
            .unwrap()
            .doc
            .entry(&ring, "m7-cred")
            .is_none()
    );

    server.stop();
    drop(root);
}

/// An admin outside a host list that drops the list's whole section writes a `Delete` of each
/// entry there for the list's machines still in `[machines]`.
///
/// NEGATIVE CONTROL: drop the loop over `base`'s sections in `draft_document_diff`: m7 still
/// reads both entries from the drafted log.
#[test]
fn an_admin_outside_a_host_list_that_drops_its_section_deletes_every_entry_there() {
    let ring = Audience::Hosts(vec!["m7".to_owned()]);
    let macs = two_macs(&[
        (Audience::Everyone, "shared", b"everyone-v1".as_slice()),
        (ring.clone(), "m7-cred", b"ring-v1".as_slice()),
        (ring.clone(), "m7-keep", b"keep-v1".as_slice()),
    ]);
    let m22 = &macs.m22;
    {
        let admin = admin_open(m22).unwrap();
        admin_publish(m22, &admin, &admin.doc.clone()).expect("a stamped version");
    }
    let admin = admin_open(m22).unwrap();
    let rendered = String::from_utf8(admin.doc.render().as_slice().to_vec()).unwrap();
    let start = rendered.find("[m7]\n").expect("the [m7] section");
    let end = rendered[start + 1..]
        .find("\n[")
        .map_or(rendered.len(), |at| start + 1 + at + 1);
    let dropped = Document::parse(&format!("{}{}", &rendered[..start], &rendered[end..])).unwrap();
    assert!(dropped.entry(&ring, "m7-cred").is_none());
    let cfg = config(m22).unwrap().unwrap();
    let cached = envelope::parse(&std::fs::read(m22.cache_file()).unwrap()).unwrap();
    let drafted = log_entries::draft_document_diff(
        m22,
        &cfg,
        &cached.signer,
        &cached.handoff,
        &read_identity(m22).unwrap().unwrap(),
        &dropped,
        log_entries::Base {
            doc: Some(&admin.doc),
            resolve: None,
            readmit: None,
        },
    )
    .unwrap()
    .expect("m22 writes the log");
    let mut store = load_records(m22).unwrap();
    store.ingest(&encode_records(&drafted.records)).unwrap();
    let m7_id = read_identity(&macs.m7).unwrap().unwrap();
    let names: Vec<String> = store
        .log()
        .unwrap()
        .open_entries(&m7_id)
        .unwrap()
        .unwrap()
        .into_iter()
        .map(|e| e.name)
        .collect();
    assert!(
        !names.iter().any(|n| n == "m7-cred" || n == "m7-keep"),
        "m7 still reads a dropped section's entries: {names:?}"
    );

    macs.server.stop();
    drop(macs.root);
}

/// A revoked Mac keeps the epoch keys it held. Before revoke wrote a `Remove` into the log, it
/// kept its seat too, so a `Put` it signed under an old epoch on top of the revoke's reseal won
/// the register on every Mac that reads the log, and every server took it.
///
/// This fixture's log carries Commons epochs, so since F18 `retire_cached` takes the Mac out of
/// the log first (`retire_commons_members_in_log`) and `remove_from_log` finds nobody to remove;
/// `remove_from_log` covers a log without them (one seeded before F18, or one
/// `trusted_commons_log` does not trust).
///
/// NEGATIVE CONTROL (before F18 merged): drop `remove_from_log` from `admin_revoke`: m7 writes the
/// revoked Mac's value into `~/.secrets/gh-token`. Since F18, dropping it as well as the in-log
/// retirement fails the revoke ("Commons retirement did not retain the requested restriction").
#[test]
fn a_revoked_mac_cannot_rewrite_an_entry_for_the_macs_that_read_the_log() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let server = start_loopback_server(&root.path().join("s1"));
    let url = format!("http://{}/v1/alab", server.addr);
    let m22 = Paths::for_home(&root.path().join("m22"));
    let created = test_create_unbound(&m22, &url, None, Some("m22")).unwrap();
    admin_keys::legacy::publish_legacy(&m22, &admin_open(&m22).unwrap());
    admin_commons_init(&m22).expect("commons init");
    let lineage = decode_key32(&created.signer).unwrap();
    let pinned = format!("{url}#{}", created.signer);
    let m7 = Paths::for_home(&root.path().join("m7"));
    let m9 = Paths::for_home(&root.path().join("m9"));
    {
        let admin = admin_open(&m22).unwrap();
        let mut doc = admin.doc.clone();
        let mut approved = Vec::new();
        for (name, paths) in [("m7", &m7), ("m9", &m9)] {
            let rec = machine_identity(paths).unwrap().recipient().to_bech32();
            doc.add_machine(name, &rec).unwrap();
            approved.push(rec);
        }
        doc.set_entry(&Audience::Everyone, "gh-token", b"token-v1")
            .unwrap();
        admin_publish_approved(&m22, &admin, &doc, &approved).unwrap();
    }
    join_host(&m7, &pinned, "m7");
    join_host(&m9, &pinned, "m9");
    enroll_commons(&m22, &m7);
    enroll_commons(&m22, &m9);
    genesis::sign_genesis(&m22, &root.path().join("alab.genesis"), &lineage, true)
        .expect("sign genesis");
    {
        let admin = admin_open(&m22).unwrap();
        admin_publish(&m22, &admin, &admin.doc.clone()).expect("a stamped version");
    }
    sync(&m7, None).expect("m7 syncs");
    sync(&m9, None).expect("m9 syncs");
    assert!(
        load_state(&m7).unwrap().records_digest.is_some(),
        "m7 reads the log"
    );

    // What m9 holds before it is revoked: the `[everyone]` epoch's key.
    let m9_id = read_identity(&m9).unwrap().unwrap();
    let held = load_records(&m9).unwrap().log().unwrap().clone();
    let opened = held.open_audience_entries(&m9_id).unwrap().unwrap();
    let epoch = opened.maximal_epochs[&crate::membership_log::EpochAudience::Everyone][0];
    let key = opened.unwrapped_keys[&epoch].clone();

    // The owner revokes m9 (stolen, say).
    let admin = admin_open(&m22).unwrap();
    admin_revoke(&m22, &admin, "m9", false).expect("revoke m9");

    // m9 reads the public records, signs a Put of its own value under the epoch it still holds
    // on top of every head, and posts it to the server.
    let mut theirs = load_records(&m22).unwrap().log().unwrap().clone();
    let (b9, a9, i9) = member_key(&m9);
    let forged = Entry {
        name: "gh-token".to_owned(),
        value: SecretBytes::new(b"stolen-token".to_vec()),
        audience: Audience::Everyone,
        if_absent: false,
        anchor: None,
    };
    let put = b9.device().with_seed(|seed| {
        theirs
            .draft_put(&a9, 9_000, epoch, &key, &forged)
            .unwrap()
            .sign(seed, i9)
            .unwrap()
    });
    let _ = theirs.insert(put.clone());
    let _ = transport::post_records(&url, &encode_records([&put]));

    sync(&m7, None).expect("m7 syncs after the forged Put");
    assert_eq!(
        std::fs::read(m7.home.join(".secrets/gh-token")).unwrap(),
        b"token-v1",
        "a revoked Mac rewrote an entry on a Mac that reads the log"
    );
    let m7_id = read_identity(&m7).unwrap().unwrap();
    let store = load_records(&m22).unwrap();
    let mut ours = store.log().unwrap().clone();
    let _ = ours.insert(put);
    let value = ours
        .open_entries(&m7_id)
        .unwrap()
        .unwrap()
        .into_iter()
        .find(|e| e.name == "gh-token")
        .map(|e| e.value.as_slice().to_vec());
    assert_eq!(value.as_deref(), Some(b"token-v1".as_slice()));

    server.stop();
    drop(root);
}

/// Two admins save different values for one entry while split. After the heal each admin's own
/// pass mirrors the merged log, every Mac writes the same value, and the other stays a candidate:
/// status names the conflict until a person saves the entry, which settles it.
///
/// NEGATIVE CONTROL: let a mirror re-put a conflicted name (`resolving` true for every conflicted
/// name in `draft_document_diff`): the heal settles the conflict and status names nothing.
#[test]
fn a_conflict_stays_until_a_person_saves_it_and_status_names_it() {
    // Every server it reads is its own on loopback, which answers.
    let _answering = ServersAnswering::set();
    let lab = split_admins();
    let urls = [lab.url1.clone(), lab.url2.clone()];

    // --- PARTITION --- each admin saves its own value for `split-key`.
    for (admin, value) in [
        (&lab.st1, b"from-st1".as_slice()),
        (&lab.st2, b"from-st2".as_slice()),
    ] {
        let a = admin_open(admin).unwrap();
        let mut d = a.doc.clone();
        d.set_entry(&Audience::Everyone, "split-key", value)
            .unwrap();
        admin_publish(admin, &a, &d).unwrap();
    }

    // --- HEAL --- nobody acts: each admin's own passes run over both servers.
    for _ in 0..2 {
        for admin in [&lab.st1, &lab.st2] {
            let _ = sync_with_candidates(admin, None, &urls);
        }
    }
    sync_with_candidates(&lab.m_v2, None, &urls).expect("m8 syncs");
    let conflict = vec!["[everyone] split-key".to_owned()];
    assert_eq!(
        conflicts(&lab.m_v2).unwrap(),
        conflict,
        "the heal settled a conflict nobody saw"
    );
    let winner = std::fs::read(lab.m_v2.home.join(".secrets/split-key")).unwrap();
    assert!(winner == b"from-st1" || winner == b"from-st2");
    assert_eq!(conflicts(&lab.st1).unwrap(), conflict);

    // A person saves the value every Mac writes: that settles it.
    let mut a = admin_open(&lab.st1).unwrap();
    assert_eq!(
        a.doc
            .entry(&Audience::Everyone, "split-key")
            .unwrap()
            .value
            .as_slice(),
        winner.as_slice()
    );
    let mut d = a.doc.clone();
    d.set_entry(&Audience::Everyone, "split-key", &winner)
        .unwrap();
    a.resolving(&Audience::Everyone, "split-key");
    admin_publish(&lab.st1, &a, &d).expect("the person's save");
    sync_with_candidates(&lab.m_v2, None, &urls).expect("m8 syncs the save");
    assert!(conflicts(&lab.m_v2).unwrap().is_empty());
    assert_eq!(
        std::fs::read(lab.m_v2.home.join(".secrets/split-key")).unwrap(),
        winner
    );

    lab.s1.stop();
    lab.s2.stop();
}

/// A publish that ends after a server stored its envelope and before it kept its records (the
/// process ended, or every reply was lost) leaves a version stamped with records only its writer's
/// hold has. The writer's own next passes take them in and send them, and every Mac's log then
/// reads the version, with nobody acting.
///
/// NEGATIVE CONTROL: skip `adopt_held_draft` in the sync apply path's `Behind` branch: m22 and m7
/// both wait on the records for good, and m7 keeps `token-v1`.
#[test]
fn a_publish_cut_short_after_the_server_stored_it_still_reaches_every_mac() {
    let macs = two_macs(&[(Audience::Everyone, "gh-token", b"token-v1".as_slice())]);
    let (m22, m7) = (&macs.m22, &macs.m7);
    {
        let admin = admin_open(m22).unwrap();
        admin_publish(m22, &admin, &admin.doc.clone()).expect("a stamped version");
    }
    sync(m7, None).expect("m7 applies the stamped version");
    assert!(
        load_state(m7).unwrap().records_digest.is_some(),
        "m7 reads the log"
    );

    // The owner rotates the token; the publish ends once the server took the envelope.
    let admin = admin_open(m22).unwrap();
    let mut doc = admin.doc.clone();
    doc.set_entry(&Audience::Everyone, "gh-token", b"token-v2")
        .unwrap();
    CUT_AFTER_STORE.with(|cut| cut.set(true));
    assert!(admin_publish(m22, &admin, &doc).is_err());
    let m7_id = read_identity(m7).unwrap().unwrap();
    let (_, view) = envelope_view(&macs.url, &m7_id);
    assert_eq!(
        view.get("gh-token").map(Vec::as_slice),
        Some(b"token-v2".as_slice()),
        "the server stored the rotation"
    );

    // Nobody acts: m22's subscriber passes, then m7's.
    for _ in 0..2 {
        let _ = sync(m22, None);
    }
    sync(m7, None).expect("m7 syncs");
    assert_eq!(
        std::fs::read(m7.home.join(".secrets/gh-token")).unwrap(),
        b"token-v2",
        "a Mac whose log reads the version waits for good on records only the writer held"
    );
    assert!(load_state(m7).unwrap().records_digest.is_some());

    macs.server.stop();
    drop(macs.root);
}

/// One side of a split removes an anchored set while the other adds an entry to it and edits
/// another entry. After the heal every admin's pass completes, every Mac still reads its
/// entries, and the next publish keeps the other side's edit.
///
/// NEGATIVE CONTROL: skip `drop_dangling_anchors` in `Log::open_audience_entries` and return
/// `base` from `admin_base` when the overlay fails: the admins' passes fail on the dangling
/// anchor, m8 marks its pass failed and reads nothing, and the next publish writes `other-key`
/// back to `v0` on every Mac.
#[test]
fn an_anchor_removed_on_one_side_of_a_split_leaves_every_mac_reading() {
    // Every server it reads is its own on loopback, which answers.
    let _answering = ServersAnswering::set();
    let lab = split_admins();
    let urls = [lab.url1.clone(), lab.url2.clone()];

    // Before the split both sides hold an anchored set.
    {
        let a = admin_open(&lab.st1).unwrap();
        let mut d = a.doc.clone();
        d.set_entry_with(&Audience::Everyone, "seed", b"seed-v0", true, None)
            .unwrap();
        d.set_entry_with(&Audience::Everyone, "old", b"old-v0", true, Some("seed"))
            .unwrap();
        admin_publish(&lab.st1, &a, &d).unwrap();
    }
    transport::post_records(&lab.url2, &load_records(&lab.st1).unwrap().encode()).unwrap();
    transport::publish(
        &Source::Url(lab.url2.clone()),
        &std::fs::read(lab.st1.cache_file()).unwrap(),
    )
    .unwrap();
    sync(&lab.st2, None).expect("st2 holds the set");
    sync(&lab.m_v2, None).expect("m8 holds the set");

    // --- PARTITION --- st1 removes the set; st2 adds to it and edits `other-key`.
    {
        let a = admin_open(&lab.st1).unwrap();
        let mut d = a.doc.clone();
        assert!(d.unset_entry(&Audience::Everyone, "old").unwrap());
        assert!(d.unset_entry(&Audience::Everyone, "seed").unwrap());
        admin_publish(&lab.st1, &a, &d).unwrap();
    }
    {
        let a = admin_open(&lab.st2).unwrap();
        let mut d = a.doc.clone();
        d.set_entry_with(&Audience::Everyone, "new", b"new-v0", true, Some("seed"))
            .unwrap();
        d.set_entry(&Audience::Everyone, "other-key", b"v1-from-st2")
            .unwrap();
        admin_publish(&lab.st2, &a, &d).unwrap();
    }

    // --- HEAL --- nobody acts.
    let mut passes = Vec::new();
    for _ in 0..2 {
        for admin in [&lab.st1, &lab.st2] {
            passes.push(sync_with_candidates(admin, None, &urls).map(|_| ()));
        }
    }
    assert!(
        passes.iter().any(Result::is_ok),
        "every admin pass failed after the heal: {passes:?}"
    );
    sync_with_candidates(&lab.m_v2, None, &urls).expect("m8 syncs both islands");
    assert!(!load_state(&lab.m_v2).unwrap().failed, "m8's pass failed");
    let cfg = config(&lab.m_v2).unwrap().unwrap();
    assert!(
        read_verified_entry(&lab.m_v2, &cfg, "split-key")
            .unwrap()
            .is_some(),
        "m8 lost its reads"
    );

    // An unchanged publish (an approval, the custodian) keeps st2's edit everywhere.
    {
        let a = admin_open(&lab.st1).unwrap();
        admin_publish_with_candidates(&lab.st1, &a, &a.doc.clone(), &urls)
            .expect("an unchanged publish");
    }
    for url in &urls {
        let (seq, view) = envelope_view(url, &lab.m_0145_id);
        assert_eq!(
            view.get("other-key").map(Vec::as_slice),
            Some(b"v1-from-st2".as_slice()),
            "{url} seq {seq} lost st2's edit"
        );
    }
    sync_with_candidates(&lab.m_v2, None, &urls).expect("m8 syncs the publish");
    assert_eq!(
        std::fs::read(lab.m_v2.home.join(".secrets/other-key")).unwrap(),
        b"v1-from-st2"
    );

    lab.s1.stop();
    lab.s2.stop();
}

#[test]
fn log_witness_skips_inert_admit_concurrent_remove_excludes_epoch_writer_and_admin_revoke_retires_in_log()
 {
    use crate::commons::{bootstrap::Device, journal::Journal};
    use crate::membership_log::{Body, Entrant, EpochAudience, Evidence, Log};

    let macs = two_macs(&[(Audience::Everyone, "shared", b"v1".as_slice())]);
    let (m22, m7) = (&macs.m22, &macs.m7);
    let (b22, actor22, inc22) = member_key(m22);
    let (b7, actor7, inc7) = member_key(m7);
    let trust = b22.trust();

    // 1. `log_witness` and `historical_seat_entry` skip an inert (plain-removed signer's) `Admit`
    //    and select the later standing `Admit`, and `historical_commons_writers` excludes an
    //    `Admit` that was concurrently beaten by a `Remove` before an `Epoch(Commons)`.
    let m8 = Paths::for_home(&macs.root.path().join("m8"));
    configure_log_member(&m8, &format!("{}#{}", macs.url, macs.signer), "m8").unwrap();
    let _ = machine_identity(&m8).unwrap();
    let req8 = Device::open(&m8)
        .unwrap()
        .unwrap()
        .request(trust.network, trust.authority)
        .unwrap();
    let actor8 = req8.binding().actor();
    let entrant8 = Entrant {
        request: req8.clone(),
        node_key: [0x88; 32],
        name: "b3".to_owned(),
        label: "m8".to_owned(),
    };

    let base_log = load_records(m22).unwrap().log().unwrap().clone();
    let genesis_id = *base_log.genesis().id();
    let mut removed_m7_log = base_log.clone();
    let rem_m7 = b22
        .device()
        .with_seed(|seed22| {
            removed_m7_log
                .draft(
                    &actor22,
                    100,
                    Body::Remove {
                        member: actor7,
                        cutoff: genesis_id,
                    },
                )
                .sign(seed22, inc22)
        })
        .unwrap();
    removed_m7_log.insert(rem_m7).unwrap();

    // Plain-removed `m7` signs an inert `Admit(m8)` (accepted by `Log::insert`, not in `voided`,
    // but unauthorized in `standing.admits`), followed by active `m22` signing a valid `Admit(m8)`.
    let inert_admit8 = b7
        .device()
        .with_seed(|seed7| {
            removed_m7_log
                .draft(
                    &actor7,
                    101,
                    Body::Admit {
                        entrant: entrant8.clone(),
                        evidence: Evidence::Phrase,
                    },
                )
                .sign(seed7, inc7)
        })
        .unwrap();
    removed_m7_log.insert(inert_admit8.clone()).unwrap();
    assert!(
        !removed_m7_log
            .membership()
            .voided()
            .contains(inert_admit8.id())
    );
    assert!(!removed_m7_log.has_historical_seat(req8.binding()));

    let valid_admit8 = b22
        .device()
        .with_seed(|seed22| {
            removed_m7_log
                .draft(
                    &actor22,
                    102,
                    Body::Admit {
                        entrant: entrant8.clone(),
                        evidence: Evidence::Phrase,
                    },
                )
                .sign(seed22, inc22)
        })
        .unwrap();
    removed_m7_log.insert(valid_admit8.clone()).unwrap();
    assert_eq!(
        removed_m7_log.historical_seat_entry(req8.binding()),
        Some(*valid_admit8.id())
    );

    // Concurrent `Remove(m8)` vs second `Admit(m8)` before `Epoch(Commons)`:
    // `m22` admits `m8` (`admit8_a`), then on branch A `m22` removes `m8` at `admit8_a`, while
    // concurrently on branch B `m7` admits `m8` again (`admit8_b`), and then `m22` merges both and
    // mints a rotated `Epoch(Commons)`. Because `Remove` beats a concurrent `Admit`, `m8` must NOT
    // appear in `historical_commons_writers` for that rotated epoch!
    let mut branch_a = base_log.clone();
    let admit8_a = b22
        .device()
        .with_seed(|seed22| {
            branch_a
                .draft(
                    &actor22,
                    200,
                    Body::Admit {
                        entrant: entrant8.clone(),
                        evidence: Evidence::Phrase,
                    },
                )
                .sign(seed22, inc22)
        })
        .unwrap();
    branch_a.insert(admit8_a.clone()).unwrap();
    let rem_m8 = b22
        .device()
        .with_seed(|seed22| {
            branch_a
                .draft(
                    &actor22,
                    201,
                    Body::Remove {
                        member: actor8,
                        cutoff: *admit8_a.id(),
                    },
                )
                .sign(seed22, inc22)
        })
        .unwrap();
    branch_a.insert(rem_m8).unwrap();

    let mut branch_b = base_log.clone();
    let admit8_b = b7
        .device()
        .with_seed(|seed7| {
            branch_b
                .draft(
                    &actor7,
                    202,
                    Body::Admit {
                        entrant: entrant8,
                        evidence: Evidence::Phrase,
                    },
                )
                .sign(seed7, inc7)
        })
        .unwrap();
    branch_b.insert(admit8_b).unwrap();
    branch_a.merge(&branch_b).unwrap();
    assert!(!branch_a.membership().contains(&actor8));

    let ep_after_concurrent_rem = b22
        .device()
        .with_seed(|seed22| {
            let (draft, _) = branch_a
                .draft_epoch(&actor22, 203, EpochAudience::Commons, 99)
                .unwrap();
            draft.sign(seed22, inc22)
        })
        .unwrap();
    let ep_id = *ep_after_concurrent_rem.id();
    branch_a.insert(ep_after_concurrent_rem).unwrap();
    let (_, writers) = branch_a
        .historical_commons_writers(&ep_id)
        .expect("effective Commons epoch");
    assert!(
        !writers.iter().any(|b| b.actor() == actor8),
        "concurrently removed m8 must not be an eligible writer in rotated epoch"
    );

    // 2. `admin_retire` and `admin_revoke` on a log-backed Commons vault retire the target in the
    //    membership log (`remove_member_in_log`) with zero new authority-seed `Journal` nodes.
    let journal_before: BTreeSet<_> = Journal::read_existing(b22.journal_dir(), trust)
        .unwrap()
        .inventory()
        .copied()
        .collect();
    let admin = admin_open(m22).unwrap();
    let r_first = admin_retire(m22, &admin, &["m7".to_owned()]).expect("admin_retire m7 in log");
    assert!(r_first.changed);
    assert_eq!(r_first.recipients, 1);
    let r_retry =
        admin_retire(m22, &admin, &["m7".to_owned()]).expect("idempotent admin_retire retry");
    assert!(!r_retry.changed);
    let revoked = admin_revoke(m22, &admin, "m7", false).expect("admin_revoke m7 on log vault");
    assert!(revoked.retirement.is_some());
    let log_after = load_records(m22).unwrap().log().unwrap().clone();
    assert!(!log_after.membership().contains(&actor7));
    assert!(log_after.membership().removal(&actor7).is_some());
    let journal_after: BTreeSet<_> = Journal::read_existing(b22.journal_dir(), trust)
        .unwrap()
        .inventory()
        .copied()
        .collect();
    assert_eq!(
        journal_after, journal_before,
        "log-backed admin_retire/admin_revoke must sign zero new authority Journal nodes"
    );

    // Save `removed_m7_log` (plus wraps for `m8`) to `m8` and verify `Bootstrap::log_witness()`
    // produces a chain that passes `has_historical_seat`.
    save_records(
        m22,
        &encode_records(&removed_m7_log.records().cloned().collect::<Vec<_>>()),
    )
    .unwrap();
    let _ = vault_passes(m22);
    save_records(&m8, &load_records(m22).unwrap().encode()).unwrap();
    let witness_wire = Bootstrap::required(&m8)
        .unwrap()
        .log_witness()
        .unwrap()
        .expect("m8 log witness");
    let decoded =
        crate::membership_log::decode_records(&encode_records(witness_wire.records())).unwrap();
    let mut iter = decoded.into_iter();
    let mut verify_log = Log::new(iter.next().unwrap()).unwrap();
    for r in iter {
        verify_log.insert(r).unwrap();
    }
    assert!(verify_log.has_historical_seat(req8.binding()));
    assert!(!verify_log.audience_left_out(&EpochAudience::Commons, &actor8));
    assert!(
        !verify_log
            .audience_left_out_recipient(&EpochAudience::Commons, &req8.binding().recipient())
    );

    // When a post-Genesis machine `m9` was wrapped in an earlier epoch, then a newer `Commons`
    // epoch was minted before `m9` enrolled (`Admit` signed after the newer epoch, followed by
    // `Wrap` for `m9`), `Bootstrap::log_witness()` includes `m9`'s `Wrap` records so the witness
    // chain does not leave `m9` out of `EpochAudience::Commons` or `EpochAudience::Everyone`.
    let m9 = Paths::for_home(&macs.root.path().join("m9"));
    configure_log_member(&m9, &format!("{}#{}", macs.url, macs.signer), "m9").unwrap();
    let req9 = Device::open(&m9)
        .unwrap()
        .unwrap()
        .request(trust.network, trust.authority)
        .unwrap();
    let rec9 = req9.binding().recipient();
    let mut pre_wrapped_m9 = base_log.clone();
    let id22_early = read_identity(m22).unwrap().unwrap();
    let opened_early = pre_wrapped_m9
        .open_audience_entries(&id22_early)
        .unwrap()
        .unwrap();
    let ep_commons_1 = opened_early.maximal_epochs[&EpochAudience::Commons][0];
    let key_commons_1 = opened_early.unwrapped_keys[&ep_commons_1].clone();
    let wrap9_1 = b22
        .device()
        .with_seed(|seed22| {
            pre_wrapped_m9
                .draft_wrap(&actor22, 300, ep_commons_1, &key_commons_1, rec9)
                .unwrap()
                .sign(seed22, inc22)
        })
        .unwrap();
    pre_wrapped_m9.insert(wrap9_1).unwrap();
    let (ep_commons_2, key_commons_2) = b22.device().with_seed(|seed22| {
        let (draft, key) = pre_wrapped_m9
            .draft_epoch(&actor22, 301, EpochAudience::Commons, 50)
            .unwrap();
        (draft.sign(seed22, inc22).unwrap(), key)
    });
    let ep_commons_2_id = *ep_commons_2.id();
    pre_wrapped_m9.insert(ep_commons_2).unwrap();
    let wrap22_2 = b22
        .device()
        .with_seed(|seed22| {
            pre_wrapped_m9
                .draft_wrap(
                    &actor22,
                    302,
                    ep_commons_2_id,
                    &key_commons_2,
                    id22_early.recipient(),
                )
                .unwrap()
                .sign(seed22, inc22)
        })
        .unwrap();
    pre_wrapped_m9.insert(wrap22_2).unwrap();
    let admit9 = b22
        .device()
        .with_seed(|seed22| {
            pre_wrapped_m9
                .draft(
                    &actor22,
                    303,
                    Body::Admit {
                        entrant: Entrant {
                            request: req9.clone(),
                            node_key: [0x99; 32],
                            name: "m9".to_owned(),
                            label: "m9".to_owned(),
                        },
                        evidence: Evidence::Manual,
                    },
                )
                .sign(seed22, inc22)
        })
        .unwrap();
    pre_wrapped_m9.insert(admit9).unwrap();
    let wrap9_2 = b22
        .device()
        .with_seed(|seed22| {
            pre_wrapped_m9
                .draft_wrap(&actor22, 304, ep_commons_2_id, &key_commons_2, rec9)
                .unwrap()
                .sign(seed22, inc22)
        })
        .unwrap();
    pre_wrapped_m9.insert(wrap9_2).unwrap();
    let ctrl_plain = opened_early.commons_control.as_ref().unwrap();
    let put_commons_2 = b22
        .device()
        .with_seed(|seed22| {
            pre_wrapped_m9
                .draft_commons_put(
                    &actor22,
                    305,
                    ep_commons_2_id,
                    &key_commons_2,
                    ctrl_plain.as_slice(),
                )
                .unwrap()
                .sign(seed22, inc22)
        })
        .unwrap();
    pre_wrapped_m9.insert(put_commons_2).unwrap();
    save_records(
        &m9,
        &encode_records(&pre_wrapped_m9.records().cloned().collect::<Vec<_>>()),
    )
    .unwrap();
    let witness9_wire = Bootstrap::required(&m9)
        .unwrap()
        .log_witness()
        .unwrap()
        .expect("m9 log witness");
    let decoded9 =
        crate::membership_log::decode_records(&encode_records(witness9_wire.records())).unwrap();
    let mut iter9 = decoded9.into_iter();
    let mut verify_log9 = Log::new(iter9.next().unwrap()).unwrap();
    for r in iter9 {
        verify_log9.insert(r).unwrap();
    }
    assert!(verify_log9.has_historical_seat(req9.binding()));
    assert!(!verify_log9.audience_left_out(&EpochAudience::Commons, &req9.binding().actor()));
    assert!(!verify_log9.audience_left_out_recipient(&EpochAudience::Commons, &rec9));

    // Verify `overlay_log_onto_document` adds a log-admitted member under `member.name` ("b3")
    // before falling back to `member.label` ("m8").
    let id22 = read_identity(m22).unwrap().unwrap();
    let mut doc = admin_open(m22).unwrap().doc.clone();
    let changed =
        log_entries::overlay_log_onto_document(&removed_m7_log, &id22, &mut doc, None).unwrap();
    assert!(changed);
    assert!(
        doc.machines().iter().any(|m| m.name == "b3"),
        "overlay_log_onto_document must preserve member.name ('b3') over member.label ('m8')"
    );

    macs.server.stop();
}

#[test]
fn configured_log_member_observation_is_stable_across_peer_records_and_concurrent_gap_closers_serialize()
 {
    use crate::commons::{Observation, Unavailable, bootstrap::Device};
    use crate::membership_log::{Entrant, EpochAudience, Evidence};
    use std::sync::{Arc, Barrier};

    let macs = two_macs(&[(Audience::Everyone, "shared", b"v1".as_slice())]);
    let (m22, m7) = (&macs.m22, &macs.m7);
    let (b22, _, _) = member_key(m22);
    let (b7, actor7, inc7) = member_key(m7);
    let trust = b22.trust();

    let m8 = Paths::for_home(&macs.root.path().join("m8"));
    configure_log_member(&m8, &format!("{}#{}", macs.url, macs.signer), "m8").unwrap();
    let _ = machine_identity(&m8).unwrap();
    let req8 = Device::open(&m8)
        .unwrap()
        .unwrap()
        .request(trust.network, trust.authority)
        .unwrap();
    let actor8 = req8.binding().actor();
    let rec8 = req8.binding().recipient();
    admit_member_in_log(
        m22,
        Entrant {
            request: req8,
            node_key: [0x88; 32],
            name: "m8".to_owned(),
            label: "m8".to_owned(),
        },
        Evidence::Phrase,
    )
    .unwrap();
    save_records(&m8, &load_records(m22).unwrap().encode()).unwrap();

    let Observation::Visible(snap_before) = crate::commons::observe(&m8).unwrap() else {
        panic!("configured log member m8 must observe Commons");
    };

    // Before `m7` learns of `m8`, `m7` mints a concurrent `Commons` epoch on its own branch.
    let id7 = read_identity(m7).unwrap().unwrap();
    let mut log7 = load_records(m7).unwrap().log().unwrap().clone();
    let ctrl = log7.open_commons_control(&id7).unwrap().unwrap();
    let branch_records = b7
        .device()
        .with_seed(|seed7| -> crate::Result<Vec<_>> {
            let (ep_draft, ep_key) = log7.draft_epoch(&actor7, 300, EpochAudience::Commons, 2)?;
            let ep_rec = ep_draft.sign(seed7, inc7)?;
            let ep_id = *ep_rec.id();
            log7.insert(ep_rec.clone())?;
            let mut recs = vec![ep_rec];
            for member in log7.membership().members().values() {
                let wrap = log7
                    .draft_wrap(&actor7, 300, ep_id, &ep_key, member.binding.recipient())?
                    .sign(seed7, inc7)?;
                log7.insert(wrap.clone())?;
                recs.push(wrap);
            }
            let put = log7
                .draft_commons_put(&actor7, 300, ep_id, &ep_key, ctrl.as_slice())?
                .sign(seed7, inc7)?;
            log7.insert(put.clone())?;
            recs.push(put);
            Ok(recs)
        })
        .unwrap();
    let branch_wire = crate::membership_log::encode_records(&branch_records);
    save_records(&m8, &branch_wire).unwrap();
    save_records(m22, &branch_wire).unwrap();

    // `m8`'s snapshot observation stays stable across new peer records when its control descriptor
    // is unchanged, preventing `Runtime::serve_mode` from tearing down its active transport.
    let Observation::Visible(snap_after) = crate::commons::observe(&m8).unwrap() else {
        panic!("configured log member m8 must remain visible across peer records");
    };
    assert!(snap_before.same_observation(&snap_after));

    // `m7`'s epoch, minted before it learned of `m8`, leaves `m8` a wrap gap. Four concurrent
    // gap closers on `m22`, each past every rank's step, serialize cleanly under the authoring
    // lock and close it once rather than racing at the same sequence: each looks again under the
    // lock. Where `m22` is not the first ranked holder, one look before them records when it
    // first saw the gap.
    let (_, actor22, _) = member_key(m22);
    let gapped = load_records(m22).unwrap().log().unwrap().clone();
    let opened = gapped
        .open_audience_entries(&read_identity(m22).unwrap().unwrap())
        .unwrap()
        .unwrap();
    let gaps = gapped.wrap_gaps(
        &opened,
        &crate::membership_log::admission_audiences(&opened),
    );
    assert!(
        gaps.iter().any(|gap| gap.recipient == rec8),
        "CONTROL: m7's epoch leaves m8 a wrap gap"
    );
    let rank = gaps
        .iter()
        .filter_map(|gap| gapped.gap_rank(&opened, gap, &actor22))
        .max()
        .expect("m22 holds a rank for the gap");
    let later = if rank == 0 {
        1
    } else {
        assert!(
            log_entries::close_wrap_gaps(m22, 1).unwrap().is_empty(),
            "a holder ranked after the first waits its step"
        );
        1 + 100 * log_entries::GAP_STEP_MS
    };
    let barrier = Arc::new(Barrier::new(4));
    let mut handles = Vec::new();
    for _ in 0..4 {
        let paths = m22.clone();
        let barrier = Arc::clone(&barrier);
        handles.push(std::thread::spawn(move || {
            barrier.wait();
            // A look that finds another closer holding the state lock is busy, and leaves the
            // gap to the next pass.
            match log_entries::close_wrap_gaps(&paths, later) {
                Ok(signed) => signed.len(),
                Err(error) => {
                    assert!(error.to_string().contains("busy"), "{error}");
                    0
                }
            }
        }));
    }
    let minted_counts: Vec<usize> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert_eq!(
        minted_counts.iter().filter(|&&n| n > 0).count(),
        1,
        "exactly one concurrent gap closer should sign the wrap to m8: {minted_counts:?}"
    );
    let id22 = read_identity(m22).unwrap().unwrap();
    let merged_log = load_records(m22).unwrap().log().unwrap().clone();
    let opened22 = merged_log.open_audience_entries(&id22).unwrap().unwrap();
    assert_eq!(
        opened22
            .maximal_epochs
            .get(&EpochAudience::Commons)
            .map(Vec::len),
        Some(1)
    );

    // Removing `m8` in the log immediately revokes `m8`'s offline Commons observation.
    remove_member_in_log(m22, actor8).unwrap();
    save_records(&m8, &load_records(m22).unwrap().encode()).unwrap();
    assert!(matches!(
        crate::commons::observe(&m8).unwrap(),
        Observation::Unavailable(Unavailable::Revoked)
    ));

    macs.server.stop();
}

/// A Mac that has never been in the vault at `root/<name>`, its self-signed request for the
/// Commons network `member` belongs to, and `member`'s Allow of it (synthetic keys).
pub(super) fn newcomer(
    root: &Path,
    member: &Paths,
    name: &str,
) -> (
    Paths,
    crate::commons::binding::Request,
    crate::members::Allowed,
) {
    let paths = Paths::for_home(&root.join(name));
    machine_identity(&paths).expect("identity");
    let device = crate::commons::bootstrap::Device::open(&paths)
        .expect("device")
        .expect("an identity");
    let trust = Bootstrap::required(member).expect("member").trust();
    let request = device
        .request(trust.network, trust.authority)
        .expect("request");
    let allowed = Bootstrap::required(member)
        .expect("member")
        .device()
        .with_seed(|seed| crate::members::Allowed::sign(request.binding(), name, seed))
        .expect("allowed");
    (paths, request, allowed)
}

/// What `member`'s desk hands the Mac `request` names after a person's Allow and the phrase:
/// [`hand_over`], as the window's desk runs it.
pub(super) fn hand(
    member: &Paths,
    request: &crate::commons::binding::Request,
    allowed: &crate::members::Allowed,
    reads_log: bool,
) -> (crate::members::Bundle, bool) {
    let handout = hand_over(
        member,
        &crate::members::desk::Handing {
            request,
            node: request.binding().recipient().0,
            name: allowed.name(),
            allowed,
            reader: if reads_log {
                crate::members::bundle::Reader::LegacyLog
            } else {
                crate::members::bundle::Reader::Files
            },
            restore: false,
            node_proven: false,
            held: None,
        },
    )
    .expect("hand over");
    (
        crate::members::Bundle::decode(handout.bundle.as_slice()).expect("bundle"),
        handout.logged,
    )
}

/// A member's desk takes this Mac's state before a run touches its phrase, and the bundle built
/// under that hold still names the swarm policy's servers after the source, on the log path and
/// on F10's. Read under the hold, an entry read takes the state shared on a descriptor of its own,
/// which the hold refuses: the policy was not found, and the bundle named the source alone.
///
/// NEGATIVE CONTROL: the hold read no policy before it took the state (`policy: Vec::new()` in
/// `take_handover_hold`), so the bundle reads it under the hold: fails with "a bundle built under
/// the hold (log: true) names the policy's server: [\"http://127.0.0.1:…/v1/alab\"]".
#[test]
fn a_bundle_built_under_the_desks_hold_names_the_policys_servers() {
    // An `.invalid` name (RFC 6761) fails at once: a `.local` one waited out an mDNS lookup of
    // five seconds on every sync that dialled it, 40 s of this test.
    let policy_url = "http://policy-a.invalid:7484/v1/alab".to_owned();
    let policy = crate::swarm_roster::SwarmPolicy {
        servers: vec![policy_url.clone()],
        ..crate::swarm_roster::SwarmPolicy::default()
    };
    let toml = policy.to_toml();
    let macs = two_macs(&[
        (Audience::Everyone, "shared", b"everyone-v1".as_slice()),
        (
            Audience::Everyone,
            crate::SWARM_POLICY_ENTRY,
            toml.as_bytes(),
        ),
    ]);
    let m7 = &macs.m7;
    assert!(
        policy_peer_urls(m7, &configured(m7).unwrap()).contains(&policy_url),
        "CONTROL: m7 reads the policy's server"
    );
    for (reads_log, name) in [(true, "m40"), (false, "m41")] {
        let (_, request, allowed) = newcomer(macs.root.path(), m7, name);
        let hold = take_handover_hold(m7).unwrap().expect("m7's state");
        let handout = hand_over(
            m7,
            &crate::members::desk::Handing {
                request: &request,
                node: request.binding().recipient().0,
                name: allowed.name(),
                allowed: &allowed,
                reader: if reads_log {
                    crate::members::bundle::Reader::LegacyLog
                } else {
                    crate::members::bundle::Reader::Files
                },
                restore: false,
                node_proven: false,
                held: Some(&hold),
            },
        )
        .expect("hand over");
        drop(hold);
        assert_eq!(handout.logged, reads_log, "CONTROL: the path taken");
        let bundle = crate::members::Bundle::decode(handout.bundle.as_slice()).expect("bundle");
        assert!(
            bundle.servers.contains(&policy_url),
            "a bundle built under the hold (log: {reads_log}) names the policy's server: {:?}",
            bundle.servers
        );
    }
    macs.server.stop();
}

#[test]
fn bounded_member_cached_head_applies_new_admission_and_removal_without_a_server() {
    let macs = two_macs(&[(Audience::Everyone, "shared", b"everyone-v1".as_slice())]);
    let (m22, m7) = (&macs.m22, &macs.m7);
    let (m40, request, allowed) = newcomer(macs.root.path(), m7, "m40");
    join_host(&m40, &format!("{}#{}", macs.url, macs.signer), "m40");
    assert_eq!(load_state(&m40).unwrap().membership, Stored::Pending);
    assert!(held_copy(&m40, &configured(&m40).unwrap(), &load_state(&m40).unwrap()).is_none());

    let (_, logged) = hand(m7, &request, &allowed, true);
    assert!(logged);
    let admission = load_records(m7).unwrap().encode();
    save_records(&m40, &admission).unwrap();
    save_records(m22, &admission).unwrap();
    remove_member_in_log(m22, request.binding().actor()).expect("signed removal");
    let removal = load_records(m22).unwrap().encode();
    macs.server.stop();

    let config = configured(&m40).unwrap();
    let outcome = apply_fetched_until(
        &m40,
        config,
        load_state(&m40).unwrap(),
        Fetched::Missing,
        None,
        Some(Instant::now() + aterm_spec::wait::MUST_HAPPEN),
    )
    .expect("cached pending envelope applies the signed admission locally");
    assert!(matches!(outcome, SyncOutcome::Applied { .. }));
    assert_eq!(load_state(&m40).unwrap().membership, Stored::In);
    assert_eq!(
        std::fs::read(m40.home.join(".secrets/shared")).unwrap(),
        b"everyone-v1"
    );

    save_records(&m40, &removal).unwrap();
    assert_eq!(
        sync_reader_until(&m40, Instant::now() + aterm_spec::wait::MUST_HAPPEN)
            .expect("unreachable envelope server cannot delay a signed removal"),
        SyncOutcome::NotIn,
    );
    assert_eq!(load_state(&m40).unwrap().membership, Stored::Revoked);
    assert!(
        read_verified_entry(&m40, &configured(&m40).unwrap(), "shared")
            .unwrap()
            .is_none()
    );
}

/// F15: m7, a plain member that holds no admin locker, lets m40 in by itself. m40 reads its
/// files and F18's Commons control from the log at once, follows the vault from the log with the
/// admin never acting for it, an admin's later publish keeps the epoch m7 wrapped to m40 rather
/// than minting one that leaves m40 out, and m40 lets the next Mac in itself.
///
/// NEGATIVE CONTROL: make `admission_audiences` return `[everyone]` alone: m7 wraps no Commons
/// epoch to m40 and the `"m40 reads Commons control from the log"` assert fails.
///
/// Dropping the `plan_recipients.extend(log_recipients…)` union in `draft_document_diff` no
/// longer fails this test: since F14's round 2 the admin's publish drafts again with m40 among
/// `[everyone]`'s recipients and keeps the epoch m7 wrapped to it either way.
///
/// NEGATIVE CONTROL: in `log_base`, read `Stamp::NoLocker` as `LogBase::Unstamped` (F14's stamp
/// rule alone): m40, which no envelope locker names, reads its absent lockers on its first sync
/// and the `"m40 stays in after it syncs"` assert fails.
#[test]
fn a_member_without_the_admin_locker_admits_a_mac() {
    let macs = two_macs(&[(Audience::Everyone, "shared", b"everyone-v1".as_slice())]);
    let (m22, m7) = (&macs.m22, &macs.m7);
    assert!(admin_open(m7).is_err(), "m7 holds no admin locker");

    let (m40, request, allowed) = newcomer(macs.root.path(), m7, "m40");
    let (bundle, logged) = hand(m7, &request, &allowed, true);
    assert!(logged, "a member seated in the log hands the log");
    assert!(
        bundle.files().is_none(),
        "the log path hands no [everyone] files"
    );
    assert!(
        post_records_to_servers(m7).expect("posted") >= 1,
        "the server took m7's records"
    );

    write_from_log_bundle(&m40, &bundle).expect("m40 writes from the log");
    assert_eq!(load_state(&m40).unwrap().membership, Stored::In);
    assert_eq!(
        std::fs::read(m40.home.join(".secrets/shared")).unwrap(),
        b"everyone-v1"
    );
    assert!(
        holds_allowed(&m40),
        "m40 keeps the Allowed for an admin's approver, which reads nothing for it"
    );
    assert!(
        matches!(
            observe_commons(&m40).unwrap(),
            crate::commons::Observation::Visible(_)
        ),
        "m40 reads Commons control from the log"
    );
    let store = load_records(&m40).unwrap();
    let log = store.log().unwrap();
    let membership = log.membership();
    let (_, m7_actor, _) = member_key(m7);
    let me = membership
        .get(&request.binding().actor())
        .expect("the log lets m40 in");
    assert_eq!(me.label, "m40", "m7 kept the label m40 asked under");
    assert_eq!(me.admitted_by, Some(m7_actor));

    // m40 follows the vault from the log: a sync keeps it in.
    sync(&m40, None).expect("m40 syncs");
    assert_eq!(
        load_state(&m40).unwrap().membership,
        Stored::In,
        "m40 stays in after it syncs"
    );

    // The admin, which never acted for m40, changes `shared` later. Its publish sees m40 in the
    // log, so the epoch m7 wrapped to m40 stays the one that seals the new value.
    sync(m22, None).expect("m22 pulls m7's records");
    let admin = admin_open(m22).unwrap();
    let mut doc = admin.doc.clone();
    doc.set_entry(&Audience::Everyone, "shared", b"everyone-v2")
        .unwrap();
    admin_publish(m22, &admin, &doc).expect("m22 publishes");
    sync(&m40, None).expect("m40 syncs the admin's change");
    assert_eq!(
        std::fs::read(m40.home.join(".secrets/shared")).unwrap(),
        b"everyone-v2",
        "m40 reads the admin's later value"
    );

    // m40 is a whole member: it lets the next Mac in from the log itself.
    let (m42, request42, allowed42) = newcomer(macs.root.path(), &m40, "m42");
    let (bundle42, logged42) = hand(&m40, &request42, &allowed42, true);
    assert!(logged42, "m40, seated by m7's Admit, hands the log");
    write_from_log_bundle(&m42, &bundle42).expect("m42 takes m40's Admit");
    assert_eq!(
        std::fs::read(m42.home.join(".secrets/shared")).unwrap(),
        b"everyone-v2"
    );

    // Another Mac asking under a label the log holds gets the next free one.
    let (_, request2, allowed2) = newcomer(macs.root.path(), m7, "m40-twin");
    let mut allowed2 = allowed2;
    allowed2 = Bootstrap::required(m7)
        .unwrap()
        .device()
        .with_seed(|seed| crate::members::Allowed::sign(request2.binding(), "m40", seed))
        .unwrap_or(allowed2);
    let (_, logged2) = hand(m7, &request2, &allowed2, true);
    assert!(logged2);
    assert_eq!(
        load_records(m7)
            .unwrap()
            .log()
            .unwrap()
            .membership()
            .label(&request2.binding().actor()),
        Some("m41")
    );

    macs.server.stop();
    drop(macs.root);
}

/// A run whose bundle never arrived is repeated with another member: m7 lets m40 in but its
/// bundle is lost; m22, once it holds m7's `Admit`, hands m40 that `Admit` (with only the
/// `Wrap`s m40 lacks, here none), and m40 takes it although m22, not m7, ran the phrase with it.
///
/// NEGATIVE CONTROL: in `write_from_log_bundle`, refuse an `Admit` signed by another member than
/// the one whose `Allowed` the bundle carries: m40 refuses m22's bundle and the `"m40 takes m7's
/// Admit from m22"` expect fails.
#[test]
fn a_mac_whose_bundle_was_lost_takes_its_admit_from_another_member() {
    let macs = two_macs(&[(Audience::Everyone, "shared", b"everyone-v1".as_slice())]);
    let (m22, m7) = (&macs.m22, &macs.m7);
    let (m40, request, allowed) = newcomer(macs.root.path(), m7, "m40");
    let (lost, logged) = hand(m7, &request, &allowed, true);
    assert!(logged, "m7 hands the log");
    let crate::members::bundle::Handed::Log { admit, .. } = lost.handed else {
        panic!("m7 hands a log bundle");
    };
    post_records_to_servers(m7).expect("m7 posts its records");
    sync_records(m22, None).expect("m22 pulls m7's records");

    let by_m22 = Bootstrap::required(m22)
        .expect("m22")
        .device()
        .with_seed(|seed| crate::members::Allowed::sign(request.binding(), "m40", seed))
        .expect("allowed");
    let (again, logged) = hand(m22, &request, &by_m22, true);
    assert!(logged, "m22 hands the log");
    match &again.handed {
        crate::members::bundle::Handed::Log { admit: handed, .. } => {
            assert_eq!(*handed, admit, "m22 hands m7's Admit, not a second one");
        }
        crate::members::bundle::Handed::Files(_) => panic!("m22 hands a log bundle"),
    }
    write_from_log_bundle(&m40, &again).expect("m40 takes m7's Admit from m22");
    assert_eq!(load_state(&m40).unwrap().membership, Stored::In);
    assert_eq!(
        std::fs::read(m40.home.join(".secrets/shared")).unwrap(),
        b"everyone-v1"
    );

    macs.server.stop();
    drop(macs.root);
}

/// F15's split on real Macs (the client-level form of the F8 lane's check, whose lane vault has
/// no log yet): while m22 (the LAN) and m7 (a laptop away from it) share nothing, each lets a
/// new Mac in from its own log, m22 lets in m41 and m7 lets in m40, and each new Mac reads its
/// files at once. After the heal both `Admit`s hold on every Mac, both new Macs stay in through
/// their first syncs, and both read the admin's next value.
///
/// NEGATIVE CONTROL: in `log_base`, read `Stamp::NoLocker` as `LogBase::Unstamped`: neither new
/// Mac has an envelope locker, so each reads `Revoked` at its first sync after the heal and the
/// `"m40 stays in after the heal"` assert fails.
#[test]
fn a_lan_and_a_laptop_each_admit_a_mac_and_both_stay_in_after_the_heal() {
    let macs = two_macs(&[(Audience::Everyone, "shared", b"everyone-v1".as_slice())]);
    let (m22, m7) = (&macs.m22, &macs.m7);

    // The cut: neither member posts or pulls, so each admits from the log it holds.
    let (m40, request40, allowed40) = newcomer(macs.root.path(), m7, "m40");
    let (bundle40, logged40) = hand(m7, &request40, &allowed40, true);
    let (m41, request41, allowed41) = newcomer(macs.root.path(), m22, "m41");
    let (bundle41, logged41) = hand(m22, &request41, &allowed41, true);
    assert!(logged40 && logged41, "both members hand the log");
    write_from_log_bundle(&m40, &bundle40).expect("m40 writes from m7's log");
    write_from_log_bundle(&m41, &bundle41).expect("m41 writes from m22's log");
    for mac in [&m40, &m41] {
        assert_eq!(
            std::fs::read(mac.home.join(".secrets/shared")).unwrap(),
            b"everyone-v1"
        );
    }
    let m7_log = load_records(m7).unwrap().log().unwrap().membership();
    assert!(
        m7_log.get(&request41.binding().actor()).is_none(),
        "m7 has not seen m41"
    );

    // The heal: each member's records reach the server and every Mac pulls.
    post_records_to_servers(m7).expect("m7 posts");
    post_records_to_servers(m22).expect("m22 posts");
    for mac in [m22, m7, &m40, &m41] {
        sync(mac, None).expect("sync after the heal");
    }
    for mac in [m22, m7, &m40, &m41] {
        let membership = load_records(mac).unwrap().log().unwrap().membership();
        assert_eq!(membership.label(&request40.binding().actor()), Some("m40"));
        assert_eq!(membership.label(&request41.binding().actor()), Some("m41"));
    }
    assert_eq!(
        load_state(&m40).unwrap().membership,
        Stored::In,
        "m40 stays in after the heal"
    );
    assert_eq!(
        load_state(&m41).unwrap().membership,
        Stored::In,
        "m41 stays in after the heal"
    );

    // The admin's next value reaches both.
    let admin = admin_open(m22).unwrap();
    let mut doc = admin.doc.clone();
    doc.set_entry(&Audience::Everyone, "shared", b"everyone-v2")
        .unwrap();
    admin_publish(m22, &admin, &doc).expect("m22 publishes");
    for mac in [&m40, &m41] {
        sync(mac, None).expect("sync the admin's change");
        assert_eq!(
            std::fs::read(mac.home.join(".secrets/shared")).unwrap(),
            b"everyone-v2",
            "both new Macs read the admin's later value"
        );
        assert_eq!(load_state(mac).unwrap().membership, Stored::In);
    }

    macs.server.stop();
    drop(macs.root);
}

/// `member`'s desk running [`hand_over`] for the Mac `request` names, as the window's does,
/// returning its result rather than a bundle.
pub(super) fn hand_result(
    member: &Paths,
    request: &crate::commons::binding::Request,
    allowed: &crate::members::Allowed,
    reads_log: bool,
) -> crate::Result<crate::members::desk::Handout> {
    hand_over(
        member,
        &crate::members::desk::Handing {
            request,
            node: request.binding().recipient().0,
            name: allowed.name(),
            allowed,
            reader: if reads_log {
                crate::members::bundle::Reader::LegacyLog
            } else {
                crate::members::bundle::Reader::Files
            },
            restore: false,
            node_proven: false,
            held: None,
        },
    )
}

/// A Mac the log removed is not let back in by a member's Allow, on either path: the log path
/// would sign a restoring `Admit` and wrap the epochs minted after the removal, and F10's would
/// hand it the current files. A new device key under the removed Mac's machine key is the same
/// Mac. The run ends with the reason, until F16's "removed, Allow again?" row asks the person.
///
/// NEGATIVE CONTROL: make `kept_out` return `None`: m7 signs a restoring `Admit` for m40 and
/// the `"an Allow of a removed Mac"` expect_err fails.
#[test]
fn a_mac_the_log_removed_is_not_let_back_in_by_a_members_allow() {
    let macs = two_macs(&[(Audience::Everyone, "shared", b"everyone-v1".as_slice())]);
    let (m22, m7) = (&macs.m22, &macs.m7);
    let (m40, request, allowed) = newcomer(macs.root.path(), m7, "m40");
    let (bundle, logged) = hand(m7, &request, &allowed, true);
    assert!(logged);
    write_from_log_bundle(&m40, &bundle).expect("m40 writes from the log");
    post_records_to_servers(m7).expect("m7 posts the admission");
    sync_records(m22, None).expect("m22 pulls it");

    let actor = request.binding().actor();
    remove_member_in_log(m22, actor).expect("m22 removes m40");
    sync_records(m7, None).expect("m7 pulls the removal");
    let removed = || {
        load_records(m7)
            .unwrap()
            .log()
            .unwrap()
            .membership()
            .removal(&actor)
            .is_some()
    };
    assert!(removed(), "CONTROL: m7 holds the removal");

    for reads_log in [true, false] {
        let error = hand_result(m7, &request, &allowed, reads_log)
            .err()
            .expect("an Allow of a removed Mac");
        assert!(error.to_string().contains("removed"), "{error}");
    }
    assert!(removed(), "m7 signed no restoring Admit");

    // A new device key under m40's machine key: the log removed that recipient.
    let again = Paths::for_home(&macs.root.path().join("m40-again"));
    clone_identity_to(&m40, &again);
    let device = crate::commons::bootstrap::Device::open(&again)
        .expect("device")
        .expect("an identity");
    let trust = Bootstrap::required(m7).expect("m7").trust();
    let request2 = device
        .request(trust.network, trust.authority)
        .expect("request");
    assert_ne!(
        request2.binding().actor(),
        actor,
        "CONTROL: a new device key"
    );
    let allowed2 = Bootstrap::required(m7)
        .unwrap()
        .device()
        .with_seed(|seed| crate::members::Allowed::sign(request2.binding(), "m40", seed))
        .unwrap();
    let error = hand_result(m7, &request2, &allowed2, true)
        .err()
        .expect("an Allow of a removed Mac's machine key");
    assert!(error.to_string().contains("removed"), "{error}");

    // Another Mac, with a fleet node key of its own (`hand`'s is m40's, which the removal keeps
    // out), is still let in.
    let (_, request3, allowed3) = newcomer(macs.root.path(), m7, "m41");
    let handout = hand_over(
        m7,
        &crate::members::desk::Handing {
            request: &request3,
            node: [0x41; 32],
            name: allowed3.name(),
            allowed: &allowed3,
            reader: crate::members::bundle::Reader::LegacyLog,
            restore: false,
            node_proven: false,
            held: None,
        },
    )
    .expect("hand over m41");
    assert!(handout.logged);

    macs.server.stop();
    drop(macs.root);
}

/// A Mac that asks under `m99999` leaves the next Mac a valid label. A request that cannot
/// enter the log is refused without handing the vault's files or recording an admission.
#[test]
fn a_failed_log_path_is_refused_and_m99999_leaves_a_label() {
    let macs = two_macs(&[(Audience::Everyone, "shared", b"everyone-v1".as_slice())]);
    let m7 = &macs.m7;
    let (_, request, allowed) = newcomer(macs.root.path(), m7, "m99999");
    assert!(hand(m7, &request, &allowed, true).1);
    let (_, request2, allowed2) = newcomer(macs.root.path(), m7, "andrews-macbook-air");
    assert!(
        hand(m7, &request2, &allowed2, true).1,
        "the next Mac is let in"
    );
    let label = load_records(m7)
        .unwrap()
        .log()
        .unwrap()
        .membership()
        .label(&request2.binding().actor())
        .map(str::to_owned)
        .expect("labelled");
    assert!(crate::membership_log::is_label(&label), "{label}");

    // An Admit the log refuses (a request bound to another Commons network, which the desk turns
    // away before a run): the log path fails without a files fallback.
    let stray = Paths::for_home(&macs.root.path().join("stray"));
    machine_identity(&stray).expect("identity");
    let request3 = crate::commons::bootstrap::Device::open(&stray)
        .expect("device")
        .expect("an identity")
        .request([0x99; 16], [0x98; 32])
        .expect("request");
    let allowed3 = Bootstrap::required(m7)
        .unwrap()
        .device()
        .with_seed(|seed| crate::members::Allowed::sign(request3.binding(), "m41", seed))
        .unwrap();
    assert!(hand_result(m7, &request3, &allowed3, true).is_err());
    assert!(
        load_records(m7)
            .unwrap()
            .log()
            .unwrap()
            .membership()
            .get(&request3.binding().actor())
            .is_none(),
        "CONTROL: the Admit the failed path signed was never kept"
    );

    macs.server.stop();
    drop(macs.root);
}

fn filled_join_log(member: &Paths, count: usize) -> crate::membership_log::Log {
    use crate::membership_log::Body;
    let bootstrap = Bootstrap::required(member).unwrap();
    let request = bootstrap.request().unwrap();
    let mut log = load_records(member).unwrap().log().unwrap().clone();
    bootstrap
        .device()
        .with_seed(|seed| {
            while log.held_count() < count {
                let count = log.held_count();
                let record = log
                    .draft(
                        &request.binding().actor(),
                        count as u64 + 10,
                        Body::Name {
                            name: format!("join-source-{count}"),
                        },
                    )
                    .sign(seed, *request.binding().incarnation())?;
                log.insert(record)?;
            }
            Ok::<(), crate::Error>(())
        })
        .unwrap();
    log
}

#[test]
fn join_reader_capacity_is_checked_before_and_after_admission() {
    use crate::crossing::RecordsBound;
    use crate::members::bundle::{Handed, Reader};
    use crate::members::desk::Handing;
    use crate::membership_log::{LEGACY_RECORDS, RecordBudgets};
    const LARGER: RecordBudgets = RecordBudgets {
        held: 8192,
        ..RecordBudgets::PRODUCTION
    };
    LARGER.within(|| {
        let macs = two_macs(&[(Audience::Everyone, "shared", b"everyone-v1".as_slice())]);
        let member = &macs.m7;
        let log = filled_join_log(member, LEGACY_RECORDS - 1);
        save_records(member, &encode_records(log.records())).unwrap();
        let before = std::fs::read(member.records_file()).unwrap();
        assert!(!records_marked_crossed(member).unwrap());
        let (_, request, allowed) = newcomer(macs.root.path(), member, "m40");
        let hand_to = |reader| {
            hand_over(
                member,
                &Handing {
                    request: &request,
                    node: *request.binding().writer(),
                    name: allowed.name(),
                    allowed: &allowed,
                    reader,
                    restore: false,
                    node_proven: false,
                    held: None,
                },
            )
        };
        // Each refusal names the Mac to update: the new Mac, an older aterm or one whose bound
        // is short of the admission's crossed history.
        let update = |why: &str| assert!(why.contains("update aterm on the new Mac"), "{why}");
        for reader in [
            Reader::LegacyLog,
            Reader::BoundedLog(RecordsBound::of(4096, false)),
        ] {
            update(&hand_to(reader).err().expect("refused").to_string());
            assert!(
                std::fs::read(member.records_file()).unwrap() == before,
                "a refused phrase admission must not change records"
            );
            assert!(!records_marked_crossed(member).unwrap());
            let proven = crate::client::Proven {
                request: &request,
                node: *request.binding().writer(),
                name: allowed.name(),
                reader,
                fleet: true,
                lan: true,
                fresh: true,
                book: crate::client::Book {
                    moved_seats_read: true,
                    ..crate::client::Book::default()
                },
            };
            let proof_result =
                crate::client::admit_by_proof(member, &proven, aterm_time::unix_millis().unwrap());
            assert!(
                std::fs::read(member.records_file()).unwrap() == before,
                "a refused proof admission must not change records"
            );
            assert!(!records_marked_crossed(member).unwrap());
            let Ok(crate::client::Readmit::Refused(why)) = proof_result else {
                panic!("a refused proof admission");
            };
            update(&why);
        }
        let proven = crate::client::Proven {
            request: &request,
            node: *request.binding().writer(),
            name: allowed.name(),
            reader: Reader::BoundedLog(RecordsBound::here(false)),
            fleet: true,
            lan: true,
            fresh: true,
            book: crate::client::Book {
                moved_seats_read: true,
                ..crate::client::Book::default()
            },
        };
        assert!(matches!(
            crate::client::admit_by_proof(member, &proven, aterm_time::unix_millis().unwrap())
                .unwrap(),
            crate::client::Readmit::Let { .. }
        ));
        let handout = hand_to(Reader::BoundedLog(RecordsBound::here(false))).unwrap();
        assert!(handout.logged);
        let Handed::Log { source, .. } = crate::members::Bundle::decode(handout.bundle.as_slice())
            .unwrap()
            .handed
        else {
            panic!("bounded reader requires the log bundle");
        };
        assert_eq!(source, Some(RecordsBound::here(true)));
        assert!(records_marked_crossed(member).unwrap());
        let after = std::fs::read(member.records_data_file().unwrap()).unwrap();
        let refused = hand_to(Reader::LegacyLog)
            .err()
            .expect("refused")
            .to_string();
        assert!(refused.contains("an older aterm"), "{refused}");
        update(&refused);
        assert_eq!(
            std::fs::read(member.records_data_file().unwrap()).unwrap(),
            after
        );
        macs.server.stop();
    });
}

#[test]
fn join_bundles_keep_waiting_records_local_and_fit_the_advertised_history() {
    use crate::crossing::RecordsBound;
    use crate::members::bundle::{Handed, Reader};
    use crate::members::desk::Handing;
    use crate::membership_log::{Body, LEGACY_RECORDS, RecordBudgets, decode_records};
    RecordBudgets {
        held: 8192,
        ..RecordBudgets::PRODUCTION
    }
    .within(|| {
        let macs = two_macs(&[(Audience::Everyone, "shared", b"everyone-v1".as_slice())]);
        let member = &macs.m7;
        let mut log = filled_join_log(member, LEGACY_RECORDS - 16);
        let complete = encode_records(log.records());
        let bootstrap = Bootstrap::required(member).unwrap();
        let author = bootstrap.request().unwrap();
        let pending = bootstrap
            .device()
            .with_seed(|seed| {
                let mut pending = Vec::new();
                for n in 0..33 {
                    let record = log
                        .draft(
                            &author.binding().actor(),
                            10_000 + n,
                            Body::Name {
                                name: format!("join-waiting-{n}"),
                            },
                        )
                        .sign(seed, *author.binding().incarnation())?;
                    log.insert(record.clone())?;
                    if n > 0 {
                        pending.push(record);
                    }
                }
                Ok::<_, crate::Error>(encode_records(&pending))
            })
            .unwrap();
        save_records(member, &complete).unwrap();
        save_records(member, &pending).unwrap();
        let (joining, request, allowed) = newcomer(macs.root.path(), member, "waiting-joiner");
        let mut received = None;
        for reader in [
            Reader::LegacyLog,
            Reader::BoundedLog(RecordsBound::of(4096, false)),
        ] {
            let handout = hand_over(
                member,
                &Handing {
                    request: &request,
                    node: *request.binding().writer(),
                    name: allowed.name(),
                    allowed: &allowed,
                    reader,
                    restore: false,
                    node_proven: false,
                    held: None,
                },
            )
            .unwrap();
            let bundle = crate::members::Bundle::decode(handout.bundle.as_slice()).unwrap();
            let proven = crate::client::Proven {
                request: &request,
                node: *request.binding().writer(),
                name: allowed.name(),
                reader,
                fleet: true,
                lan: true,
                fresh: true,
                book: crate::client::Book {
                    moved_seats_read: true,
                    ..crate::client::Book::default()
                },
            };
            let crate::client::Readmit::Let { bundle: proof, .. } =
                crate::client::admit_by_proof(member, &proven, aterm_time::unix_millis().unwrap())
                    .unwrap()
            else {
                panic!("the complete admission fits the reader");
            };
            let proof = crate::members::Bundle::decode(proof.as_slice()).unwrap();
            let source = load_records(member).unwrap();
            assert!(source.len() > LEGACY_RECORDS);
            assert!(source.complete_inventory().len() <= LEGACY_RECORDS);
            assert_eq!(source.len() - source.complete_inventory().len(), 32);
            for bundle in [&bundle, &proof] {
                let Handed::Log { records, .. } = &bundle.handed else {
                    panic!("the join hands a log");
                };
                let offered =
                    decode_records(records).expect("uncrossed history fits a legacy reader");
                assert_eq!(
                    offered
                        .iter()
                        .map(|r| *r.id())
                        .collect::<std::collections::BTreeSet<_>>(),
                    source.complete_inventory()
                );
            }
            received = Some(bundle);
        }
        RecordBudgets::PRODUCTION.within(|| {
            write_from_log_bundle(&joining, &received.unwrap()).unwrap();
            let retained = load_records(&joining).unwrap();
            assert_eq!(retained.len(), retained.complete_inventory().len());
            assert_eq!(
                std::fs::read(joining.home.join(".secrets/shared")).unwrap(),
                b"everyone-v1"
            );
        });
        macs.server.stop();
    });
}

/// A log bundle whose genesis `seed` signs, founded by a synthetic member that admits the Mac at
/// `joiner` under `m40`, wraps it an `[everyone]` epoch and puts `gh-token` = `value` under it.
fn forged_log_bundle(
    joiner: &Paths,
    seed: &[u8; 32],
    signer: [u8; 32],
    followed: Vec<[u8; 32]>,
    value: &[u8],
) -> crate::members::Bundle {
    use crate::membership_log::{Body, Log};
    const NETWORK: [u8; 16] = [0x71; 16];
    const AUTHORITY: [u8; 32] = [0x72; 32];
    let founder_identity = crate::age::Identity::from_secret_bytes([0x73; 32]);
    let founder_seed = [0x74; 32];
    let founder_incarnation = [0x75; 16];
    let founder = crate::commons::binding::Request::create(
        NETWORK,
        AUTHORITY,
        [0x76; 16],
        founder_incarnation,
        &founder_identity,
        &founder_seed,
    )
    .expect("founder request");
    let founder_actor = founder.binding().actor();
    let identity = machine_identity(joiner).expect("identity");
    let request = Device::open(joiner)
        .expect("device")
        .expect("an identity")
        .request(NETWORK, AUTHORITY)
        .expect("request");
    let genesis = Record::found(
        seed,
        vec![Entrant {
            request: founder,
            node_key: [0x77; 32],
            name: "m1".to_owned(),
            label: "m1".to_owned(),
        }],
        1_000,
    )
    .expect("genesis");
    let mut log = Log::new(genesis).expect("log");
    let sign = |log: &mut Log, record: Record| {
        log.insert(record.clone()).expect("insert");
        record
    };
    let admit = log
        .draft(
            &founder_actor,
            1_001,
            Body::Admit {
                entrant: Entrant {
                    request: request.clone(),
                    node_key: [0x40; 32],
                    name: "m40".to_owned(),
                    label: "m40".to_owned(),
                },
                evidence: Evidence::Phrase,
            },
        )
        .sign(&founder_seed, founder_incarnation)
        .expect("admit");
    let admit = sign(&mut log, admit);
    let (draft, key) = log
        .draft_epoch(&founder_actor, 1_002, Audience::Everyone, 1)
        .expect("epoch");
    let epoch = sign(
        &mut log,
        draft.sign(&founder_seed, founder_incarnation).unwrap(),
    );
    for recipient in [founder_identity.recipient(), identity.recipient()] {
        let wrap = log
            .draft_wrap(&founder_actor, 1_003, *epoch.id(), &key, recipient)
            .unwrap()
            .sign(&founder_seed, founder_incarnation)
            .unwrap();
        sign(&mut log, wrap);
    }
    let entry = Entry {
        name: "gh-token".to_owned(),
        value: SecretBytes::from(value),
        audience: Audience::Everyone,
        if_absent: false,
        anchor: None,
    };
    let put = log
        .draft_put(&founder_actor, 1_004, *epoch.id(), &key, &entry)
        .unwrap()
        .sign(&founder_seed, founder_incarnation)
        .unwrap();
    sign(&mut log, put);
    crate::members::Bundle {
        vault: crate::ALAB_VAULT.to_owned(),
        signer,
        followed,
        servers: vec!["http://127.0.0.1:9/v1/alab".to_owned()],
        allowed: crate::members::Allowed::sign(request.binding(), "m40", &founder_seed)
            .expect("allowed"),
        handed: crate::members::bundle::Handed::Log {
            source: None,
            admit: *admit.id(),
            records: encode_records(log.records()),
        },
    }
}

/// A new Mac takes ALab's genesis only from the keys its build names (until F12b; the compiled
/// digest after), never from a key the member's bundle names: a bundle that names a key of its
/// own as the vault's pin and the real vault key as followed, with a log that key founded, wrote
/// that log's values to `~/.secrets` (git and gh credentials) and followed its servers.
///
/// NEGATIVE CONTROL: build the joiner's store with `records_store_for` (the bundle's pin and
/// followed keys among those a dormant ALab store takes): the forged bundle is written and the
/// `"a genesis only the bundle names"` expect_err fails.
#[test]
fn a_new_mac_takes_alabs_genesis_only_from_keys_its_build_names() {
    use crate::envelope::{Retired, TEST_RETIRED, signer_public};
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            TEST_RETIRED.with(|cell| *cell.borrow_mut() = None);
        }
    }
    let vault_seed = [0x61; 32];
    let vault = signer_public(&vault_seed).expect("public");
    let forger_seed = [0x62; 32];
    let forger = signer_public(&forger_seed).expect("public");
    TEST_RETIRED.with(|cell| {
        *cell.borrow_mut() = Some(Retired {
            keys: Vec::new(),
            current: Some(vault),
        });
    });
    let _reset = Reset;
    let root = aterm_tempfile::tempdir().expect("tempdir");

    let joiner = Paths::for_home(&root.path().join("j"));
    let forged = forged_log_bundle(&joiner, &forger_seed, forger, vec![vault], b"forged");
    write_from_log_bundle(&joiner, &forged).expect_err("a genesis only the bundle names");
    assert!(!joiner.records_file().exists());
    assert!(!joiner.home.join(".secrets/gh-token").exists());
    assert!(config(&joiner).unwrap().is_none(), "no vault configured");

    // CONTROL: the same log under the vault key's genesis is taken (while no digest is
    // compiled; with one, the store checks the genesis by it alone).
    if crate::membership_log::genesis::compiled()
        .unwrap()
        .is_none()
    {
        let honest = Paths::for_home(&root.path().join("h"));
        let bundle = forged_log_bundle(&honest, &vault_seed, vault, Vec::new(), b"real");
        write_from_log_bundle(&honest, &bundle).expect("the vault key's genesis");
        assert_eq!(
            std::fs::read(honest.home.join(".secrets/gh-token")).unwrap(),
            b"real"
        );
    }
    drop(root);
}

/// A Mac another member removes in the log is not let back into the vault by its Allow, though
/// the admin's approver still holds the request the Mac queued with it and the admin's own
/// `code-only` never named its key (only the admin's own removal does): the approver judges it
/// once and approves nothing, pass after pass. The mesh churn lab found the loop this ends (`lab
/// mesh churn --n 30 --seed 1793870641921`): the removed Mac let back in by its old Allow, left
/// out by the next version, let back in again, for good.
///
/// NEGATIVE CONTROL: drop the `recipient_is_removed` look from `auto_approve_listed`'s `allowed`:
/// the approver lets m40 back in and the `"nothing for the removed m40"` assert fails
/// (`left: ["m40"]`).
#[test]
fn a_mac_another_member_removed_is_not_let_back_in_by_the_allow_it_queued() {
    let macs = two_macs(&[(Audience::Everyone, "shared", b"everyone-v1".as_slice())]);
    let (m22, m7) = (&macs.m22, &macs.m7);
    let (m40, request, allowed) = newcomer(macs.root.path(), m7, "m40");
    let (bundle, logged) = hand(m7, &request, &allowed, true);
    assert!(logged);
    post_records_to_servers(m7).expect("m7 posts the admission");
    write_from_log_bundle(&m40, &bundle).expect("m40 writes from the log");
    let recipient = request.binding().recipient().to_bech32();
    let queued = || {
        requests_for(m22, &config(m22).unwrap().unwrap(), None)
            .unwrap()
            .into_iter()
            .filter(|asked| asked.recipient == recipient && asked.allowed.is_some())
            .collect::<Vec<_>>()
    };
    ask_again_if_due(&m40, &config(&m40).unwrap().unwrap(), &|_| {});
    let asked = queued();
    assert_eq!(asked.len(), 1, "CONTROL: m40's Allowed is queued");

    // m7 removes m40 in the log, and the admin reads the removal; its own `code-only` does not
    // name m40's key.
    remove_member_in_log(m7, request.binding().actor()).expect("m7 removes m40");
    post_records_to_servers(m7).expect("m7 posts the removal");
    sync_records(m22, None).expect("m22 pulls the removal");
    let admin = admin_open(m22).unwrap();
    assert!(
        !admin.published_doc.code_only().contains(&recipient),
        "CONTROL: the admin's own list never let m40's key go"
    );
    assert!(
        !admin
            .published_doc
            .machines()
            .iter()
            .any(|machine| machine.recipient == recipient),
        "CONTROL: m40 is in no [machines] the admin publishes"
    );
    for pass in 0..2 {
        let done = auto_approve_listed(
            m22,
            &admin_open(m22).unwrap(),
            &asked,
            &mut false,
            &mut BTreeSet::new(),
            (&mut JoinKeys::default(), None),
        )
        .unwrap();
        assert_eq!(
            done.approved,
            Vec::<String>::new(),
            "nothing for the removed m40 (pass {pass})"
        );
    }
    assert!(
        !admin_open(m22)
            .unwrap()
            .doc
            .machines()
            .iter()
            .any(|machine| machine.recipient == recipient),
        "m40 stays out of the vault"
    );
    macs.server.stop();
    drop(macs.root);
}

/// A Mac the log let in keeps its `Allowed` and posts it to its source each hour until an
/// admin's approver names it in `[machines]` (that approver names it in `aterm-swarm-roster` and
/// the envelope 0.145 Macs read; the fleet admits it by the log since F17): the member's one
/// post can miss (a laptop split from the LAN posts to a server no admin reads, and a queue
/// expires after a week), and nothing else ever posted it again once the Mac was in.
///
/// The log's overlay names m40 in the admin's `[machines]` before the approver looks, so the
/// approver publishes because the version served does not address m40 yet, not because it
/// approves it.
///
/// NEGATIVE CONTROLS: in `write_from_log_bundle`, forget the `Allowed` (`forget_allowed`, as
/// before): the `"m40 keeps its Allowed"` assert fails, and no post reaches the approver. In
/// `auto_approve_listed`, publish only for an approval or a roster change (drop `unaddressed`):
/// m40 never gets a locker and the `"named in [machines], m40 posts it no more"` assert fails.
#[test]
fn a_mac_the_log_admits_posts_its_allowed_until_an_admins_approver_names_it() {
    let macs = two_macs(&[(Audience::Everyone, "shared", b"everyone-v1".as_slice())]);
    let (m22, m7) = (&macs.m22, &macs.m7);
    let (m40, request, allowed) = newcomer(macs.root.path(), m7, "m40");
    // `hand` posts no `Allowed`: the member's own post is lost.
    let (bundle, logged) = hand(m7, &request, &allowed, true);
    assert!(logged);
    post_records_to_servers(m7).expect("m7 posts the admission");
    write_from_log_bundle(&m40, &bundle).expect("m40 writes from the log");
    assert!(holds_allowed(&m40), "m40 keeps its Allowed");

    let recipient = request.binding().recipient().to_bech32();
    let queued = || {
        requests_for(m22, &config(m22).unwrap().unwrap(), None)
            .unwrap()
            .into_iter()
            .any(|asked| asked.recipient == recipient && asked.allowed.is_some())
    };
    assert!(!queued(), "CONTROL: no member posted m40's Allowed");
    ask_again_if_due(&m40, &config(&m40).unwrap().unwrap(), &|_| {});
    assert!(queued(), "m40's subscriber posts its Allowed");

    // The admin's approver names m40 in `[machines]`; m40's locker then arrives, and the Allowed
    // has done its work.
    sync_records(m22, None).expect("m22 pulls m7's records");
    approver_pass(m22, &|_| {}, &mut Approver::default());
    assert!(
        admin_open(m22)
            .unwrap()
            .doc
            .machines()
            .iter()
            .any(|machine| machine.recipient == recipient),
        "the approver named m40"
    );
    sync(&m40, None).expect("m40 syncs");
    assert_eq!(load_state(&m40).unwrap().membership, Stored::In);
    assert_eq!(
        std::fs::read(m40.home.join(".secrets/shared")).unwrap(),
        b"everyone-v1"
    );
    assert!(
        !holds_allowed(&m40),
        "named in [machines], m40 posts it no more"
    );
    // The version served addresses m40 now, so a later pass publishes nothing for it.
    let seq = admin_open(m22).unwrap().seq;
    approver_pass(m22, &|_| {}, &mut Approver::default());
    assert_eq!(admin_open(m22).unwrap().seq, seq, "no publish again");

    macs.server.stop();
    drop(macs.root);
}

/// After a split longer than a few steps, the gap a heal shows opened long ago, yet the holders
/// still close it one at a time: each counts its rank's steps from when it first saw the gap,
/// not from when the gap opened, at which every holder would already be past due and all ~35
/// would sign a `Wrap` and post their whole stores.
///
/// NEGATIVE CONTROL: in `wrap_gaps_pass`, count from `gap.opened_ms` again (`let first =
/// gap.opened_ms;`): the rank-1 holder signs at its first look and the `"the rank-1 holder
/// waits a step after it first sees the gap"` assert fails.
#[test]
fn a_gap_seen_long_after_it_opened_is_still_closed_by_one_holder_first() {
    let macs = two_macs(&[(Audience::Everyone, "shared", b"everyone-v1".as_slice())]);
    let (m40, gap, first, second) = admitted_beside_a_concurrent_epoch(&macs);
    let step = log_entries::GAP_STEP_MS;
    let healed = gap.opened_ms + 10 * step;

    assert!(
        log_entries::close_wrap_gaps(&second, healed)
            .unwrap()
            .is_empty(),
        "the rank-1 holder waits a step after it first sees the gap"
    );
    assert!(
        log_entries::close_wrap_gaps(&second, healed + step - 1)
            .unwrap()
            .is_empty(),
        "and the whole step"
    );
    assert_eq!(
        log_entries::close_wrap_gaps(&first, healed).unwrap().len(),
        1,
        "the rank-0 holder closes it at first sight"
    );
    // Had the rank-0 holder been away, the rank-1 holder closes it one step after it saw it.
    assert_eq!(
        log_entries::close_wrap_gaps(&second, healed + step)
            .unwrap()
            .len(),
        1,
        "the rank-1 holder closes a gap still open a step on"
    );
    post_records_to_servers(&first).expect("posted");
    sync(&m40, None).expect("m40 syncs");
    assert_eq!(
        std::fs::read(m40.home.join(".secrets/b-value")).unwrap(),
        b"put-beside-the-admit"
    );

    macs.server.stop();
    drop(macs.root);
}

/// The approver's `aterm-swarm-roster` follows the log's labels (C11 under F15): a Mac a member
/// let in as `m40` gets `m40` in the roster too, though the fleet-accounts sheet names `m40`
/// (whose 24-hour hold the person's Allow settled), and a Mac the log does not label that asks
/// as `m40` (F10's files) gets another label rather than a second `m40`. The log's overlay names
/// the first Mac in `[machines]` before the approver looks, so the approver gives a still-asking
/// member the log labels its row too.
///
/// NEGATIVE CONTROLS: in `maintain_swarm_roster_on_approve`, skip the fix-up after
/// `assign_newcomer`: the log's `m40` keeps the roster's own label (`m42`) and the `"the roster
/// takes the log's label"` assert fails. In `auto_approve_listed`, collect no `unrostered`
/// member: the log's Mac gets no row and `"a row"` fails.
#[test]
fn the_approvers_roster_follows_the_labels_the_log_assigned() {
    let macs = two_macs(&[(Audience::Everyone, "shared", b"everyone-v1".as_slice())]);
    let (m22, m7) = (&macs.m22, &macs.m7);
    admin_seed_swarm_roster(m22, &[]).expect("seed the roster");
    {
        let admin = admin_open(m22).unwrap();
        let mut doc = admin.doc.clone();
        doc.set_entry(
            &Audience::Everyone,
            "fleet-accounts.toml",
            b"[[account]]\nlabel = \"a\"\nmachines = [\"m40\"]\n",
        )
        .unwrap();
        admin_publish(m22, &admin, &doc).expect("publish the sheet");
    }
    sync(m7, None).expect("m7 syncs");

    let (_, request, allowed) = newcomer(macs.root.path(), m7, "m40");
    assert!(hand(m7, &request, &allowed, true).1);
    assert_eq!(
        load_records(m7)
            .unwrap()
            .log()
            .unwrap()
            .membership()
            .label(&request.binding().actor()),
        Some("m40"),
        "CONTROL: the log labels it m40"
    );
    post_records_to_servers(m7).expect("posted");
    post_allowed(m7, &allowed).expect("m7 posts the Allowed");
    let (_, request2, allowed2) = newcomer(macs.root.path(), m7, "m40-again");
    let allowed2 = Bootstrap::required(m7)
        .unwrap()
        .device()
        .with_seed(|seed| crate::members::Allowed::sign(request2.binding(), "m40", seed))
        .unwrap_or(allowed2);
    assert!(
        !hand(m7, &request2, &allowed2, false).1,
        "F10's files for a Mac that does not read the log"
    );
    post_allowed(m7, &allowed2).expect("m7 posts the Allowed");

    sync_records(m22, None).expect("m22 pulls the records");
    approver_pass(m22, &|_| {}, &mut Approver::default());
    let roster = crate::swarm_roster::SwarmRoster::from_document(&admin_open(m22).unwrap().doc)
        .unwrap()
        .expect("a roster");
    let row = |recipient: crate::age::Recipient| {
        roster
            .members
            .iter()
            .find(|row| row.recipient == recipient.to_bech32())
            .cloned()
            .expect("a row")
    };
    let logged = row(request.binding().recipient());
    assert_eq!(logged.label, "m40", "the roster takes the log's label");
    assert_eq!(logged.claimed_label, None);
    assert_ne!(
        row(request2.binding().recipient()).label,
        "m40",
        "no second m40"
    );

    macs.server.stop();
    drop(macs.root);
}

/// The no-log fallback stays the working path where the log path does not apply (F15 keeps
/// F10's interim there): a vault with no genesis yet (ALab until F12b), and a new Mac that does
/// not read the log bundle. Each hands F10's `[everyone]` files, the new Mac writes them with
/// no records file and holds the member's `Allowed` for the admin's approver.
///
/// NEGATIVE CONTROL: in `hand_over`, ignore `reads_log` (take the log path whenever the member
/// can sign one): the Mac that does not read the log gets a log bundle and the
/// `"a Mac that does not read the log gets files"` assert fails.
#[test]
fn a_vault_with_no_log_still_admits_by_the_allowed_relay() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let server = start_loopback_server(&root.path().join("s1"));
    let url = format!("http://{}/v1/alab", server.addr);
    let m22 = Paths::for_home(&root.path().join("m22"));
    let created = test_create_unbound(&m22, &url, None, Some("m22")).unwrap();
    admin_keys::legacy::publish_legacy(&m22, &admin_open(&m22).unwrap());
    admin_commons_init(&m22).expect("commons init");
    let pinned = format!("{url}#{}", created.signer);
    let m7 = Paths::for_home(&root.path().join("m7"));
    let m7_rec = machine_identity(&m7).unwrap().recipient().to_bech32();
    {
        let admin = admin_open(&m22).unwrap();
        let mut doc = admin.doc.clone();
        doc.add_machine("m7", &m7_rec).unwrap();
        doc.set_entry(&Audience::Everyone, "shared", b"everyone-v1")
            .unwrap();
        admin_publish_approved(&m22, &admin, &doc, std::slice::from_ref(&m7_rec)).unwrap();
    }
    join_host(&m7, &pinned, "m7");
    enroll_commons(&m22, &m7);
    assert!(load_records(&m7).unwrap().log().is_none(), "no genesis yet");

    let (m40, request, allowed) = newcomer(root.path(), &m7, "m40");
    let (bundle, logged) = hand(&m7, &request, &allowed, true);
    assert!(!logged, "a vault with no log hands F10's files");
    write_from_bundle(&m40, &bundle).expect("m40 writes the files");
    assert_eq!(
        std::fs::read(m40.home.join(".secrets/shared")).unwrap(),
        b"everyone-v1"
    );
    assert_eq!(load_state(&m40).unwrap().membership, Stored::Pending);
    assert!(holds_allowed(&m40), "m40 waits on the admin's approver");
    assert!(
        !m40.records_file().exists(),
        "no records travel without a log"
    );
    assert!(write_from_log_bundle(&m40, &bundle).is_err());

    // Once the vault has a log, a new Mac that does not read the log bundle still gets files.
    genesis::sign_genesis(
        &m22,
        &root.path().join("alab.genesis"),
        &decode_key32(&created.signer).unwrap(),
        true,
    )
    .expect("sign genesis");
    sync(&m7, None).expect("m7 syncs the seeded log");
    let (_, request2, allowed2) = newcomer(root.path(), &m7, "m41");
    let (old_joiner, logged) = hand(&m7, &request2, &allowed2, false);
    assert!(
        !logged && old_joiner.files().is_some(),
        "a Mac that does not read the log gets files"
    );
    let (_, logged) = hand(&m7, &request2, &allowed2, true);
    assert!(
        logged,
        "the same member hands the log to a Mac that reads it"
    );

    server.stop();
    drop(root);
}

/// m7 lets m40 in while m22, not yet holding that `Admit`, mints `[everyone]` epoch 2 for m22
/// and m7 only and puts `b-value` under it; then every Mac holds every record. Returns m40, the
/// one wrap gap (epoch 2 to m40), and that gap's two holders, the one
/// [`crate::membership_log::Log::gap_rank`] ranks first first.
fn admitted_beside_a_concurrent_epoch(
    macs: &TwoMacs,
) -> (Paths, crate::membership_log::Gap, Paths, Paths) {
    let (m22, m7) = (&macs.m22, &macs.m7);

    // m22's view before m7's admission reaches it.
    let mut before = load_records(m22).unwrap().log().unwrap().clone();
    let (m40, request, allowed) = newcomer(macs.root.path(), m7, "m40");
    let (bundle, logged) = hand(m7, &request, &allowed, true);
    assert!(logged);
    write_from_log_bundle(&m40, &bundle).expect("m40 writes from the log");
    post_records_to_servers(m7).expect("m7 posts the admission");

    // m22 mints epoch 2 for m22 and m7 only, and puts `b-value` under it.
    let (b22, a22, i22) = member_key(m22);
    let m7_recipient = read_identity(m7).unwrap().unwrap().recipient();
    let m22_recipient = read_identity(m22).unwrap().unwrap().recipient();
    let minted: Vec<Record> = b22.device().with_seed(|seed| {
        let (draft, key) = before
            .draft_epoch(&a22, 5_000, Audience::Everyone, 2)
            .unwrap();
        let epoch = draft.sign(seed, i22).unwrap();
        let epoch_id = *epoch.id();
        before.insert(epoch.clone()).unwrap();
        let mut out = vec![epoch];
        for recipient in [m22_recipient, m7_recipient] {
            let wrap = before
                .draft_wrap(&a22, 5_001, epoch_id, &key, recipient)
                .unwrap()
                .sign(seed, i22)
                .unwrap();
            before.insert(wrap.clone()).unwrap();
            out.push(wrap);
        }
        let entry = Entry {
            name: "b-value".to_owned(),
            value: SecretBytes::from(b"put-beside-the-admit".as_slice()),
            audience: Audience::Everyone,
            if_absent: false,
            anchor: None,
        };
        let put = before
            .draft_put(&a22, 5_002, epoch_id, &key, &entry)
            .unwrap()
            .sign(seed, i22)
            .unwrap();
        before.insert(put.clone()).unwrap();
        out.push(put);
        out
    });
    transport::post_records(&macs.url, &encode_records(&minted)).unwrap();
    for mac in [m22, m7, &m40] {
        sync_records(mac, None).expect("records");
    }
    let log = load_records(m7).unwrap().log().unwrap().clone();
    assert_eq!(
        log.heads(),
        load_records(m22).unwrap().log().unwrap().heads()
    );
    let opened = log
        .open_audience_entries(&read_identity(m7).unwrap().unwrap())
        .unwrap()
        .unwrap();
    let gaps = log.wrap_gaps(&opened, &[crate::membership_log::SeatAudience::Everyone]);
    assert_eq!(gaps.len(), 1, "{gaps:?}");
    let gap = gaps[0].clone();
    let (_, a7, _) = member_key(m7);
    let (first, second) = match (
        log.gap_rank(&opened, &gap, &a22),
        log.gap_rank(&opened, &gap, &a7),
    ) {
        (Some(0), Some(1)) => (m22.clone(), m7.clone()),
        (Some(1), Some(0)) => (m7.clone(), m22.clone()),
        ranks => panic!("two holders rank 0 and 1: {ranks:?}"),
    };
    (m40, gap, first, second)
}

/// F15's wrap-gap rule on real Macs: m7 lets m40 in while m22, not yet holding that `Admit`,
/// mints a new `[everyone]` epoch for the members it knows and puts a value under it. Once both
/// hold every record, exactly one of the epoch's holders signs the missing `Wrap` at the gap's
/// opening (the one [`crate::membership_log::Log::gap_rank`] ranks first); the other waits a
/// step and then finds nothing left to close. m40 then reads the value.
///
/// NEGATIVE CONTROL: in `wrap_gaps_pass`, skip every gap (`continue` before the rank check):
/// no `Wrap` is signed, the `"the rank-0 holder closes the gap"` assert fails, and m40 never
/// reads `b-value`.
#[test]
fn a_wrap_gap_after_a_concurrent_epoch_is_closed_by_one_holder() {
    let macs = two_macs(&[(Audience::Everyone, "shared", b"everyone-v1".as_slice())]);
    let (m40, gap, first, second) = admitted_beside_a_concurrent_epoch(&macs);
    let (first, second) = (&first, &second);

    {
        let _writer = StateLock::take(first).unwrap();
        assert!(log_entries::close_wrap_gaps(first, gap.opened_ms).is_err());
    }
    struct FailedReads(PathBuf);
    impl Drop for FailedReads {
        fn drop(&mut self) {
            fsx::fail_reads(&self.0, 0);
        }
    }
    for path in [first.cache_file(), first.dir.join("commons/device.age")] {
        let fault = FailedReads(path);
        fsx::fail_reads(&fault.0, usize::MAX);
        let unreadable = log_entries::close_wrap_gaps(first, gap.opened_ms);
        drop(fault);
        assert!(unreadable.is_err());
    }

    assert!(
        log_entries::close_wrap_gaps(second, gap.opened_ms)
            .unwrap()
            .is_empty(),
        "the rank-1 holder waits a step"
    );
    let closed = log_entries::close_wrap_gaps(first, gap.opened_ms).unwrap();
    assert_eq!(closed.len(), 1, "the rank-0 holder closes the gap");
    post_records_to_servers(first).expect("posted");
    sync_records(second, None).expect("records");
    assert!(
        log_entries::close_wrap_gaps(second, gap.opened_ms + log_entries::GAP_STEP_MS)
            .unwrap()
            .is_empty(),
        "a gap one holder closed is closed for the other"
    );

    sync(&m40, None).expect("m40 syncs");
    assert_eq!(
        std::fs::read(m40.home.join(".secrets/b-value")).unwrap(),
        b"put-beside-the-admit"
    );

    macs.server.stop();
    drop(macs.root);
}

/// Unproved founders (Genesis v2 founders with no seat yet) respect the Commons audience: `m7` and
/// `m9`, in Commons control's host list, open Commons from the log and may enroll; vault-only
/// `v1` does neither. Under one-mesh M1 and M4 an unproved founder is no member, so the next
/// rotation's carry (the current seats) drops it: `m9` keeps Commons across `m7`'s retirement only
/// because it first takes a seat through log enrollment (the design's step 0: each live seatless
/// recipient asks the desk for a seat). Retiring the still-unproved `m7` rotates both audiences
/// away from it and bars it from enrollment; revoking the seated `m9` does the same; a concurrent
/// non-retiring Commons epoch merged in does not undo `m7`'s retirement; a post-genesis vault
/// machine `p1` is barred the moment it is retired.
///
/// NEGATIVE CONTROL: skip `m9`'s `enroll_commons_member_in_log` (leave it an unproved founder):
/// `m7`'s retirement carries only the current seats, drops the seatless `m9`, and the
/// `log_after_retire.wraps_maximal_audience_to(&EpochAudience::Commons, &m9_rec)` assert fails.
#[test]
fn unproved_founders_respect_commons_audience_and_revocation_rotates_and_bars_enrollment() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let vault_file = root.path().join("alab.vault");
    let m22 = Paths::for_home(&root.path().join("m22"));
    let created =
        test_create_unbound(&m22, vault_file.to_str().unwrap(), None, Some("m22")).unwrap();
    admin_keys::legacy::publish_legacy(&m22, &admin_open(&m22).unwrap());
    admin_commons_init(&m22).expect("commons init");
    let pinned = format!("{}#{}", vault_file.display(), created.signer);

    let m7 = Paths::for_home(&root.path().join("m7"));
    let m9 = Paths::for_home(&root.path().join("m9"));
    let v1 = Paths::for_home(&root.path().join("v1"));
    let m7_rec = machine_identity(&m7).unwrap().recipient();
    let m9_rec = machine_identity(&m9).unwrap().recipient();
    let v1_rec = machine_identity(&v1).unwrap().recipient();
    let m7_str = m7_rec.to_bech32();
    let m9_str = m9_rec.to_bech32();
    let v1_str = v1_rec.to_bech32();

    // Add `m7`, `m9`, and `v1` to `[machines]`, then restrict Commons control to `m22`, `m7`, and `m9`
    // (`v1` is a vault-only machine).
    {
        let admin = admin_open(&m22).unwrap();
        let mut doc = admin.doc.clone();
        doc.add_machine("m7", &m7_str).unwrap();
        doc.add_machine("m9", &m9_str).unwrap();
        doc.add_machine("v1", &v1_str).unwrap();
        doc.set_entry(&Audience::Everyone, "shared", b"v1").unwrap();
        admin_publish_approved(
            &m22,
            &admin,
            &doc,
            &[m7_str.clone(), m9_str.clone(), v1_str.clone()],
        )
        .unwrap();
    }
    {
        let admin = admin_open(&m22).unwrap();
        let mut doc = admin.doc.clone();
        let ctrl = doc
            .entries()
            .into_iter()
            .find(|e| e.name == crate::commons::ENTRY)
            .expect("commons control entry");
        doc.unset_entry(&ctrl.audience, crate::commons::ENTRY)
            .unwrap();
        doc.set_entry(
            &Audience::Hosts(vec!["m22".to_owned(), "m7".to_owned(), "m9".to_owned()]),
            crate::commons::ENTRY,
            ctrl.value.as_slice(),
        )
        .unwrap();
        admin_publish(&m22, &admin, &doc).unwrap();
    }

    join_host(&m7, &pinned, "m7");
    join_host(&m9, &pinned, "m9");
    join_host(&v1, &pinned, "v1");

    // Seed Genesis v2 before `m7`, `m9`, or `v1` enroll in Commons so all three are unproved founders.
    let gen_out = root.path().join("genesis.bin");
    seed_vault_genesis(&m22, &gen_out).expect("seed genesis v2");

    let trust = Bootstrap::required(&m22).unwrap().trust();
    let req7 = Device::open(&m7)
        .unwrap()
        .unwrap()
        .request(trust.network, trust.authority)
        .unwrap();
    let req9 = Device::open(&m9)
        .unwrap()
        .unwrap()
        .request(trust.network, trust.authority)
        .unwrap();
    let req_v1 = Device::open(&v1)
        .unwrap()
        .unwrap()
        .request(trust.network, trust.authority)
        .unwrap();

    let log_initial = load_records(&m22).unwrap().log().unwrap().clone();
    assert_eq!(log_initial.membership().unproved().len(), 3);
    // Positive control: `m7` and `m9` (in Commons audience) are wrapped in `EpochAudience::Commons`
    // and pass `check_log_commons_enrollment`.
    assert!(log_initial.wraps_maximal_audience_to(&EpochAudience::Commons, &m7_rec));
    assert!(log_initial.wraps_maximal_audience_to(&EpochAudience::Commons, &m9_rec));
    let id7 = read_identity(&m7).unwrap().unwrap();
    let id9 = read_identity(&m9).unwrap().unwrap();
    let id_v1 = read_identity(&v1).unwrap().unwrap();
    assert!(
        log_initial.open_commons_control(&id7).unwrap().is_some(),
        "unproved founder m7 wrapped in Commons must open commons control from log"
    );
    assert!(
        log_initial.open_commons_control(&id9).unwrap().is_some(),
        "unproved founder m9 wrapped in Commons must open commons control from log"
    );
    assert!(
        log_initial.open_commons_control(&id_v1).unwrap().is_none(),
        "vault-only founder v1 must not open commons control from log"
    );
    check_log_commons_enrollment(&m22, trust, req7.binding()).expect("m7 eligible");
    check_log_commons_enrollment(&m22, trust, req9.binding()).expect("m9 eligible");

    // Negative control: vault-only `v1` is not wrapped in `EpochAudience::Commons`, is not promoted
    // into `aterm.commons.control` by `overlay_log_onto_document`, and is refused Commons enrollment.
    assert!(!log_initial.wraps_maximal_audience_to(&EpochAudience::Commons, &v1_rec));
    let id22 = read_identity(&m22).unwrap().unwrap();
    let mut doc_overlay = admin_open(&m22).unwrap().doc.clone();
    let _ = log_entries::overlay_log_onto_document(&log_initial, &id22, &mut doc_overlay, None)
        .unwrap();
    let ctrl_aud = doc_overlay
        .entries()
        .into_iter()
        .find(|e| e.name == crate::commons::ENTRY)
        .unwrap()
        .audience;
    assert_eq!(
        ctrl_aud,
        Audience::Hosts(vec!["m22".to_owned(), "m7".to_owned(), "m9".to_owned()]),
        "vault-only founder v1 must not be promoted into Commons control audience"
    );
    assert!(
        check_log_commons_enrollment(&m22, trust, req_v1.binding()).is_err(),
        "vault-only founder v1 must be refused Commons enrollment"
    );

    // One-mesh M1: an unproved founder holds no seat, so it is no current recipient and the next
    // rotation drops it. `m9` takes a seat through log enrollment (an `Evidence::Listed` `Admit`
    // on its possession proof); `m7` stays unproved.
    assert!(
        !log_initial
            .current_recipients(&EpochAudience::Commons)
            .contains(&m9_rec),
        "CONTROL: unproved founder m9 holds no seat before it enrolls"
    );
    let (mut pending9, challenge9) = Pending::issue(&req9, trust.network, trust.authority).unwrap();
    let answer9 = Device::open(&m9)
        .unwrap()
        .unwrap()
        .answer(&req9, &challenge9)
        .unwrap();
    let possession9 = pending9.verify(&answer9).unwrap();
    enroll_commons_member_in_log(&m22, trust, req9.clone(), possession9)
        .expect("unproved founder m9 takes a seat");
    let log_seated = load_records(&m22).unwrap().log().unwrap().clone();
    let seats = log_seated.current_recipients(&EpochAudience::Commons);
    assert!(seats.contains(&m9_rec), "enrolled m9 holds a current seat");
    assert!(
        !seats.contains(&m7_rec),
        "m7, never enrolled, is still seatless"
    );

    // Retiring unproved founder `m7` via `admin_retire` rotates `EpochAudience::Commons` and
    // `EpochAudience::Everyone` away from `m7` while the carry, the current seats, keeps the
    // seated `m9`'s wraps.
    let admin = admin_open(&m22).unwrap();
    let retired_m7 = admin_retire(&m22, &admin, &["m7".to_owned()]).expect("retire unproved m7");
    assert!(retired_m7.changed);
    let retired_m7_retry =
        admin_retire(&m22, &admin, &["m7".to_owned()]).expect("idempotent retry");
    assert!(!retired_m7_retry.changed);

    let log_after_retire = load_records(&m22).unwrap().log().unwrap().clone();
    assert!(!log_after_retire.wraps_maximal_audience_to(&EpochAudience::Commons, &m7_rec));
    assert!(!log_after_retire.wraps_maximal_audience_to(&EpochAudience::Everyone, &m7_rec));
    assert!(log_after_retire.wraps_maximal_audience_to(&EpochAudience::Commons, &m9_rec));
    assert!(log_after_retire.wraps_maximal_audience_to(&EpochAudience::Everyone, &m9_rec));
    assert!(
        log_after_retire
            .open_commons_control(&id7)
            .unwrap()
            .is_none()
    );
    assert!(
        log_after_retire
            .open_commons_control(&id9)
            .unwrap()
            .is_some()
    );
    assert!(
        check_log_commons_enrollment(&m22, trust, req7.binding()).is_err(),
        "retired unproved founder m7 must be barred from Commons enrollment"
    );
    check_log_commons_enrollment(&m22, trust, req9.binding()).expect("m9 still eligible");

    // Revoking listed founder `m9` (seated above) via `admin_revoke` rotates
    // `EpochAudience::Commons` away from `m9` and bars `m9` from Commons enrollment, while keeping
    // previously retired unproved founder `m7` out of `commons_eligibility`,
    // `EpochAudience::Commons`, and `EpochAudience::Everyone`.
    let admin = admin_open(&m22).unwrap();
    assert!(
        !admin.commons_eligibility.includes("m7", m7_rec),
        "overlay_log_onto_document must not restore retired unproved founder m7 into commons_eligibility"
    );
    let revoked_m9 = admin_revoke(&m22, &admin, "m9", false).expect("revoke unproved m9");
    assert!(revoked_m9.retirement.is_some());
    let log_after_revoke = load_records(&m22).unwrap().log().unwrap().clone();
    assert!(!log_after_revoke.wraps_maximal_audience_to(&EpochAudience::Commons, &m7_rec));
    assert!(!log_after_revoke.wraps_maximal_audience_to(&EpochAudience::Everyone, &m7_rec));
    assert!(!log_after_revoke.wraps_maximal_audience_to(&EpochAudience::Commons, &m9_rec));
    assert!(!log_after_revoke.wraps_maximal_audience_to(&EpochAudience::Everyone, &m9_rec));
    assert!(
        log_after_revoke
            .open_commons_control(&id9)
            .unwrap()
            .is_none()
    );
    assert!(
        check_log_commons_enrollment(&m22, trust, req9.binding()).is_err(),
        "revoked unproved founder m9 must be barred from Commons enrollment"
    );

    // Concurrent retirement vs non-retiring epoch rotation for unproved founder `m7`:
    // `log_after_retire` retired `m7`, whereas a concurrent branch on `log_initial` rotated
    // `Commons` still wrapping `m7`. Merging both must NOT undo `m7`'s retirement.
    let (b22, actor22, inc22) = member_key(&m22);
    let mut branch_keep_m7 = log_initial;
    let ctrl_bytes = branch_keep_m7.open_commons_control(&id22).unwrap().unwrap();
    b22.device()
        .with_seed(|seed22| -> crate::Result<()> {
            let (ep_draft, ep_key) =
                branch_keep_m7.draft_epoch(&actor22, 999, EpochAudience::Commons, 2)?;
            let ep_rec = ep_draft.sign(seed22, inc22)?;
            let ep_id = *ep_rec.id();
            branch_keep_m7.insert(ep_rec)?;
            for rec in [id22.recipient(), m7_rec, m9_rec] {
                let wrap = branch_keep_m7
                    .draft_wrap(&actor22, 999, ep_id, &ep_key, rec)?
                    .sign(seed22, inc22)?;
                branch_keep_m7.insert(wrap)?;
            }
            let put = branch_keep_m7
                .draft_commons_put(&actor22, 999, ep_id, &ep_key, ctrl_bytes.as_slice())?
                .sign(seed22, inc22)?;
            branch_keep_m7.insert(put)?;
            Ok(())
        })
        .unwrap();
    let mut merged_retire = log_after_retire;
    merged_retire.merge(&branch_keep_m7).unwrap();
    assert!(
        !merged_retire.wraps_maximal_audience_to(&EpochAudience::Commons, &m7_rec),
        "concurrent non-retiring epoch must not undo unproved founder m7's retirement"
    );
    assert!(merged_retire.open_commons_control(&id7).unwrap().is_none());
    assert!(merged_retire.wraps_maximal_audience_to(&EpochAudience::Commons, &m9_rec));

    // Post-Genesis approved machine `p1` (added via `admin_publish_approved` after Genesis v2, so
    // `p1` is neither in `membership.members()` nor in `membership.unproved()`): retiring `p1` via
    // `admin_retire` rotates `Commons` and `Everyone` away from `p1` and immediately bars `p1` from
    // `check_log_commons_enrollment` even before any new vault envelope is published.
    let p1 = Paths::for_home(&root.path().join("p1"));
    let p1_rec = machine_identity(&p1).unwrap().recipient();
    let p1_str = p1_rec.to_bech32();
    {
        let admin = admin_open(&m22).unwrap();
        let mut doc = admin.doc.clone();
        doc.add_machine("p1", &p1_str).unwrap();
        admin_publish_approved(&m22, &admin, &doc, std::slice::from_ref(&p1_str)).unwrap();
    }
    join_host(&p1, &pinned, "p1");
    let id_p1 = read_identity(&p1).unwrap().unwrap();
    let req_p1 = Bootstrap::required(&p1)
        .expect("post-genesis p1 observes Commons control from log before enrolling")
        .request()
        .unwrap();
    check_log_commons_enrollment(&m22, trust, req_p1.binding()).expect("post-genesis p1 eligible");
    let admin = admin_open(&m22).unwrap();
    let retired_p1 =
        admin_retire(&m22, &admin, &["p1".to_owned()]).expect("retire post-genesis p1");
    assert!(retired_p1.changed);
    let log_after_p1 = load_records(&m22).unwrap().log().unwrap().clone();
    assert!(!log_after_p1.wraps_maximal_audience_to(&EpochAudience::Commons, &p1_rec));
    assert!(!log_after_p1.wraps_maximal_audience_to(&EpochAudience::Everyone, &p1_rec));
    assert!(log_after_p1.open_commons_control(&id_p1).unwrap().is_none());
    assert!(
        check_log_commons_enrollment(&m22, trust, req_p1.binding()).is_err(),
        "retired post-genesis machine p1 must be barred from Commons enrollment"
    );
}

/// Concurrent removals on two islands leave `[everyone]` and Commons two newest epochs each, each
/// still wrapping the other island's removed Mac; after the merge the vault passes on `m22` (a
/// remover, so it acts within a step) coalesce both into one epoch each that wraps neither, and the next pass,
/// after a newcomer's admission, finds nothing due.
#[test]
fn the_tender_coalesces_concurrent_remove_epochs_and_skips_superseded_everyone_epochs() {
    use crate::commons::bootstrap::Device;
    use crate::membership_log::{Entrant, EpochAudience, Evidence, MemberId};

    let macs = two_macs(&[(Audience::Everyone, "shared", b"v1".as_slice())]);
    let (m22, m7) = (&macs.m22, &macs.m7);
    let (b22, _, _) = member_key(m22);
    let trust = b22.trust();

    let admit_new = |name: &str, byte: u8| -> (Paths, MemberId, Recipient) {
        let p = Paths::for_home(&macs.root.path().join(name));
        configure_log_member(&p, &format!("{}#{}", macs.url, macs.signer), name).unwrap();
        let rec = machine_identity(&p).unwrap().recipient();
        let req = Device::open(&p)
            .unwrap()
            .unwrap()
            .request(trust.network, trust.authority)
            .unwrap();
        let actor = req.binding().actor();
        admit_member_in_log(
            m22,
            Entrant {
                request: req,
                node_key: [byte; 32],
                name: name.to_owned(),
                label: name.to_owned(),
            },
            Evidence::Phrase,
        )
        .unwrap();
        (p, actor, rec)
    };

    let (_m8, actor8, rec8) = admit_new("m8", 0x88);
    let (_m9, actor9, rec9) = admit_new("m9", 0x99);
    let base_wire = load_records(m22).unwrap().encode();
    save_records(m7, &base_wire).unwrap();

    // Concurrent removals on two islands: `m22` removes `m8` while `m7` removes `m9`.
    remove_member_in_log(m22, actor8).unwrap();
    remove_member_in_log(m7, actor9).unwrap();

    // Merge both islands onto `m22`. Before the tender, both `EpochAudience::Commons` and
    // `EpochAudience::Everyone` have 2 concurrent maximal epochs, each still wrapping the other
    // island's removed member.
    let wire7 = load_records(m7).unwrap().encode();
    save_records(m22, &wire7).unwrap();
    let id22 = read_identity(m22).unwrap().unwrap();
    let pre_log = load_records(m22).unwrap().log().unwrap().clone();
    let active_recs: BTreeSet<Recipient> = pre_log
        .membership()
        .members()
        .values()
        .map(|m| m.binding.recipient())
        .collect();
    assert!(!active_recs.contains(&rec8));
    assert!(!active_recs.contains(&rec9));
    let due: BTreeSet<EpochAudience> = pre_log
        .rekey_dues()
        .into_iter()
        .filter(|due| due.rotation && due.coverage)
        .map(|due| due.audience)
        .collect();
    assert_eq!(
        due,
        BTreeSet::from([EpochAudience::Everyone, EpochAudience::Commons]),
        "CONTROL: both audiences are due"
    );

    let minted = vault_passes(m22);
    assert!(!minted.is_empty());

    let post_log = load_records(m22).unwrap().log().unwrap().clone();
    assert!(
        post_log.rekey_dues().is_empty(),
        "the tender converges in one pass without a false-positive loop"
    );
    let opened = post_log.open_audience_entries(&id22).unwrap().unwrap();
    for aud in [EpochAudience::Commons, EpochAudience::Everyone] {
        let max_ids = opened.maximal_epochs.get(&aud).unwrap();
        assert_eq!(
            max_ids.len(),
            1,
            "coalesce must leave a single maximal epoch for {aud:?}"
        );
        let wraps = opened.wrapped_recipients.get(&max_ids[0]).unwrap();
        assert!(!wraps.contains(&rec8), "{aud:?} must exclude removed m8");
        assert!(!wraps.contains(&rec9), "{aud:?} must exclude removed m9");
    }

    // Admitting a newcomer `m10` after epoch rotations wraps only the winning and newest epochs
    // to `m10`; no re-key is due and no wrap gap opens, so the vault passes sign nothing rather
    // than looping on superseded epochs.
    let (_m10, _, rec10) = admit_new("m10", 0xaa);
    let after_admit_log = load_records(m22).unwrap().log().unwrap().clone();
    assert!(
        after_admit_log
            .membership()
            .members()
            .values()
            .any(|m| m.binding.recipient() == rec10)
    );
    assert!(
        after_admit_log.rekey_dues().is_empty(),
        "superseded epochs must not make a re-key due after a newcomer's admission"
    );
    assert!(vault_passes(m22).is_empty());

    // A log-only newcomer (`m10`, configured with `configure_log_member` and holding no 0.145
    // envelope) reads reserved entries (`aterm-swarm-roster`, `aterm-fleet-code`), overlays
    // log-admitted members into `swarm_roster`, and resolves its own canonical `m<N>` label.
    let wire22 = load_records(m22).unwrap().encode();
    save_records(&_m10, &wire22).unwrap();
    let roster10 = swarm_roster_until(
        &_m10,
        std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN,
    )
    .unwrap()
    .expect("log-only member swarm_roster");
    let labels10: BTreeSet<String> = roster10.members.iter().map(|m| m.label.clone()).collect();
    assert!(labels10.contains("m22"));
    assert!(labels10.contains("m7"));
    assert!(labels10.contains("m10"));
    assert!(
        !labels10.contains("m8"),
        "removed m8 must not appear in swarm_roster"
    );
    assert!(
        !labels10.contains("m9"),
        "removed m9 must not appear in swarm_roster"
    );
    assert_eq!(
        super::roster::resolve_local_swarm_label(&_m10, &[])
            .unwrap()
            .as_deref(),
        Some("m10")
    );

    macs.server.stop();
}

/// A Mac a member let in by F10's files (Pending, holding the member's `Allowed`) waits on an
/// admin's approver and asks the members nobody while it holds no membership log, as F10 has it;
/// once it holds a trusted one, any member can let it in by the log (F15), so its subscriber asks
/// the members again instead of waiting on an approver that may never run.
///
/// NEGATIVE CONTROL: in `ask_members_if_due`, gate on `!holds_allowed(paths)` alone (as before):
/// the Mac holding the log asks nobody and the `"with a trusted log it asks the members"` assert
/// fails.
#[test]
fn a_mac_holding_f10s_files_asks_the_members_again_once_it_holds_a_log() {
    use crate::envelope::{Retired, TEST_RETIRED};
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            TEST_RETIRED.with(|cell| *cell.borrow_mut() = None);
        }
    }
    let macs = two_macs(&[(Audience::Everyone, "shared", b"everyone-v1".as_slice())]);
    let m7 = &macs.m7;
    let (m40, request, allowed) = newcomer(macs.root.path(), m7, "m40");
    let (bundle, logged) = hand(m7, &request, &allowed, false);
    assert!(!logged, "CONTROL: F10's files");
    write_from_bundle(&m40, &bundle).expect("m40 writes the files");
    assert_eq!(load_state(&m40).unwrap().membership, Stored::Pending);
    assert!(holds_allowed(&m40), "m40 holds the member's Allowed");

    let asks = std::sync::atomic::AtomicUsize::new(0);
    let ask = |_: &Paths| Ok(asks.fetch_add(1, Ordering::SeqCst) + 1);
    let count = || asks.load(Ordering::SeqCst);
    let vault = decode_key32(&macs.signer).expect("the vault key");
    TEST_RETIRED.with(|cell| {
        *cell.borrow_mut() = Some(Retired {
            keys: Vec::new(),
            current: Some(vault),
        });
    });
    let _reset = Reset;
    let mut asked = MembersAsked::default();
    ask_members_if_due(&m40, &mut asked, &ask, &|_| {});
    assert_eq!(
        count(),
        0,
        "with no log, F10's relay: the Allowed waits on an admin's approver"
    );

    sync_records(&m40, None).expect("m40 pulls the records");
    assert!(
        log_entries::trusted_configured_log(&m40, &config(&m40).unwrap().unwrap())
            .unwrap()
            .is_some(),
        "CONTROL: m40 holds the vault's log"
    );
    asked = MembersAsked::default();
    ask_members_if_due(&m40, &mut asked, &ask, &|_| {});
    assert_eq!(count(), 1, "with a trusted log it asks the members");

    macs.server.stop();
    drop(macs.root);
}

/// A member that lacks an epoch only as its own wrap gap (let in on one side of a split while
/// the other side minted it, before a holder wrapped it the epoch) still lets the next Mac in by
/// the log: the new Mac takes the epochs the member holds and an `Admit`, never F10's files from
/// a partial view and an `Allowed` that waits on an admin, and the epoch's holders close the gap
/// for both Macs.
///
/// NEGATIVE CONTROL: in `admit_by_phrase`'s `holds`, accept only epochs this Mac unwrapped (drop
/// the `is_wrap_gap` arm): the log path does not apply, and F10's fallback has no applied
/// version to read on a Mac the log alone let in, so the run ends with "this Mac holds no applied
/// vault to hand a new Mac" (where m40 also held a locker it would hand F10's files from a partial
/// view instead).
#[test]
fn a_member_missing_an_epoch_only_as_its_own_wrap_gap_still_admits_by_the_log() {
    let macs = two_macs(&[(Audience::Everyone, "shared", b"everyone-v1".as_slice())]);
    let (m40, _, first, second) = admitted_beside_a_concurrent_epoch(&macs);
    assert!(
        !m40.home.join(".secrets/b-value").exists(),
        "CONTROL: m40 lacks the epoch the LAN side minted"
    );

    let (m41, request, allowed) = newcomer(macs.root.path(), &m40, "m41");
    let (bundle, logged) = hand(&m40, &request, &allowed, true);
    assert!(logged, "m40 lets m41 in by the log");
    write_from_log_bundle(&m41, &bundle).expect("m41 writes from the log");
    assert_eq!(load_state(&m41).unwrap().membership, Stored::In);
    assert_eq!(
        std::fs::read(m41.home.join(".secrets/shared")).unwrap(),
        b"everyone-v1",
        "m41 reads what m40 holds at once"
    );
    post_records_to_servers(&m40).expect("m40 posts the admission");

    // Both holders see two gaps (m40's and m41's) and, between them, close both within a step.
    let now = aterm_time::unix_millis().expect("the clock");
    for at in [now, now + log_entries::GAP_STEP_MS] {
        for holder in [&first, &second] {
            sync_records(holder, None).expect("records");
            log_entries::close_wrap_gaps(holder, at).expect("close");
            post_records_to_servers(holder).expect("posted");
        }
    }
    for mac in [&m40, &m41] {
        sync(mac, None).expect("syncs");
        assert_eq!(
            std::fs::read(mac.home.join(".secrets/b-value")).unwrap(),
            b"put-beside-the-admit"
        );
    }

    macs.server.stop();
    drop(macs.root);
}

/// A value removed from `[everyone]` stays closed to a Mac admitted after: removing it mints a
/// fresh `[everyone]` epoch and reseals what stays, so the epoch that sealed the removed value is
/// neither newest nor winning, and the member that admits the next Mac wraps it no key that
/// opens the old value (the bundle carries every record, the old `Put` included).
///
/// NEGATIVE CONTROL: in `draft_document_diff`, drop `left_everyone` from the reuse test: the
/// removal writes a `Delete` under the old epoch, m7 wraps that epoch to m40, and the `"no key
/// that sealed the removed value"` assert fails.
#[test]
fn a_value_removed_from_everyone_stays_closed_to_a_mac_admitted_after() {
    let macs = two_macs(&[
        (Audience::Everyone, "shared", b"everyone-v1".as_slice()),
        (
            Audience::Everyone,
            "secret",
            b"removed-before-m40".as_slice(),
        ),
    ]);
    let (m22, m7) = (&macs.m22, &macs.m7);
    let id7 = read_identity(m7).unwrap().unwrap();
    let sealed_secret: BTreeSet<crate::membership_log::RecordId> = {
        let log = load_records(m7).unwrap().log().unwrap().clone();
        let opened = log.open_audience_entries(&id7).unwrap().unwrap();
        assert!(
            opened
                .by_audience
                .get(&Audience::Everyone)
                .is_some_and(|entries| entries.contains_key("secret")),
            "CONTROL: the log holds the value"
        );
        // The `[everyone]` epochs sealing the values then (`secret` among them).
        opened.winning_epochs[&crate::membership_log::EpochAudience::Everyone].clone()
    };
    {
        let admin = admin_open(m22).unwrap();
        let mut doc = admin.doc.clone();
        assert!(doc.unset_entry(&Audience::Everyone, "secret").unwrap());
        admin_publish(m22, &admin, &doc).expect("publish the removal");
    }
    sync(m7, None).expect("m7 syncs");
    assert!(!m7.home.join(".secrets/secret").exists());

    let (m40, request, allowed) = newcomer(macs.root.path(), m7, "m40");
    let (bundle, logged) = hand(m7, &request, &allowed, true);
    assert!(logged);
    write_from_log_bundle(&m40, &bundle).expect("m40 writes from the log");
    assert_eq!(
        std::fs::read(m40.home.join(".secrets/shared")).unwrap(),
        b"everyone-v1"
    );
    assert!(!m40.home.join(".secrets/secret").exists());
    let log = load_records(&m40).unwrap().log().unwrap().clone();
    let opened = log
        .open_audience_entries(&read_identity(&m40).unwrap().unwrap())
        .unwrap()
        .unwrap();
    let held: Vec<&crate::membership_log::RecordId> = opened
        .unwrapped_keys
        .keys()
        .filter(|epoch| sealed_secret.contains(*epoch))
        .collect();
    assert!(
        held.is_empty(),
        "no key that sealed the removed value: {held:?}"
    );

    macs.server.stop();
    drop(macs.root);
}

/// A member whose vault state a pass holds ends a log run within `HANDOVER_LOCK_WAIT` with the
/// refusal that says to Allow again, as F10's handover does: the whole `ask` exchange runs inside
/// the fleet listener's handshake watchdog, so a log path that waited for the lock without bound
/// left the new Mac with a cut socket and no reason.
///
/// NEGATIVE CONTROL: in `log_path`, take the state lock with `StateLock::take` (as before): the
/// run waits for the pass this test holds, and the one-minute hang detector fails.
#[test]
fn a_busy_member_ends_a_log_run_with_the_refusal_inside_the_ask_exchanges_bound() {
    let macs = two_macs(&[(Audience::Everyone, "shared", b"everyone-v1".as_slice())]);
    let m7 = macs.m7.clone();
    let (_, request, allowed) = newcomer(macs.root.path(), &m7, "m40");
    let held = StateLock::take(&m7).expect("a pass holds m7's state");
    let (tx, rx) = std::sync::mpsc::channel();
    let member = m7.clone();
    std::thread::spawn(move || {
        let handed = hand_over(
            &member,
            &crate::members::desk::Handing {
                request: &request,
                node: [0x40; 32],
                name: allowed.name(),
                allowed: &allowed,
                reader: crate::members::bundle::Reader::LegacyLog,
                restore: false,
                node_proven: false,
                held: None,
            },
        );
        let _ = tx.send(handed.map(|handout| handout.logged));
    });
    let handed = rx
        .recv_timeout(Duration::from_secs(60))
        .expect("the run ends (a one-minute hang detector)");
    drop(held);
    let error = handed.expect_err("a busy member refuses");
    assert!(
        error.to_string().contains("Allow the new Mac again"),
        "{error}"
    );

    macs.server.stop();
    drop(macs.root);
}

#[test]
fn log_bundle_newcomer_is_materialization_ready_before_first_sync_and_fails_over_to_higher_seq_peer()
 {
    // Every server it reads is its own on loopback, which answers.
    let _answering = ServersAnswering::set();
    let macs = two_macs(&[(Audience::Everyone, "shared", b"everyone-v1".as_slice())]);
    let (m22, m7) = (&macs.m22, &macs.m7);
    let (m40, request, allowed) = newcomer(macs.root.path(), m7, "m40");

    // Before m7 admits m40, m40 asks m22's desk and a person presses Allow on m22 too.
    let mut desk22 = crate::members::desk::Desk::new(
        crate::members::desk::Local::of(m22)
            .unwrap()
            .expect("m22 local"),
    );
    let dev40 = crate::commons::bootstrap::Device::open(&m40)
        .unwrap()
        .unwrap();
    let ask_msg = crate::members::desk::ask_message(
        desk22.local().key(),
        &[0x40; 32],
        "m40",
        "Mac15,3",
        request.bytes(),
    );
    let ask_sig = dev40
        .with_seed(|seed| {
            let kp = ring::signature::Ed25519KeyPair::from_seed_unchecked(seed).unwrap();
            let mut out = [0u8; 64];
            out.copy_from_slice(kp.sign(&ask_msg).as_ref());
            Ok::<_, Error>(out)
        })
        .unwrap();
    let now_inst = Instant::now();
    assert_eq!(
        desk22
            .hear(
                &crate::members::desk::Asking {
                    wire: request.bytes(),
                    node: [0x40; 32],
                    name: "m40",
                    model: "Mac15,3",
                    signature: &ask_sig,
                },
                "192.0.2.40".parse().unwrap(),
                now_inst,
            )
            .unwrap(),
        crate::members::desk::Answer::Asking
    );
    let row22 = desk22.rows(now_inst)[0].id;
    let phrase22 = desk22.allow(row22, now_inst).unwrap();

    let (bundle, logged) = hand(m7, &request, &allowed, true);
    assert!(logged);
    write_from_log_bundle(&m40, &bundle).expect("m40 writes from the log");
    post_records_to_servers(m7).expect("m7 posts admit records");

    // Negative control: before m22 pulls the `Admit` record, refreshing m22's desk leaves m40's
    // row open (`Allowed`). Once `sync_records` brings `Admit` to m22, refreshing closes m40's
    // row and burns its phrase even without a `done` from m40 (§3.3.1 step 6).
    desk22.refresh(crate::members::desk::Local::of(m22).unwrap().unwrap());
    assert_eq!(
        desk22.rows(now_inst)[0].state,
        crate::members::desk::RowState::Allowed(phrase22)
    );
    sync_records(m22, None).expect("m22 pulls m7's admit record");
    desk22.refresh(crate::members::desk::Local::of(m22).unwrap().unwrap());
    assert_eq!(
        desk22.rows(now_inst)[0].state,
        crate::members::desk::RowState::Closed
    );
    assert_eq!(
        desk22.phrase_state(phrase22),
        Some(crate::pake::PhraseState::Burned)
    );

    // Before any sync, m40 has no cached envelope (`state.digest == None`), yet its local files
    // were materialized from the verified membership log bundle.
    let state40 = load_state(&m40).unwrap();
    let cfg40 = config(&m40).unwrap().unwrap();
    assert!(state40.digest.is_none());
    assert!(state40.records_digest.is_some());
    assert_eq!(
        log_entries::current_records_digest(&m40, &cfg40, &state40),
        state40.records_digest
    );
    assert!(materialization_ready(&m40).unwrap());
    assert!(materialization_ready_for(&m40, &cfg40).unwrap());
    assert_eq!(
        super::fleet::swarm_denied_nodes(&m40).unwrap(),
        Some(Vec::new())
    );
    let mark_ready = materialization_mark(&m40).unwrap().expect("mark");
    let observed = match observe_materialization(&m40, &cfg40, || {
        Ok(std::fs::read(m40.home.join(".secrets/shared")).unwrap())
    })
    .unwrap()
    {
        MaterializationObservation::Ready(m) => m,
        MaterializationObservation::Blocked(why) => panic!("unexpected blocker: {why:?}"),
    };
    assert_eq!(observed.inputs, b"everyone-v1");
    assert!(observed.snapshot.is_current(&m40).unwrap());

    // Negative control: corrupting a materialized file blocks readiness with LocalFilesIncomplete.
    std::fs::write(m40.home.join(".secrets/shared"), b"tampered").unwrap();
    assert!(!materialization_ready(&m40).unwrap());
    let mark_tampered = materialization_mark(&m40).unwrap().expect("mark");
    assert_ne!(mark_ready, mark_tampered);
    assert!(matches!(
        observe_materialization(&m40, &cfg40, || Ok(())).unwrap(),
        MaterializationObservation::Blocked(MaterializationBlocker::LocalFilesIncomplete)
    ));
    std::fs::write(m40.home.join(".secrets/shared"), b"everyone-v1").unwrap();
    assert!(materialization_ready(&m40).unwrap());

    // Sync m40 once against primary (`macs.server`) so m40 holds the current envelope head, still
    // without an envelope locker for m40 (only m22 and m7 are in `[machines]`).
    sync(&m40, None).expect("m40 syncs initial envelope");
    let seq_initial = load_state(&m40).unwrap().seq;
    assert!(seq_initial > 0);
    assert!(observed.snapshot.is_current(&m40).unwrap());

    // Start a peer server `s2`; publish a new version on m22 and replicate it to `s2` while
    // reverting primary (`macs.server`) to the older envelope so primary lags behind `s2`.
    let old_env = std::fs::read(m40.cache_file()).unwrap();
    let s2 = start_loopback_server(&macs.root.path().join("s2-peer"));
    let url2 = format!("http://{}/v1/{}", s2.addr, crate::ALAB_VAULT);
    {
        let admin = admin_open(m22).unwrap();
        let mut doc = admin.doc.clone();
        doc.set_entry(&Audience::Everyone, "shared", b"everyone-v2")
            .unwrap();
        admin_publish(m22, &admin, &doc).expect("m22 publishes v2");
    }
    let new_env = std::fs::read(m22.cache_file()).unwrap();
    let new_records = load_records(m22).unwrap().encode();
    transport::publish(&Source::Url(url2.clone()), &new_env).unwrap();
    transport::post_records(&url2, &new_records).unwrap();
    // Restore primary's store to the older envelope so primary serves NoNewer(seq_initial).
    std::fs::write(
        macs.root
            .path()
            .join("s1")
            .join(format!("{}.vault", crate::ALAB_VAULT)),
        &old_env,
    )
    .unwrap();

    // `peer_subsumes_held` uses `open_entries_preferring_log` so log-admitted m40 (with no locker
    // in either envelope) fails over to `s2`'s higher-seq head and materializes `everyone-v2`.
    sync_with_candidates(&m40, None, std::slice::from_ref(&url2)).expect("m40 fails over to s2");
    assert!(load_state(&m40).unwrap().seq > seq_initial);
    assert_eq!(
        std::fs::read(m40.home.join(".secrets/shared")).unwrap(),
        b"everyone-v2"
    );

    s2.stop();
    macs.server.stop();
    drop(macs.root);
}

#[test]
fn code_only_log_member_is_removed_on_admin_publish_and_not_rewrapped_by_commons_or_gap_closers() {
    let macs = two_macs(&[(Audience::Everyone, "shared", b"everyone-v1".as_slice())]);
    let (m22, m7) = (&macs.m22, &macs.m7);
    let (m40, request, allowed) = newcomer(macs.root.path(), m7, "m40");
    let m40_recipient = request.binding().recipient();

    // Simulate a key held on a `[join]` `code-only` line (e.g. let go before the log or while m7
    // could not see `[join]`): m22 records `code-only` for m40's recipient, while plain member m7
    // (which reads no `[join]`) admits m40 into the log via Allow.
    {
        let admin = admin_open(m22).unwrap();
        let mut doc = admin.doc.clone();
        doc.add_code_only(&m40_recipient.to_bech32()).unwrap();
        admin_publish(m22, &admin, &doc).expect("m22 publishes code-only");
    }
    let (bundle, logged) = hand(m7, &request, &allowed, true);
    assert!(logged);
    write_from_log_bundle(&m40, &bundle).expect("m40 writes from the log");
    post_records_to_servers(m7).expect("m7 posts admit records");

    // m22 pulls m7's admit records and publishes an update. `draft_document_diff` must draft a
    // `Body::Remove` for m40 (because its recipient is on `code-only`) and rotate both `[everyone]`
    // and `EpochAudience::Commons` without wrapping either to m40.
    sync(m22, None).expect("m22 pulls m7's admit records");
    {
        let admin = admin_open(m22).unwrap();
        let mut doc = admin.doc.clone();
        doc.set_entry(&Audience::Everyone, "shared", b"everyone-v2")
            .unwrap();
        admin_publish(m22, &admin, &doc).expect("m22 publishes v2");
    }

    let log22 = load_records(m22).unwrap().log().unwrap().clone();
    assert!(
        log22.recipient_is_removed(&m40_recipient),
        "admin publish must write Body::Remove for a log member on a code-only line"
    );
    assert!(
        !log22.wraps_maximal_audience_to(
            &crate::membership_log::EpochAudience::Everyone,
            &m40_recipient
        ),
        "new [everyone] epoch must not be wrapped to code-only m40"
    );
    assert!(
        !log22.wraps_maximal_audience_to(
            &crate::membership_log::EpochAudience::Commons,
            &m40_recipient
        ),
        "new Commons epoch must not be wrapped to code-only m40"
    );

    // Neither m22's vault passes (the ranked closer and tender) nor `close_wrap_gaps` on m7's
    // subscriber pass may re-wrap `[everyone]` or `Commons` epochs back to removed m40.
    let _ = vault_passes(m22);
    sync(m7, None).expect("m7 syncs v2 and runs gap closer");
    let log7 = load_records(m7).unwrap().log().unwrap().clone();
    assert!(log7.recipient_is_removed(&m40_recipient));
    assert!(!log7.wraps_maximal_audience_to(
        &crate::membership_log::EpochAudience::Everyone,
        &m40_recipient
    ));
    assert!(!log7.wraps_maximal_audience_to(
        &crate::membership_log::EpochAudience::Commons,
        &m40_recipient
    ));

    // Once m40 pulls the records, even before its first envelope sync (`state.digest == None`),
    // `observe_commons` on the log-only path reports `Unavailable::Revoked`.
    sync_records(&m40, None).expect("m40 pulls records");
    assert!(load_state(&m40).unwrap().digest.is_none());
    let cfg40 = config(&m40).unwrap().unwrap();
    assert!(matches!(
        observe_commons(&m40).unwrap(),
        crate::commons::Observation::Unavailable(crate::commons::Unavailable::Revoked)
    ));
    assert!(matches!(
        observe_materialization(&m40, &cfg40, || Ok(())).unwrap(),
        MaterializationObservation::Blocked(MaterializationBlocker::Revoked)
    ));
    assert!(!materialization_ready(&m40).unwrap());

    // And a full sync marks m40 Revoked, blocks materialization readiness, and never decrypts
    // `everyone-v2`.
    sync(&m40, None).expect("m40 syncs");
    assert_eq!(load_state(&m40).unwrap().membership, Stored::Revoked);
    assert!(!materialization_ready(&m40).unwrap());
    assert_eq!(
        std::fs::read(m40.home.join(".secrets/shared")).unwrap(),
        b"everyone-v1"
    );

    macs.server.stop();
    drop(macs.root);
}

#[test]
fn second_member_remove_after_sync_forms_quorum_and_voids_post_cutoff_records() {
    let macs = two_macs(&[(Audience::Everyone, "shared", b"everyone-v1".as_slice())]);
    let (m22, m7) = (&macs.m22, &macs.m7);
    let (m40, request, allowed) = newcomer(macs.root.path(), m7, "m40");
    let actor_m40 = request.binding().actor();

    let (bundle, logged) = hand(m7, &request, &allowed, true);
    assert!(logged);
    write_from_log_bundle(&m40, &bundle).expect("m40 writes from the log");
    post_records_to_servers(m7).expect("m7 posts admit records");
    sync_records(m22, None).expect("m22 pulls admit records");

    // 0. Before any `Body::Remove`, a re-key by `m22` that leaves `m40` out of `Everyone` and
    //    `Commons` is no revocation (one-mesh M1): `m40` still holds a seat no `Remove` names, so
    //    each missing `Wrap` is a gap and the vault passes heal it. Only the `Remove` below keeps
    //    `m40` out.
    let rec_m40 = request.binding().recipient();
    {
        let lock = StateLock::take(m22).unwrap();
        log_entries::retire_recipients_in_log_local(m22, &lock, &[], &BTreeSet::from([rec_m40]))
            .expect("rotate epochs excluding m40");
    }
    let healed = vault_passes(m22);
    let log22 = load_records(m22).unwrap().log().unwrap().clone();
    for audience in [EpochAudience::Everyone, EpochAudience::Commons] {
        assert!(
            log22.wraps_maximal_audience_to(&audience, &rec_m40),
            "the vault passes heal m40 into the newest {audience:?} epoch: {} signed",
            healed.len()
        );
    }
    // `vault_pass_at` leaves posting to its caller; m22's later records build on the heals.
    post_records_to_servers(m22).expect("m22 posts the re-key and its heals");
    sync_records(m7, None).expect("m7 pulls the re-key and its heals");

    // 1. `m40` has signed no records of its own. `m22` removes `m40` first.
    let minted22 = remove_member_in_log(m22, actor_m40).expect("m22 removes m40");
    assert!(!minted22.is_empty());

    // 2. Before seeing `m22`'s removal, `m40` signs a post-cutoff `Name` record and posts it.
    sync_records(&m40, None).ok();
    let dev40 = crate::commons::bootstrap::Device::open(&m40)
        .unwrap()
        .unwrap();
    let log40 = load_records(&m40).unwrap().log().unwrap().clone();
    let post_cutoff_rec = dev40
        .with_seed(|seed| {
            log40
                .draft(
                    &actor_m40,
                    99,
                    crate::membership_log::Body::Name {
                        name: "rogue-m40".into(),
                    },
                )
                .sign(seed, *request.binding().incarnation())
        })
        .unwrap();
    let post_cutoff_id = *post_cutoff_rec.id();
    save_records(
        m7,
        &crate::membership_log::encode_records(std::slice::from_ref(&post_cutoff_rec)),
    )
    .unwrap();

    // 3. `m7` syncs `m22`'s removal (so `m40` is already in `removed()` on `m7`) and also holds
    //    `m40`'s post-cutoff record. Calling `remove_member_in_log` on `m7` must reuse `m22`'s
    //    cutoff (not `m40`'s post-cutoff record, and not fail with "not active") to form an
    //    accepted quorum `Remove` that voids `post_cutoff_id`.
    sync_records(m7, None).expect("m7 pulls m22's removal");
    assert!(
        !load_records(m7)
            .unwrap()
            .log()
            .unwrap()
            .membership()
            .removal(&actor_m40)
            .unwrap()
            .quorum
    );
    let minted7 = remove_member_in_log(m7, actor_m40).expect("m7 adds quorum share");
    assert_eq!(
        minted7.len(),
        1,
        "quorum share after epochs were already rotated should only mint Body::Remove"
    );

    let log7 = load_records(m7).unwrap().log().unwrap().clone();
    let membership7 = log7.membership();
    let removal = membership7.removal(&actor_m40).expect("m40 is removed");
    assert!(
        removal.quorum,
        "m22 and m7 shares must form a quorum Remove"
    );
    assert!(
        membership7.voided().contains(&post_cutoff_id),
        "quorum Remove must void m40's post-cutoff record"
    );

    // 4. Calling `remove_member_in_log` again on `m7` is idempotent and mints nothing, and
    //    `admit_by_phrase` refuses a request from removed `m40`.
    let again = remove_member_in_log(m7, actor_m40).expect("idempotent repeat");
    assert!(again.is_empty());
    assert!(matches!(
        admit::admit_by_phrase(m7, &request, [0x40; 32], "m40", 600).unwrap(),
        admit::ByLog::KeptOut(_)
    ));

    macs.server.stop();
    drop(macs.root);
}

/// One-mesh M1, M3 and M5: narrowing `aterm-commons-control-v1` to fewer hosts than the vault's
/// members is no revocation. The narrowing still mints a Commons epoch its batch wraps to `m22`
/// alone, but `m7` is a current seat (no `Remove` names it), so its missing `Wrap` is a gap: the
/// vault pass of the same publish heals it, the overlay puts `m7` back into Commons control,
/// and `m7` reads, writes and observes Commons as before. Only an explicit removal keeps `m7`
/// out: a generic `[machines]` removal is refused while `m7` is still in Commons, the log
/// `Remove` re-keys both audiences without it, and no heal wraps a removed seat back.
///
/// NEGATIVE CONTROL: set `crate::membership_log::admission::OLD_GAP_RULE` around step 1's
/// publish (0.154.0's rule: an omission the minter knew is a revocation): `m22`'s vault pass then
/// reads `m7`'s omission from the narrowed epoch as a leaving out, no heal wraps it, and the
/// first assert of step 1 fails.
#[test]
fn narrowing_commons_control_is_a_gap_the_heal_closes_and_only_a_remove_keeps_the_member_out() {
    let macs = two_macs(&[(Audience::Everyone, "shared", b"everyone-v1".as_slice())]);
    let (m22, m7) = (&macs.m22, &macs.m7);
    let m7_id = read_identity(m7).unwrap().unwrap();
    let m7_recipient = m7_id.recipient();

    // Record the initial Commons epoch id that m7 was wrapped into at Genesis v2.
    let log_init = load_records(m22).unwrap().log().unwrap().clone();
    let init_commons_view = log_init
        .commons_epoch_view(&m7_id)
        .unwrap()
        .expect("m7 starts with Commons access");
    let init_commons_basis = init_commons_view.basis;

    // 1. Admin m22 narrows `aterm-commons-control-v1` to `["m22"]` (keeping `m7` in `[machines]`
    //    and `[everyone]`) and publishes. `draft_document_diff` rotates `EpochAudience::Commons`
    //    with a batch that omits `m7`; the publish's vault pass (`close_wrap_gaps`, m22 ranked
    //    first as the minter its own batch wraps) heals that gap at once.
    {
        let admin = admin_open(m22).unwrap();
        let mut doc = admin.doc.clone();
        let ctrl = doc
            .entries()
            .into_iter()
            .find(|e| e.name == crate::commons::ENTRY)
            .expect("commons control entry");
        doc.unset_entry(&ctrl.audience, crate::commons::ENTRY)
            .unwrap();
        doc.set_entry(
            &Audience::Hosts(vec!["m22".to_owned()]),
            crate::commons::ENTRY,
            ctrl.value.as_slice(),
        )
        .unwrap();
        admin_publish(m22, &admin, &doc).expect("publish narrowed commons control");
    }

    let log_after = load_records(m22).unwrap().log().unwrap().clone();
    let actor_m7 = *log_after
        .membership()
        .members()
        .iter()
        .find(|(_, m)| m.binding.recipient() == m7_recipient)
        .expect("m7 remains a vault member")
        .0;
    assert!(
        !log_after.audience_left_out(&crate::membership_log::EpochAudience::Commons, &actor_m7),
        "a narrowed Commons control is no Remove: m7's missing Wrap is a gap the heal closes"
    );
    assert!(
        log_after.wraps_maximal_audience_to(
            &crate::membership_log::EpochAudience::Commons,
            &m7_recipient
        ),
        "the healed newest Commons epoch wraps current seat m7"
    );
    let healed_view = log_after
        .commons_epoch_view(&m7_id)
        .unwrap()
        .expect("healed m7 has a current Commons epoch view");
    assert_ne!(
        healed_view.basis, init_commons_basis,
        "the narrowing rotated Commons; m7 holds the rotated epoch through the heal"
    );
    assert!(
        log_after
            .unwrap_commons_epoch(&init_commons_basis, &m7_id)
            .unwrap()
            .is_some(),
        "m7, still a member, keeps the Commons history it was wrapped"
    );

    // 2. Subsequent `admin_open` (which runs `overlay_log_onto_document`) puts `m7` back into
    //    `aterm-commons-control-v1` from the log, and a publish keeps it in
    //    `EpochAudience::Commons`.
    {
        let admin = admin_open(m22).unwrap();
        assert!(
            admin.commons_eligibility.includes("m7", m7_recipient),
            "overlay_log_onto_document restores healed m7 into commons_eligibility"
        );
        let mut doc = admin.doc.clone();
        doc.set_entry(&Audience::Everyone, "shared", b"everyone-v2")
            .unwrap();
        admin_publish(m22, &admin, &doc).expect("publish v2");
    }
    let log_v2 = load_records(m22).unwrap().log().unwrap().clone();
    assert!(
        log_v2.wraps_maximal_audience_to(
            &crate::membership_log::EpochAudience::Commons,
            &m7_recipient
        ),
        "subsequent admin publish keeps member m7 in EpochAudience::Commons"
    );

    // 3. On m22, `Bootstrap::access` (`current_from_loaded_log`) and `historical_commons_writers`
    //    list m7 as a Commons Writer of the rotated epoch, as they do of `init_commons_basis`.
    let boot22 = Bootstrap::required(m22).expect("m22 bootstrap");
    let access22 = boot22.access().unwrap().expect("m22 access");
    assert!(
        access22
            .members()
            .iter()
            .any(|m| m.binding().recipient() == m7_recipient),
        "current_from_loaded_log lists healed member m7"
    );
    let (_, init_writers) = log_v2
        .historical_commons_writers(&init_commons_basis)
        .expect("init commons writers");
    assert!(
        init_writers.iter().any(|b| b.recipient() == m7_recipient),
        "m7 was a writer in init_commons_basis"
    );
    let (_, rotated_writers) = log_v2
        .historical_commons_writers(access22.basis())
        .expect("rotated commons writers");
    assert!(
        rotated_writers
            .iter()
            .any(|b| b.recipient() == m7_recipient),
        "healed m7 is a writer in the rotated Commons epoch"
    );

    // 4. m7 holds its heal and observes Commons control through the envelope and through the log
    //    alone; `admit_by_phrase` and `admit_member_in_log` for already-active m7 are no-ops for
    //    Commons, and a newcomer `m8` admitted after the rotation is not listed as a writer in the
    //    superseded `init_commons_basis` epoch.
    let boot7 = Bootstrap::required(m7).expect("m7 bootstrap");
    let m7_req = boot7.request().expect("m7 request");
    sync_records(m7, None).expect("m7 pulls m22's heal");
    sync(m7, None).expect("m7 syncs v2");
    assert_eq!(
        std::fs::read(m7.home.join(".secrets/shared")).unwrap(),
        b"everyone-v2"
    );
    assert!(
        load_records(m7)
            .unwrap()
            .log()
            .unwrap()
            .wraps_maximal_audience_to(
                &crate::membership_log::EpochAudience::Commons,
                &m7_recipient
            ),
        "m7's own log holds the Wrap that healed it"
    );
    let records_before = std::fs::read(m7.records_file()).unwrap();
    assert!(
        matches!(
            crate::commons::observe(m7).unwrap(),
            crate::commons::Observation::Visible(_)
        ),
        "a narrowed Commons control excludes no member: healed m7 observes Commons control"
    );
    assert!(matches!(
        status(m7).unwrap().membership,
        Membership::In { .. }
    ));
    assert_eq!(std::fs::read(m7.records_file()).unwrap(), records_before);
    let original_state = load_state(m7).unwrap();
    let retained_cache = m7.cache_file().with_extension("retained-envelope");
    std::fs::rename(m7.cache_file(), &retained_cache).unwrap();
    let mut log_only_state = original_state.clone();
    log_only_state.digest = None;
    save_state(m7, &log_only_state).unwrap();
    let log_only_observation = crate::commons::observe(m7);
    save_state(m7, &original_state).unwrap();
    std::fs::rename(retained_cache, m7.cache_file()).unwrap();
    assert!(
        matches!(
            log_only_observation.unwrap(),
            crate::commons::Observation::Visible(_)
        ),
        "from the log alone, healed m7 observes Commons control"
    );
    assert_eq!(std::fs::read(m7.records_file()).unwrap(), records_before);
    assert!(matches!(
        admit::admit_by_phrase(m22, &m7_req, [0u8; 32], "m7", 700).unwrap(),
        admit::ByLog::Admitted(_)
    ));
    let m7_seat = log_v2.membership().get(&actor_m7).unwrap().clone();
    let m7_entrant = crate::membership_log::Entrant {
        request: m7_req,
        node_key: [0u8; 32],
        name: "m7".to_owned(),
        label: m7_seat.label,
    };
    admit_member_in_log(m22, m7_entrant, crate::membership_log::Evidence::Phrase)
        .expect("admit_member_in_log for already-active m7");
    let log_after_admit = load_records(m22).unwrap().log().unwrap().clone();
    assert!(
        log_after_admit.wraps_maximal_audience_to(
            &crate::membership_log::EpochAudience::Commons,
            &m7_recipient
        ),
        "admit_by_phrase and admit_member_in_log leave already-wrapped m7 in Commons"
    );
    assert!(
        vault_passes(m22).is_empty(),
        "the vault passes are a clean no-op once m7 is wrapped in Everyone and Commons"
    );

    let (m8, req8, _) = newcomer(macs.root.path(), m22, "m8");
    let m8_id = read_identity(&m8).unwrap().unwrap();
    admit_member_in_log(
        m22,
        crate::membership_log::Entrant {
            request: req8.clone(),
            node_key: [0x88; 32],
            name: "m8".to_owned(),
            label: "m8".to_owned(),
        },
        crate::membership_log::Evidence::Phrase,
    )
    .expect("admit newcomer m8");
    let log_with_m8 = load_records(m22).unwrap().log().unwrap().clone();
    assert!(
        log_with_m8
            .unwrap_commons_epoch(&init_commons_basis, &m8_id)
            .unwrap()
            .is_none(),
        "newcomer m8 gets no backfilled wrap of the superseded init_commons_basis: it reads that \
         history through a handoff (ACHG1)"
    );
    assert!(
        log_with_m8
            .unwrap_commons_epoch(access22.basis(), &m8_id)
            .unwrap()
            .is_some(),
        "newcomer m8 is wrapped the Commons epoch in use at its admission"
    );
    let (_, init_writers_after_m8) = log_with_m8
        .historical_commons_writers(&init_commons_basis)
        .expect("init commons writers after m8");
    assert!(
        !init_writers_after_m8.contains(req8.binding()),
        "newcomer m8 admitted after superseded epoch must not be a writer in init_commons_basis"
    );
    let (_, rotated_writers_after_m8) = log_with_m8
        .historical_commons_writers(access22.basis())
        .expect("rotated commons writers after m8");
    assert!(
        rotated_writers_after_m8.contains(req8.binding()),
        "newcomer m8 is a writer in the active Commons epoch at its admission"
    );

    // 5. `retain_records_for_commons` on an unseeded configured Mac refuses a forged `Genesis`
    //    (matching `(network, authority)` but signed by an untrusted key) and refuses unrooted
    //    non-Genesis records.
    let m9 = Paths::for_home(&macs.root.path().join("m9"));
    configure_log_member(&m9, &format!("{}#{}", macs.url, macs.signer), "m9").unwrap();
    let trust = boot22.trust();
    let forged_genesis = crate::membership_log::Record::genesis(
        &[0xEE; 32],
        crate::membership_log::Root {
            network: trust.network,
            authority: trust.authority,
        },
        vec![crate::membership_log::Founder {
            recipient: req8.binding().recipient(),
            binding: Some(req8.binding().clone()),
            node_key: None,
            name: "m8".to_owned(),
            label: "m8".to_owned(),
        }],
        9_999,
    )
    .unwrap();
    let non_genesis_wire = log_with_m8.records().nth(1).unwrap().bytes().to_vec();
    let forged_verdicts = retain_records_for_commons(
        &m9,
        trust.network,
        trust.authority,
        &[forged_genesis.bytes().to_vec(), non_genesis_wire],
    )
    .unwrap();
    assert_eq!(forged_verdicts, vec![false, false]);
    assert!(load_records(&m9).unwrap().is_empty());

    // 6. The narrowing retired nothing: a generic vault removal of m7, still a Commons member,
    //    is refused until Commons membership is retired. The explicit log `Remove` is what takes
    //    m7 out: it re-keys `EpochAudience::Everyone` and `EpochAudience::Commons` without m7,
    //    no vault pass heals the removed seat back, the generic removal then succeeds, a
    //    subsequent `admin_open` does not re-add m7, and m7 loses read access in `open_entries` /
    //    `sight` / `sync` and can no longer serve Commons enrollment.
    let pre_removal_log7 = load_records(m7).unwrap().log().unwrap().clone();
    let id7 = machine_identity(m7).unwrap();
    let r7 = id7.recipient();
    {
        let admin = admin_open(m22).unwrap();
        let mut doc_without_m7 = admin.doc.clone();
        doc_without_m7.remove_machine("m7", false).unwrap();
        assert!(
            admin_publish(m22, &admin, &doc_without_m7).is_err(),
            "a generic removal of a Commons member is refused: the narrowing retired nothing"
        );
    }
    remove_member_in_log(m22, actor_m7).expect("m22 removes m7 in log");
    let _ = vault_passes(m22);
    let log_removed = load_records(m22).unwrap().log().unwrap().clone();
    assert!(
        !log_removed.wraps_maximal_audience_to(&crate::membership_log::EpochAudience::Commons, &r7),
        "the explicit Remove re-keys Commons without m7, and no heal wraps a removed seat back"
    );
    assert!(
        !log_removed
            .wraps_maximal_audience_to(&crate::membership_log::EpochAudience::Everyone, &r7),
        "the explicit Remove re-keys Everyone without m7"
    );
    let admin = admin_open(m22).unwrap();
    let mut doc_without_m7 = admin.doc.clone();
    // admin_open's overlay may already have dropped the log-removed m7 from [machines]; the
    // [machines] edit then publishes nothing new, and the rotation below is the log Remove's.
    doc_without_m7.remove_machine("m7", false).unwrap();
    admin_publish(m22, &admin, &doc_without_m7)
        .expect("the [machines] removal of m7 publishes once the log removed it");
    assert!(
        load_records(m22)
            .unwrap()
            .log()
            .unwrap()
            .audience_left_out_recipient(&crate::membership_log::EpochAudience::Everyone, &r7),
        "the log Remove rotated EpochAudience::Everyone leaving m7 out"
    );
    assert!(
        admin_open(m22)
            .unwrap()
            .doc
            .machines()
            .iter()
            .all(|m| m.name != "m7"),
        "subsequent admin_open must not re-add [machines]-removed m7 from log membership"
    );
    sync(m7, None).unwrap();
    assert_eq!(load_state(m7).unwrap().membership, Stored::Revoked);
    assert!(sight(m7).unwrap().is_none());
    assert!(
        load_records(m7)
            .unwrap()
            .log()
            .unwrap()
            .open_entries(&machine_identity(m7).unwrap())
            .unwrap()
            .is_none(),
        "member left out of EpochAudience::Everyone must not read stale entries from older epochs"
    );
    assert!(
        check_log_commons_enrollment(m7, trust, req8.binding()).is_err(),
        "removed m7 must not serve Commons enrollment"
    );

    // 7. An `Admit` signed by `m7` outside `m22`'s removal cutoff is accepted into `m22`'s
    //    store (`m7` held a seat in its causal past) but is cut off (`!has_historical_seat`),
    //    so `admin_retire` must not treat `m11` as a known Commons identity.
    remove_member_in_log(m22, actor_m7).expect("m22's repeat of the log Remove is idempotent");
    let m11 = Paths::for_home(&macs.root.path().join("m11"));
    let _ = machine_identity(&m11).unwrap();
    let dev11 = crate::commons::bootstrap::Device::open(&m11)
        .unwrap()
        .unwrap();
    let req11 = dev11.request(trust.network, trust.authority).unwrap();
    assert!(!matches!(
        log_entries::open_local_member_signer(m7, &pre_removal_log7),
        Ok(Some(_))
    ));
    // The attacker still holds its old key; the authorized signer API must refuse it.
    let dev7 = crate::commons::bootstrap::Device::open_existing(m7).unwrap();
    let req7 = dev7.request(trust.network, trust.authority).unwrap();
    let inc7 = *req7.binding().incarnation();
    let cut_off_admit = dev7
        .with_seed(|seed| {
            pre_removal_log7
                .draft(
                    &actor_m7,
                    10_000,
                    crate::membership_log::Body::Admit {
                        entrant: crate::membership_log::Entrant {
                            request: req11.clone(),
                            node_key: [0x11; 32],
                            name: "m11".to_owned(),
                            label: "m11".to_owned(),
                        },
                        evidence: crate::membership_log::Evidence::Phrase,
                    },
                )
                .sign(seed, inc7)
        })
        .unwrap();
    let mut store22 = load_records(m22).unwrap();
    store22.ingest(cut_off_admit.bytes()).unwrap();
    save_records(m22, &store22.encode()).unwrap();
    let admin = admin_open(m22).unwrap();
    assert!(
        admin_retire(m22, &admin, &[req11.binding().recipient().to_bech32()]).is_err(),
        "cut-off Admit from removed m7 must not count as a known Commons identity in admin_retire"
    );

    // 8. `handover_bundle` on m7 refuses once m7 is removed in the membership log, never
    //    falling back to m7's cached envelope lockers.
    sync_records(m7, None).unwrap();
    let (_, _, allowed_from_m7) = newcomer(macs.root.path(), m22, "m10");
    assert!(
        handover_bundle(m7, &allowed_from_m7).is_err(),
        "removed member m7 must not hand over [everyone] files from cached envelope lockers"
    );

    // 9. Log-only admitted ALab member `m40` is eligible for `steward_alab_policy_role` and
    //    `check_standby_eligibility` until removed in the log; `steward_revoke_check` and
    //    `steward_finish_revokes` detect and complete an unmirrored membership-log removal.
    let (m40, req40, allowed40) = newcomer(macs.root.path(), m22, "m40");
    let (bundle40, logged40) = hand(m22, &req40, &allowed40, true);
    assert!(logged40);
    post_records_to_servers(m22).unwrap();
    write_from_log_bundle(&m40, &bundle40).unwrap();
    let signer = config(m22).unwrap().unwrap().signer;
    envelope::TEST_RETIRED.with(|cell| {
        *cell.borrow_mut() = Some(Retired {
            keys: Vec::new(),
            current: signer,
        });
    });
    assert_eq!(
        steward_alab_policy_role(&m40),
        Some((true, Some("m40".to_owned())))
    );
    assert_eq!(
        steward::check_standby_eligibility(&m40, None).unwrap(),
        Ok("m40".to_owned())
    );
    let admin = admin_open(m22).unwrap();
    let mut doc_with_m40_roster = admin.doc.clone();
    let seeded_roster = crate::swarm_roster::SwarmRoster {
        members: vec![crate::swarm_roster::SwarmMember {
            label: "m40".to_owned(),
            vault_name: "m40".to_owned(),
            recipient: req40.binding().recipient().to_bech32(),
            node_id: Some("node-m40".to_owned()),
            node_key: None,
            aliases: Vec::new(),
            hint: None,
            hw_hint: None,
            admitted_ms: 1,
            claimed_label: None,
            claimed_at_ms: None,
        }],
        deny_labels: vec!["m7".to_owned()],
        deny_nodes: Vec::new(),
    };
    doc_with_m40_roster
        .set_entry(
            &Audience::Everyone,
            crate::swarm_roster::ROSTER_ENTRY,
            seeded_roster.to_toml().as_bytes(),
        )
        .unwrap();
    admin_publish(m22, &admin, &doc_with_m40_roster).unwrap();
    sync(m22, None).unwrap();
    assert!(matches!(
        steward_revoke_check(m22).unwrap(),
        StewardRevoke::Current(_)
    ));

    remove_member_in_log(m22, req40.binding().actor()).unwrap();
    sync_records(&m40, None).unwrap();
    assert_eq!(steward_alab_policy_role(&m40), None);
    // Signed removal remains authoritative even while the local apply is incomplete.
    let removed_state = load_state(&m40).unwrap();
    let mut incomplete = removed_state.clone();
    incomplete.failed = true;
    incomplete
        .pending
        .insert("a-file".to_owned(), "pending".to_owned());
    save_state(&m40, &incomplete).unwrap();
    assert!(matches!(
        steward::observe_alab_role(&m40),
        steward::AlabRoleObservation::Denied
    ));
    save_state(&m40, &removed_state).unwrap();
    assert!(
        steward::check_standby_eligibility(&m40, None)
            .unwrap()
            .is_err()
    );
    assert!(matches!(
        steward_revoke_check(m22).unwrap(),
        StewardRevoke::Due { .. }
    ));
    assert!(matches!(
        steward_finish_revokes(m22).unwrap(),
        StewardRevoke::Done(_)
    ));
    sync(m22, None).unwrap();
    assert!(matches!(
        steward_revoke_check(m22).unwrap(),
        StewardRevoke::Current(_)
    ));

    // 10. Log-only member `m50` admitted in the log: `EntrySnapshot::read` succeeds across
    //     unrelated membership-log appends (admitting `m60`) while `state.seq == 0`; and when a
    //     secondary admin (`m60`, alongside `m22`) is removed in the membership log, `m60` is
    //     refused by `admin_open`, `admin_cached`, `stewardship`, and `admin_mirror_log` even
    //     before `m22` mirrors the removal envelope to the server.
    let (m50, req50, allowed50) = newcomer(macs.root.path(), m22, "m50");
    let (bundle50, logged50) = hand(m22, &req50, &allowed50, true);
    assert!(logged50);
    post_records_to_servers(m22).unwrap();
    write_from_log_bundle(&m50, &bundle50).unwrap();
    let cfg50 = config(&m50).unwrap().unwrap();
    let verified50 = read_verified_entry(&m50, &cfg50, "shared")
        .unwrap()
        .expect("log-only m50 reads shared");
    let snap50 = verified50.snapshot().clone();
    assert!(snap50.read(&m50).unwrap().is_some());

    let (m60, req60, allowed60) = newcomer(macs.root.path(), m22, "m60");
    let (bundle60, logged60) = hand(m22, &req60, &allowed60, true);
    assert!(logged60);
    post_records_to_servers(m22).unwrap();
    write_from_log_bundle(&m60, &bundle60).unwrap();

    // `m50` syncs the new membership-log records (which change `records_heads_digest` while
    // `m50` remains log-only at `state.seq == 0`): `snap50.read(&m50)` must still succeed.
    sync(&m50, None).unwrap();
    assert!(
        snap50.read(&m50).unwrap().is_some(),
        "log-only EntrySnapshot::read must succeed after unrelated log append"
    );

    let admin = admin_open(m22).unwrap();
    let mut doc_with_m60_admin = admin.doc.clone();
    doc_with_m60_admin.add_admin("m60").unwrap();
    admin_publish(m22, &admin, &doc_with_m60_admin).unwrap();
    sync(&m60, None).unwrap();
    assert!(admin_open(&m60).is_ok());
    assert!(admin_cached(&m60).is_ok());
    assert!(matches!(stewardship(&m60), Ok(Stewardship::Steward { .. })));

    remove_member_in_log(m22, req60.binding().actor()).unwrap();
    sync_records(&m60, None).unwrap();
    assert!(
        admin_open(&m60).is_err(),
        "removed secondary admin must be refused by admin_open before mirror"
    );
    assert!(
        admin_cached(&m60).is_err(),
        "removed secondary admin must be refused by admin_cached before mirror"
    );
    assert!(
        !matches!(stewardship(&m60), Ok(Stewardship::Steward { .. })),
        "removed secondary admin must not report Stewardship::Steward"
    );
    assert!(
        admin_mirror_log(&m60, &[]).unwrap().is_none(),
        "removed secondary admin must not mirror log to servers"
    );
    envelope::TEST_RETIRED.with(|cell| *cell.borrow_mut() = None);

    macs.server.stop();
    drop(macs.root);
}

/// `enroll_commons_member_in_log` runs inside a Commons enrollment (`ACE1` / `ACW2`), which one
/// deadline bounds: the records it mints go to the vault's source after it returns, so a source
/// that takes the POST and does not answer (a server that stalls, a Mac asleep mid-answer) does
/// not hold the enrollment past its own deadline. `publish_local_node_key` yields within its
/// lock bound while another local writer holds `StateLock`.
/// NEGATIVE CONTROL: posting in `enroll_commons_member_in_log` before it returns keeps it from
/// returning while the source holds the POST, and the hang detector fires.
#[test]
fn enrollment_returns_while_the_vault_source_holds_its_post() {
    use crate::commons::bootstrap::Device;
    use std::io::{Read, Write};

    let macs = two_macs(&[(Audience::Everyone, "shared", b"v1".as_slice())]);
    let (m22, m7) = (&macs.m22, &macs.m7);
    let (b22, _, _) = member_key(m22);
    let trust = b22.trust();
    let admit_new = |name: &str, byte: u8| -> MemberId {
        let p = Paths::for_home(&macs.root.path().join(name));
        configure_log_member(&p, &format!("{}#{}", macs.url, macs.signer), name).unwrap();
        let req = Device::open(&p)
            .unwrap()
            .unwrap()
            .request(trust.network, trust.authority)
            .unwrap();
        let actor = req.binding().actor();
        admit_member_in_log(
            m22,
            Entrant {
                request: req,
                node_key: [byte; 32],
                name: name.to_owned(),
                label: name.to_owned(),
            },
            Evidence::Phrase,
        )
        .unwrap();
        actor
    };
    // Removals on two islands leave concurrent Commons epochs.
    let actor8 = admit_new("m8", 0x88);
    let actor9 = admit_new("m9", 0x99);
    save_records(m7, &load_records(m22).unwrap().encode()).unwrap();
    remove_member_in_log(m22, actor8).unwrap();
    remove_member_in_log(m7, actor9).unwrap();
    save_records(m22, &load_records(m7).unwrap().encode()).unwrap();

    // The vault's source takes each POST and answers it only once the enrollment returned.
    let source = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let mut cfg = config(m22).unwrap().unwrap();
    cfg.source = Source::Url(format!(
        "http://{}/v1/alab",
        source.local_addr().expect("addr")
    ));
    save_config(m22, &cfg).unwrap();

    // When `StateLock` is held by another local writer, `publish_local_node_key` yields within
    // its 2 s lock bound instead of blocking the fleet listener watch loop indefinitely.
    {
        let _held = StateLock::take(m7).expect("hold m7 StateLock");
        use ring::signature::{Ed25519KeyPair, KeyPair as _};
        let node_pair = Ed25519KeyPair::from_seed_unchecked(&[0x77; 32]).unwrap();
        let node_pub: [u8; 32] = node_pair.public_key().as_ref().try_into().unwrap();
        let node_yielded = publish_local_node_key(m7, node_pub, |network, actor| {
            node_pair
                .sign(&crate::membership_log::node_proof_message(network, actor))
                .as_ref()
                .try_into()
                .unwrap()
        })
        .expect("publish node key");
        assert!(
            node_yielded.is_none(),
            "publish_local_node_key must yield None when StateLock is held past its bound"
        );
    }

    // `enroll_commons_member_in_log` (called inside `ACE1` / `ACW2` enrollment) posts its
    // newly minted records via `post_records_aside` so a stalled source never delays `ACW2`.
    // Admit `m10` on `m7`'s island before merging into `m22`, so `m10` lacks `Wrap`s on `m22`'s
    // newest epochs and `enroll_commons_member_in_log` on `m22` mints and posts them aside.
    let m10 = Paths::for_home(&macs.root.path().join("m10"));
    configure_log_member(&m10, &format!("{}#{}", macs.url, macs.signer), "m10").unwrap();
    let dev10 = Device::open(&m10).unwrap().unwrap();
    let req10 = dev10.request(trust.network, trust.authority).unwrap();
    admit_member_in_log(
        m7,
        Entrant {
            request: req10.clone(),
            node_key: [0xaa; 32],
            name: "m10".to_owned(),
            label: "m10".to_owned(),
        },
        Evidence::Phrase,
    )
    .unwrap();
    save_records(m22, &load_records(m7).unwrap().encode()).unwrap();

    let (mut pending10, ch10) =
        crate::commons::binding::Pending::issue(&req10, trust.network, trust.authority).unwrap();
    let ans10 = dev10.answer(&req10, &ch10).unwrap();
    let poss10 = pending10.verify(&ans10).unwrap();

    let (returned2, on_return2) = std::sync::mpsc::channel::<()>();
    let (posted2, on_post2) = std::sync::mpsc::channel::<Vec<u8>>();
    let holder2 = std::thread::spawn(move || {
        let (mut stream, _) = source.accept().expect("enrollment records posted");
        stream
            .set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
            .expect("timeout");
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") && stream.read(&mut byte).expect("read") == 1 {
            head.push(byte[0]);
        }
        let head = String::from_utf8_lossy(&head).into_owned();
        let length = head
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())
                    .flatten()
            })
            .unwrap_or(0);
        let mut body = vec![0u8; length];
        stream.read_exact(&mut body).expect("body");
        let _ = posted2.send(body);
        let answered_after = on_return2
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .is_ok();
        let _ = stream.write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n");
        answered_after
    });
    let paths2 = m22.clone();
    let worker2 = std::thread::spawn(move || {
        let chain = enroll_commons_member_in_log(&paths2, trust, req10, poss10);
        let _ = returned2.send(());
        chain
    });
    let body2 = on_post2
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("enrollment minted records reach the source");
    assert!(
        holder2.join().expect("the source"),
        "enroll_commons_member_in_log returned only once the source answered its POST"
    );
    let chain = worker2
        .join()
        .expect("the enroll thread")
        .expect("enrolled");
    assert!(!chain.is_empty());
    assert!(!body2.is_empty());
    macs.server.stop();
}

#[test]
fn log_only_member_tracks_rotated_signer_applies_log_sync_and_revokes_cascade_by_log_label() {
    let macs = two_macs(&[(Audience::Everyone, "shared", b"everyone-v1".as_slice())]);
    let m22 = &macs.m22;
    let m7 = &macs.m7;

    // 1. Rotate the vault signing key on m22 (`admin_rotate`) and sync m7 so `config.signer`
    //    is the rotated key while `config.followed` holds the genesis key.
    let admin = admin_open(m22).unwrap();
    let doc = admin.doc.clone();
    admin_rotate(m22, &admin, &doc).unwrap();
    sync(m7, None).unwrap();
    let rotated_signer = config(m22).unwrap().unwrap().signer.unwrap();

    // 2. Admit newcomer m23 (`name = "andrews-macbook"`, `label = "m23"`) via `hand_over` +
    //    `write_from_log_bundle` on m7 so m23 is a log-only member (`state.seq == 0`, no cached
    //    0.145 envelope).
    let (m50, request50, allowed50) = newcomer(macs.root.path(), m7, "andrews-macbook");
    let (bundle50, logged) = hand(m7, &request50, &allowed50, true);
    assert!(
        logged,
        "m7 must admit m50 via the membership log after key rotation"
    );
    admit::write_from_log_bundle(&m50, &bundle50).expect("m50 writes log bundle");

    // `sight` and `commons::cached` on log-only m50 must report `rotated_signer`, not the old
    // genesis signer.
    let sight50 = sight(&m50).unwrap().expect("m50 sight");
    assert_eq!(
        sight50.signer, rotated_signer,
        "log-only sight must report the current rotated vault signer"
    );
    assert_eq!(sight50.host, "andrews-macbook");
    let snap50 = crate::commons::cached(&m50)
        .unwrap()
        .expect("m50 commons snapshot");
    assert_eq!(
        snap50.signer(),
        &rotated_signer,
        "log-only Commons snapshot must report the current rotated vault signer"
    );
    let actor50 = request50.binding().actor();
    assert!(
        admit::log_admitted_actors(&m50).contains(&actor50),
        "log_admitted_actors must include active log-only member m50"
    );

    // A log-only member (`state.digest == None`, `state.records_digest.is_some()`) can also hand
    // F10's `[everyone]` files (`Handed::Files`) to a pre-F15 joiner (`reads_log == false`).
    let (_, req_pre_f15, allowed_pre_f15) = newcomer(macs.root.path(), &m50, "pre-f15-joiner");
    let (pre_f15_bundle, pre_f15_logged) = hand(&m50, &req_pre_f15, &allowed_pre_f15, false);
    assert!(
        !pre_f15_logged && pre_f15_bundle.files().is_some(),
        "log-only member m50 must serve Handed::Files to a pre-F15 joiner"
    );

    // 3. Even when m50's configured source points to a path with no 0.145 envelope
    //    (`Fetched::Missing`), `sync(&m50, None)` applies new entries from `<name>.records`
    //    via `apply_configured_log`.
    save_records(m22, &load_records(m7).unwrap().encode()).unwrap();
    let admin = admin_open(m22).unwrap();
    let mut doc = admin.doc.clone();
    doc.set_entry(
        &Audience::Hosts(vec!["andrews-macbook".to_owned()]),
        "host_secret",
        b"for-m50-only",
    )
    .unwrap();
    doc.set_entry(
        &Audience::Everyone,
        crate::SWARM_POLICY_ENTRY,
        b"release_hosts = [\"m22\"]\n",
    )
    .unwrap();
    doc.set_entry(
        &Audience::Everyone,
        "ai-pool.toml",
        b"[[account]]\nprovider = \"claude\"\nemail = \"pool@example.com\"\nwarm = [\"m23\", \"m60\"]\nsince = [1, 2]\nwhy = [\"a\", \"b\"]\nused = [3, 4]\n",
    )
    .unwrap();
    admin_publish(m22, &admin, &doc).unwrap();
    save_records(&m50, &load_records(m22).unwrap().encode()).unwrap();

    let mut cfg50 = config(&m50).unwrap().unwrap();
    let log_only_source = macs.root.path().join("no-envelope.vault");
    cfg50.source = Source::Path(log_only_source);
    save_config(&m50, &cfg50).unwrap();
    let mut state50 = load_state(&m50).unwrap();
    state50.source_identity = Some(source_identity(&cfg50));
    save_state(&m50, &state50).unwrap();

    match sync(&m50, None).expect("log-only sync without envelope") {
        SyncOutcome::Applied { report, .. } => {
            assert!(
                report.written.iter().any(|p| p.ends_with("host_secret")),
                "sync on log-only member must materialize new entry from log: {report:?}"
            );
        }
        other => panic!("expected Applied, got {other:?}"),
    }
    assert_eq!(
        read_verified_entry(&m50, &cfg50, "host_secret")
            .unwrap()
            .expect("verified host_secret on log-only m50")
            .value()
            .as_slice(),
        b"for-m50-only"
    );

    // `prepare_account_decision` on `m22` and `read_account_bound_entry` on log-only `m50` both
    // accept a `SecretRef` naming the prior genesis signer in `config.followed`.
    let admin = admin_open(m22).unwrap();
    let genesis_signer = admin.config.followed[0];
    let (boot22, journal22) =
        Bootstrap::required_with_journal_until(m22, Instant::now() + Duration::from_secs(2))
            .unwrap();
    let account_rec = admin
        .prepare_account_decision(
            &boot22,
            &journal22,
            crate::commons::accounts::Request {
                account: [0x11; 32],
                generation: 1,
                parents: Vec::new(),
                action: crate::commons::accounts::Action::Prepare(Box::new(
                    crate::commons::accounts::Preparation {
                        table: [0x22; 32],
                        key: "acct-m50".to_owned(),
                        proposal: [0x33; 32],
                        configuration: [0x44; 32],
                        provider: "claude".to_owned(),
                        method: "oauth".to_owned(),
                        device: request50.binding().clone(),
                        intent: None,
                        secret: Some(crate::commons::accounts::SecretRef {
                            vault: admin.config.name.clone(),
                            signer: genesis_signer,
                            entry: "host_secret".to_owned(),
                        }),
                    },
                )),
            },
        )
        .expect("prepare_account_decision with followed signer");
    drop(journal22);
    assert_eq!(
        read_account_bound_entry(&m50, &cfg50, &account_rec)
            .unwrap()
            .expect("read_account_bound_entry on log-only m50")
            .value()
            .as_slice(),
        b"for-m50-only"
    );

    // 4. `machine_named_in_vault` resolves log label `"m23"` to `"andrews-macbook"`, and revoking
    //    `m50` by its log label `"m23"` (`admin_revoke_stolen`) resolves `"m23"` to
    //    `"andrews-macbook"`, adds `"m23"` and its `node_key` to `aterm-swarm-roster`'s deny-lists,
    //    strips `"m23"` from `ai-pool.toml`, and names `"m23"` in `needs_you_line`.
    assert_eq!(
        machine_named_in_vault(m22, &admin, "m23").unwrap(),
        "andrews-macbook"
    );
    let stolen = admin_revoke_stolen(m22, &admin, "m23", true).expect("revoke m23 by log label");
    assert!(
        stolen.needs_you_line.contains("m23"),
        "expected m23 in needs_you_line, got: {}",
        stolen.needs_you_line
    );
    let admin = admin_open(m22).unwrap();
    let roster = crate::swarm_roster::SwarmRoster::from_document(&admin.published_doc)
        .unwrap()
        .expect("roster after revoke");
    assert!(roster.deny_labels.contains(&"m23".to_owned()));
    assert!(
        roster
            .deny_nodes
            .contains(&hex(&request50.binding().recipient().0))
    );
    let pool_text = std::str::from_utf8(
        admin
            .published_doc
            .entry(&Audience::Everyone, "ai-pool.toml")
            .unwrap()
            .value
            .as_slice(),
    )
    .unwrap();
    assert!(!pool_text.contains("\"m23\""), "{pool_text}");
    assert!(pool_text.contains("\"m60\""), "{pool_text}");
    assert!(
        !admit::log_admitted_actors(m22).contains(&actor50),
        "log_admitted_actors must exclude m50 once left out of EpochAudience::Everyone"
    );
    assert!(
        handover::handover_bundle(m22, &allowed50).is_err(),
        "handover_bundle must refuse m50 once left out of EpochAudience::Everyone"
    );

    // 5. Admit `m60` and remove `m60` purely in the membership log (never mirrored into
    //    `doc.machines()`). `steward_revoke` and `steward_finish_revokes` must detect `m60` in
    //    `log.membership().removed_labels()` / `removed_node_keys()`, add `m60` to
    //    `aterm-swarm-roster`'s deny-lists, and strip `m60` from `ai-pool.toml`.
    let (b22, _, _) = member_key(m22);
    let trust = b22.trust();
    let m60 = Paths::for_home(&macs.root.path().join("m60"));
    configure_log_member(&m60, &format!("{}#{}", macs.url, macs.signer), "m60").unwrap();
    let req60 = Device::open(&m60)
        .unwrap()
        .unwrap()
        .request(trust.network, trust.authority)
        .unwrap();
    let actor60 = req60.binding().actor();
    // A listener's memoized deny list (`swarm_denied_nodes_now`) and the node's memoized roster
    // (`swarm_roster_now`), read before `m60` is admitted and removed, take in the log-only
    // removal at their next read. NEGATIVE CONTROLS (2026-11-01): with the memo never reading
    // again (`true` for its stamp comparison), the memoized list below lacks `[0x60; 32]` and the
    // `assert_eq!` against `pre_mirror_denied` fails; with `swarm_roster_now` alone never reading
    // again (a `OnceLock` of its first call's roster in place of `MEMO.read`), it answers the
    // roster read before, without `m60`, and the `assert_eq!` against `pre_mirror_roster` fails.
    let primed = swarm_denied_nodes_now(m22)
        .unwrap()
        .expect("primed swarm_denied_nodes_now");
    assert!(!primed.contains(&hex(&[0x60; 32])));
    let primed_roster = swarm_roster_until(
        m22,
        std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN,
    )
    .unwrap()
    .expect("primed swarm_roster_now");
    assert!(!primed_roster.deny_labels.contains(&"m60".to_owned()));
    admit_member_in_log(
        m22,
        Entrant {
            request: req60,
            node_key: [0x60; 32],
            name: "mac-sixty".to_owned(),
            label: "m60".to_owned(),
        },
        Evidence::Phrase,
    )
    .unwrap();
    remove_member_in_log(m22, actor60).unwrap();

    // Even before `steward_finish_revokes` mirrors `m60`'s removal into `aterm-swarm-roster`,
    // The bounded roster read and `swarm_denied_nodes` immediately include `m60` and
    // `[0x60; 32]` from the local membership log.
    let pre_mirror_roster = swarm_roster_until(
        m22,
        std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN,
    )
    .unwrap()
    .expect("pre-mirror swarm_roster");
    assert!(pre_mirror_roster.deny_labels.contains(&"m60".to_owned()));
    assert!(pre_mirror_roster.deny_nodes.contains(&hex(&[0x60; 32])));
    let pre_mirror_denied = fleet::swarm_denied_nodes(m22)
        .unwrap()
        .expect("pre-mirror swarm_denied_nodes");
    assert!(pre_mirror_denied.contains(&hex(&[0x60; 32])));
    assert_eq!(
        swarm_denied_nodes_now(m22)
            .unwrap()
            .expect("memoized swarm_denied_nodes_now"),
        pre_mirror_denied
    );
    assert_eq!(
        swarm_roster_until(
            m22,
            std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN
        )
        .unwrap()
        .expect("memoized swarm_roster_now"),
        pre_mirror_roster
    );

    envelope::TEST_RETIRED.with(|cell| {
        *cell.borrow_mut() = Some(Retired {
            keys: Vec::new(),
            current: Some(rotated_signer),
        });
    });
    let check_res = steward_revoke_check(m22).unwrap();
    let finish_res = steward_finish_revokes(m22).unwrap();
    envelope::TEST_RETIRED.with(|cell| *cell.borrow_mut() = None);

    assert!(
        matches!(check_res, StewardRevoke::Due { .. }),
        "steward_revoke_check must detect log-only removed m60, got {check_res:?}"
    );
    assert!(
        matches!(finish_res, StewardRevoke::Done(_)),
        "steward_finish_revokes must clean up log-only removed m60, got {finish_res:?}"
    );
    let admin = admin_open(m22).unwrap();
    let roster = crate::swarm_roster::SwarmRoster::from_document(&admin.published_doc)
        .unwrap()
        .expect("roster after steward_finish_revokes");
    assert!(roster.deny_labels.contains(&"m60".to_owned()));
    assert!(roster.deny_nodes.contains(&hex(&[0x60; 32])));
    let pool_text = std::str::from_utf8(
        admin
            .published_doc
            .entry(&Audience::Everyone, "ai-pool.toml")
            .unwrap()
            .value
            .as_slice(),
    )
    .unwrap();
    assert!(!pool_text.contains("\"m60\""), "{pool_text}");

    // 7. When an envelope member's recipient (`m7`) is excluded from `EpochAudience::Everyone`
    //    via `retire_recipients_in_log_local` without a `Body::Remove` record (as in a half-done
    //    revoke of an unproved founder), `observe_commons` on `m7` must report
    //    `Observation::Unavailable(Unavailable::Revoked)` rather than falling back to decrypting
    //    the cached envelope's locker. `m7` still holds a seat no `Remove` names, so the omission
    //    is a gap, not a revocation (one-mesh M1): the vault pass of the steward's publish heals
    //    it, and the roster the publish mirrors denies no `m7`.
    let m7_rec = read_identity(m7).unwrap().unwrap().recipient();
    {
        let lock = StateLock::take(m22).unwrap();
        log_entries::retire_recipients_in_log_local(m22, &lock, &[], &BTreeSet::from([m7_rec]))
            .unwrap();
    }
    save_records(m7, &load_records(m22).unwrap().encode()).unwrap();
    let obs = observe_commons(m7).unwrap();
    assert!(
        matches!(
            obs,
            crate::commons::Observation::Unavailable(crate::commons::Unavailable::Revoked)
        ),
        "m7 excluded from [everyone] must report Revoked in observe_commons, got {obs:?}"
    );
    envelope::TEST_RETIRED.with(|cell| {
        *cell.borrow_mut() = Some(Retired {
            keys: Vec::new(),
            current: Some(rotated_signer),
        });
    });
    let check_m7 = steward_revoke_check(m22).unwrap();
    let finish_m7 = steward_finish_revokes(m22).unwrap();
    envelope::TEST_RETIRED.with(|cell| *cell.borrow_mut() = None);
    assert!(
        matches!(check_m7, StewardRevoke::Due { .. }),
        "steward_revoke_check must detect [everyone]-excluded m7, got {check_m7:?}"
    );
    assert!(
        matches!(finish_m7, StewardRevoke::Done(_)),
        "steward_finish_revokes must clean up [everyone]-excluded m7, got {finish_m7:?}"
    );
    let admin = admin_open(m22).unwrap();
    let roster = crate::swarm_roster::SwarmRoster::from_document(&admin.published_doc)
        .unwrap()
        .expect("roster after m7 steward_finish_revokes");
    let log_now = load_records(m22).unwrap().log().unwrap().clone();
    assert!(
        log_now.wraps_maximal_audience_to(&EpochAudience::Everyone, &m7_rec),
        "the heal wraps m7 into the newest [everyone] epoch"
    );
    assert!(!roster.deny_labels.contains(&"m7".to_owned()));

    // 8. `machine_denied_by_roster_or_log` honors an active log/roster seat (`m22`) even when
    //    passed a stale denied `[machines]` host name (`"m7"`), while denying a seatless
    //    recipient whose `[machines]` host name is denied.
    let mut roster = roster;
    roster.deny_labels.push("m7".to_owned());
    let m22_rec = read_identity(m22).unwrap().unwrap().recipient();
    let seatless_rec = Identity::from_secret_bytes([0x99; 32]).recipient();
    assert!(
        !log_entries::machine_denied_by_roster_or_log(Some(&roster), &log_now, "m7", &m22_rec),
        "active member m22 must not be denied by a stale denied host label"
    );
    assert!(
        log_entries::machine_denied_by_roster_or_log(Some(&roster), &log_now, "m7", &seatless_rec),
        "seatless machine with denied host label m7 must be denied"
    );

    macs.server.stop();
}

/// m7, a member with no admin locker, puts `shared = value` in the log alone, and the records
/// reach the vault's server and m22.
fn put_in_log_only(macs: &TwoMacs, value: &[u8]) {
    let (m22, m7) = (&macs.m22, &macs.m7);
    let log = load_records(m7).unwrap().log().unwrap().clone();
    let id7 = read_identity(m7).unwrap().unwrap();
    let opened = log.open_audience_entries(&id7).unwrap().unwrap();
    let epoch = opened.maximal_epochs[&EpochAudience::Everyone][0];
    let key = *opened.unwrapped_keys[&epoch];
    let (b7, a7, i7) = member_key(m7);
    let entry = Entry {
        name: "shared".to_owned(),
        value: SecretBytes::from(value),
        audience: Audience::Everyone,
        if_absent: false,
        anchor: None,
    };
    let put = b7
        .device()
        .with_seed(|seed| {
            log.draft_put(&a7, 9_000, epoch, &key, &entry)
                .unwrap()
                .sign(seed, i7)
        })
        .unwrap();
    transport::post_records(&macs.url, &encode_records(std::slice::from_ref(&put))).unwrap();
    sync_records(m22, None).expect("m22 takes the Put");
}

/// F20's wire behavior with a synthetic unproved subscriber: an observation within 14 days keeps
/// the mirror on, so a log-only edit reaches the envelope. This does not execute a 0.145 binary.
///
/// NEGATIVE CONTROL: make `mirror_still_needed` return `false` whatever was heard (the mirror
/// stopped early): the first mirror publishes nothing, and the 0.145 Mac keeps reading `v1`.
#[test]
fn a_0_145_member_heard_in_14_days_keeps_the_mirror_on() {
    use std::sync::atomic::{AtomicU64, Ordering};
    const START: u64 = 1_800_000_000;
    static CLOCK: AtomicU64 = AtomicU64::new(START);
    fn clock() -> u64 {
        CLOCK.load(Ordering::SeqCst)
    }
    // Every server it reads is its own on loopback, which answers.
    let _answering = ServersAnswering::set();
    let macs = two_macs_at(
        &[(Audience::Everyone, "shared", b"v1".as_slice())],
        false,
        Some(clock),
    );
    assert!(
        transport::fetch_heard(&macs.url).is_err(),
        "new telemetry is not a complete history"
    );
    CLOCK.store(
        START + log_entries::OLD_MAC_WINDOW_SECS + 1,
        Ordering::SeqCst,
    );
    let old = Identity::from_secret_bytes([0x45; 32])
        .recipient()
        .to_bech32();
    transport::fetch_within_as(&macs.url, None, Duration::from_secs(60), Some(&old))
        .expect("the 0.145 Mac subscribes");
    let heard = transport::fetch_heard(&macs.url)
        .unwrap()
        .expect("the route answers");
    assert!(
        heard
            .subscribers
            .contains_key(&transport::heard_tag(&Recipient::parse(&old).unwrap())),
        "{heard:?}"
    );
    assert!(
        !heard.subscribers.keys().any(|tag| tag.contains("age1")),
        "the route names no key"
    );

    put_in_log_only(&macs, b"v2");
    let id7 = read_identity(&macs.m7).unwrap().unwrap();
    assert!(
        admin_mirror_log(&macs.m22, &[]).unwrap().is_some(),
        "an unproved Mac heard today keeps the mirror on"
    );
    let (_, view) = envelope_view(&macs.url, &id7);
    assert_eq!(
        view.get("shared").map(Vec::as_slice),
        Some(b"v2".as_slice())
    );
    macs.server.stop();

    // No unproved Mac heard: the members read the log, and nothing reads a mirror.
    CLOCK.store(START, Ordering::SeqCst);
    let quiet = two_macs_at(
        &[(Audience::Everyone, "shared", b"v1".as_slice())],
        false,
        Some(clock),
    );
    CLOCK.store(
        START + log_entries::OLD_MAC_WINDOW_SECS + 1,
        Ordering::SeqCst,
    );
    put_in_log_only(&quiet, b"v2");
    let quiet_id = id7_of(&quiet);
    let quiet_recipient = quiet_id.recipient().to_bech32();
    let (seq_before, _) = envelope_view_as(&quiet.url, &quiet_id, Some(&quiet_recipient));
    assert_eq!(admin_mirror_log(&quiet.m22, &[]).unwrap(), None);
    let (seq_after, view) = envelope_view_as(&quiet.url, &quiet_id, Some(&quiet_recipient));
    assert_eq!(seq_after, seq_before, "the mirror stopped");
    assert_eq!(
        view.get("shared").map(Vec::as_slice),
        Some(b"v1".as_slice())
    );
    sync(&quiet.m7, None).expect("the current member reads the log without a new mirror");
    assert_eq!(
        std::fs::read(quiet.m7.home.join(".secrets/shared")).unwrap(),
        b"v2"
    );
    // Even a complete quiet history cannot suppress the envelope that removes a machine.
    let removed = id7_of(&quiet);
    let (_, actor, _) = member_key(&quiet.m7);
    remove_member_in_log(&quiet.m22, actor).expect("remove m7 in the log");
    assert!(
        admin_mirror_log(&quiet.m22, &[]).unwrap().is_some(),
        "membership removal must reseal"
    );
    let Fetched::Body(served) = transport::fetch(
        &Source::Url(quiet.url.clone()),
        None,
        Duration::from_secs(60),
    )
    .unwrap() else {
        panic!("envelope");
    };
    assert!(
        open_lockers(&envelope::parse(&served).unwrap(), &removed)
            .unwrap()
            .is_none(),
        "a removed machine still opens the new mirror"
    );
    quiet.server.stop();
}

/// F20 beside an admin at work: an admin that opens the vault (as its approver, the ALab release
/// check and the steward's lease do routinely) names itself to the vault's server and to each
/// candidate it probes, so its own reads keep no mirror on once every plain read is past the
/// window.
///
/// NEGATIVE CONTROL: read without naming the reader in `admin_open_with_candidates` (plain
/// `transport::fetch` for the primary, or plain `fetch_within` for the probes): the open books a
/// plain read on that server inside the window, and the mirror is published.
#[test]
fn an_admin_opening_the_vault_keeps_no_mirror_on() {
    use std::sync::atomic::{AtomicU64, Ordering};
    const START: u64 = 1_820_000_000;
    static CLOCK: AtomicU64 = AtomicU64::new(START);
    fn clock() -> u64 {
        CLOCK.load(Ordering::SeqCst)
    }
    let macs = two_macs_at(
        &[(Audience::Everyone, "shared", b"v1".as_slice())],
        false,
        Some(clock),
    );
    let peer = start_loopback_server_at(&macs.root.path().join("peer"), Some(clock));
    let peer_url = format!("http://{}/v1/alab", peer.addr);
    let envelope = std::fs::read(macs.m22.cache_file()).expect("envelope");
    transport::publish(&Source::Url(peer_url.clone()), &envelope).expect("seed peer");
    let candidates = [peer_url];
    CLOCK.store(
        START + log_entries::OLD_MAC_WINDOW_SECS + 1,
        Ordering::SeqCst,
    );

    admin_open_with_candidates(&macs.m22, &candidates).expect("the admin opens the vault");
    put_in_log_only(&macs, b"v2");
    assert_eq!(
        admin_mirror_log(&macs.m22, &candidates).unwrap(),
        None,
        "the admin's own reads kept the mirror on"
    );
    peer.stop();
    macs.server.stop();
}

fn id7_of(macs: &TwoMacs) -> Identity {
    read_identity(&macs.m7).unwrap().unwrap()
}

/// F20's gate on its own: unknown keeps the mirror on; an unproved recipient counts only within
/// 14 days; members' own recipients never keep it on.
#[test]
fn the_mirror_gate_counts_only_unproved_macs_heard_in_14_days() {
    let macs = two_macs(&[]);
    let log = load_records(&macs.m22).unwrap().log().unwrap().clone();
    let members: Vec<Recipient> = [&macs.m22, &macs.m7]
        .iter()
        .map(|p| read_identity(p).unwrap().unwrap().recipient())
        .collect();
    let old = Identity::from_secret_bytes([0x46; 32]).recipient();
    let now = 2_000_000_000;
    let day = 24 * 60 * 60;
    let heard = |extra: Option<u64>| {
        let mut map: BTreeMap<String, u64> = members
            .iter()
            .map(|r| (transport::heard_tag(r), now))
            .collect();
        if let Some(at) = extra {
            map.insert(transport::heard_tag(&old), at);
        }
        transport::Heard {
            as_of: now,
            subscribers: map,
            anonymous: None,
        }
    };
    assert!(log_entries::mirror_still_needed(&log, None, now));
    assert!(!log_entries::mirror_still_needed(
        &log,
        Some(&heard(None)),
        now
    ));
    assert!(log_entries::mirror_still_needed(
        &log,
        Some(&heard(Some(now - 13 * day))),
        now
    ));
    assert!(log_entries::mirror_still_needed(
        &log,
        Some(&heard(Some(now - 14 * day))),
        now
    ));
    assert!(!log_entries::mirror_still_needed(
        &log,
        Some(&heard(Some(now - 14 * day - 1))),
        now
    ));
    assert!(!log_entries::mirror_still_needed(
        &log,
        Some(&heard(Some(now - 15 * day))),
        now
    ));
    macs.server.stop();
}

/// A modern server cannot speak for a server whose heard route is unsupported or unavailable.
/// The second real loopback server first answers the same 404 an old server does, then holds
/// the vault and supplies its own observations to the union.
#[test]
fn every_server_must_answer_before_the_mirror_can_stop() {
    use std::sync::atomic::{AtomicU64, Ordering};
    const START: u64 = 1_810_000_000;
    static CLOCK: AtomicU64 = AtomicU64::new(START);
    fn clock() -> u64 {
        CLOCK.load(Ordering::SeqCst)
    }
    let macs = two_macs_at(&[], false, Some(clock));
    let log = load_records(&macs.m22).unwrap().log().unwrap().clone();
    let peer = start_loopback_server_at(&macs.root.path().join("peer"), Some(clock));
    let peer_url = format!("http://{}/v1/alab", peer.addr);
    let now = aterm_time::unix_secs().expect("clock");
    CLOCK.store(
        START + log_entries::OLD_MAC_WINDOW_SECS - 1,
        Ordering::SeqCst,
    );
    assert!(
        log_entries::heard_across(std::slice::from_ref(&macs.url), now).is_none(),
        "partial window became complete"
    );
    CLOCK.store(
        START + log_entries::OLD_MAC_WINDOW_SECS + 1,
        Ordering::SeqCst,
    );
    assert!(transport::fetch_heard(&peer_url).unwrap().is_none());
    for urls in [
        vec![macs.url.clone(), peer_url.clone()],
        vec![peer_url.clone(), macs.url.clone()],
        Vec::new(),
    ] {
        let heard = log_entries::heard_across(&urls, now);
        assert!(heard.is_none(), "an unanswered server was treated as empty");
        assert!(log_entries::mirror_still_needed(&log, heard.as_ref(), now));
    }

    let unavailable = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let unavailable_url = format!("http://{}/v1/alab", unavailable.local_addr().unwrap());
    drop(unavailable);
    assert!(log_entries::heard_across(&[macs.url.clone(), unavailable_url], now).is_none());

    let envelope = std::fs::read(macs.m22.cache_file()).expect("envelope");
    transport::publish(&Source::Url(peer_url.clone()), &envelope).expect("seed peer");
    let urls = [macs.url.clone(), peer_url.clone()];
    let heard = log_entries::heard_across(&urls, now).expect("both modern servers answer");
    assert!(!log_entries::mirror_still_needed(&log, Some(&heard), now));

    // A plain GET is the tagged 0.145 request shape, with neither new purpose header. The
    // current transport supplies that exact absence of identity; no old executable is run.
    transport::fetch_within(&peer_url, None, Duration::from_secs(60)).unwrap();
    let heard = log_entries::heard_across(&urls, now).unwrap();
    assert_eq!(heard.anonymous, Some(now));
    assert!(log_entries::mirror_still_needed(&log, Some(&heard), now));
    CLOCK.store(
        START + 2 * log_entries::OLD_MAC_WINDOW_SECS + 1,
        Ordering::SeqCst,
    );
    transport::observe_after_nowait(&peer_url, None, Duration::from_secs(60)).unwrap();
    let heard = log_entries::heard_across(&urls, now).unwrap();
    assert!(
        log_entries::mirror_still_needed(&log, Some(&heard), now),
        "the exact anonymous TTL was excluded"
    );
    CLOCK.store(
        START + 2 * log_entries::OLD_MAC_WINDOW_SECS + 2,
        Ordering::SeqCst,
    );
    transport::observe_after_nowait(&peer_url, None, Duration::from_secs(60)).unwrap();
    let heard = log_entries::heard_across(&urls, now).unwrap();
    assert!(
        !log_entries::mirror_still_needed(&log, Some(&heard), now),
        "an observer renewed an expired anonymous consumer"
    );

    let old = Identity::from_secret_bytes([0x47; 32]).recipient();
    transport::fetch_within_as(
        &peer_url,
        None,
        Duration::from_secs(60),
        Some(&old.to_bech32()),
    )
    .expect("the unproved Mac subscribes only to the second server");
    let heard = log_entries::heard_across(&urls, now).expect("both servers answer");
    assert!(heard.subscribers.contains_key(&transport::heard_tag(&old)));
    assert!(log_entries::mirror_still_needed(&log, Some(&heard), now));
    peer.stop();
    macs.server.stop();
}

#[test]
fn pending_request_members_are_verified_without_admin_credentials() {
    let macs = two_macs(&[]);
    assert!(admin_open(&macs.m7).is_err());
    let config = config(&macs.m7).unwrap().unwrap();
    let members = request_members(&macs.m7, &config, None).unwrap().unwrap();
    let member = machine_identity(&macs.m7).unwrap().recipient().to_bech32();
    assert!(members.contains(&member));
    let stranger = crate::age::Identity::from_secret_bytes([79; 32])
        .recipient()
        .to_bech32();
    assert!(!members.contains(&stranger));
    let empty = Paths::for_home(&macs.root.path().join("untrusted-reader"));
    assert!(request_members(&empty, &config, None).unwrap().is_none());
}

#[test]
fn an_admins_approved_recipient_is_not_pending_while_enrollment_is_unfinished() {
    let macs = two_macs(&[]);
    let admin = admin_open(&macs.m22).unwrap();
    let recipient = Identity::from_secret_bytes([79; 32])
        .recipient()
        .to_bech32();
    let mut doc = admin.doc.clone();
    doc.add_machine("awaiting-enrollment", &recipient).unwrap();
    let membership = request_members(&macs.m22, &admin.config, None)
        .unwrap()
        .expect("the administrator already has authenticated membership");
    assert!(!membership.contains(&recipient));
    let approved = request_members(&macs.m22, &admin.config, Some(&doc))
        .unwrap()
        .unwrap();
    assert!(
        approved.contains(&recipient),
        "an existing log must not hide the administrator's approval"
    );
    assert!(membership.iter().all(|member| approved.contains(member)));
    assert!(
        !request_members(&macs.m22, &admin.config, None)
            .unwrap()
            .unwrap()
            .contains(&recipient),
        "filtering a request must not enroll its recipient"
    );
}

/// One-mesh M4's pending writes ([`log_entries::wait_for_key`]): each set waits on its own version
/// and lands when a server is seen to hold that version, whatever became of the others. A
/// person's later write of a name wins whichever version is read first, as it does where two
/// servers took different versions of one sequence: a later version that lands takes its names
/// out of every earlier set. A version this publish builds on, or a parent of it, lands its set
/// before the publish. A refusal drops only its set.
///
/// NEGATIVE CONTROL: `land_sets` without the `retain` that takes a landing set's names out of
/// the earlier sets (restored with cp from a backup plus touch, confirmed with cmp): v1's set keeps
/// `shared`, which, read second, would put it back to `v1`, and the test fails "v1's set stays
/// pending, less the name v2 wrote".
#[test]
fn pending_waiting_sets_land_by_version_and_a_later_write_of_a_name_wins() {
    let macs = two_macs(&[]);
    let m22 = &macs.m22;
    let identity = read_identity(m22).unwrap().unwrap();
    let state = || log_entries::waiting_and_pending(m22, &identity);
    let values = |pairs: &[(&str, &[u8])]| -> log_entries::Values {
        pairs
            .iter()
            .map(|&(name, value)| (name.to_owned(), value.to_vec()))
            .collect()
    };
    let wait = |writes: &[(&str, &[u8])], version: u8, stored: &[[u8; 32]]| {
        log_entries::wait_for_key(
            m22,
            &identity,
            &log_entries::Waiting::of_puts(writes),
            u64::from(version),
            &[version; 32],
            &["https://one.example".to_owned()],
            stored,
            None,
        )
        .expect("pending");
    };

    // Two publishes from one text: v1 (`shared` = v1, `only1`), then v2 (`shared` = v2).
    wait(&[("shared", b"v1"), ("only1", b"a")], 1, &[]);
    wait(&[("shared", b"v2")], 2, &[]);
    let (waiting, pending) = state();
    assert!(
        waiting.is_empty(),
        "CONTROL: nothing waits before a server is seen"
    );
    assert_eq!(pending.len(), 2, "CONTROL: both sets are pending");

    // One server holds v2, read first; another holds v1, read after.
    log_entries::land_waiting(m22, &identity, &[[2; 32]], None).expect("land v2");
    let (waiting, pending) = state();
    assert_eq!(waiting, values(&[("shared", b"v2")]));
    assert_eq!(
        pending,
        vec![([1; 32], values(&[("only1", b"a")]))],
        "v1's set stays pending, less the name v2 wrote"
    );
    log_entries::land_waiting(m22, &identity, &[[1; 32]], None).expect("land v1");
    let (waiting, pending) = state();
    assert_eq!(
        waiting.get("shared").map(Vec::as_slice),
        Some(b"v2".as_slice()),
        "the later publish's write of shared wins"
    );
    assert_eq!(waiting, values(&[("shared", b"v2"), ("only1", b"a")]));
    assert!(pending.is_empty(), "nothing is pending after both land");

    // A publish built on a version whose parent is pending lands that parent's set first.
    wait(&[("x", b"3")], 3, &[]);
    wait(&[("y", b"4")], 4, &[[9; 32], [3; 32]]);
    let (waiting, pending) = state();
    assert_eq!(
        waiting.get("x").map(Vec::as_slice),
        Some(b"3".as_slice()),
        "the parent's set lands"
    );
    assert_eq!(pending, vec![([4; 32], values(&[("y", b"4")]))]);

    // A refusal drops only its set.
    wait(&[("w", b"5")], 5, &[]);
    wait(&[("v", b"6")], 6, &[]);
    log_entries::forget_waiting(m22, &[6; 32]);
    let (_, pending) = state();
    assert_eq!(
        pending,
        vec![
            ([4; 32], values(&[("y", b"4")])),
            ([5; 32], values(&[("w", b"5")]))
        ]
    );
    macs.server.stop();
}

/// What [`unread_set`] saw.
struct UnreadSet {
    macs: TwoMacs,
    /// The pending set's file m22 cannot read.
    file: PathBuf,
    /// The files m22 named as unread before anything ran over them.
    unread: (Option<String>, Vec<String>),
    /// A publish's keep of a write, and one that keeps nothing, over the unread set.
    keeps: crate::Result<()>,
    keeps_nothing: crate::Result<()>,
    /// A read of the unread set's version as stored.
    landed: crate::Result<()>,
    /// How many records a vault pass signed over it.
    signed: crate::Result<usize>,
    /// Whether the file is still there after all of that.
    there: bool,
}

/// m22 lands v2's set, so `late` waits; then v3's pending set no longer opens with m22's key.
/// Over it m22 keeps a publish's write, keeps one that has none, reads v3 as stored and runs a
/// vault pass, `control` setting [`log_entries::UNREAD_AS_NONE`] for all of them.
fn unread_set(control: bool) -> UnreadSet {
    let macs = two_macs(&[(Audience::Everyone, "shared", b"v1".as_slice())]);
    let m22 = macs.m22.clone();
    let identity = read_identity(&m22).unwrap().unwrap();
    let wait = |writes: &log_entries::Waiting, version: u8| {
        log_entries::wait_for_key(
            &m22,
            &identity,
            writes,
            u64::from(version),
            &[version; 32],
            &["https://one.example".to_owned()],
            &[],
            None,
        )
    };
    wait(&log_entries::Waiting::of_puts(&[("late", b"2")]), 2).expect("pending");
    log_entries::land_waiting(&m22, &identity, &[[2; 32]], None).expect("land v2");
    wait(&log_entries::Waiting::of_puts(&[("other", b"3")]), 3).expect("pending");
    let hex = aterm_codec::hex::encode_lower(&[3; 32]);
    let file = std::fs::read_dir(m22.dir.join("everyone.pending"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.to_string_lossy().ends_with(&hex))
        .expect("CONTROL: v3's set has a file");
    fsx::write_atomic(&file, b"not sealed", 0o600).unwrap();
    let unread = log_entries::unread_waiting(&m22, &identity);

    log_entries::UNREAD_AS_NONE.with(|cell| cell.set(control));
    let keeps = wait(&log_entries::Waiting::of_puts(&[("new", b"4")]), 4);
    let keeps_nothing = wait(&log_entries::Waiting::default(), 5);
    let landed = log_entries::land_waiting(&m22, &identity, &[[3; 32]], None);
    let signed = log_entries::sign_waiting_writes(&m22, aterm_time::unix_millis().unwrap_or(0))
        .map(|records| records.len());
    log_entries::UNREAD_AS_NONE.with(|cell| cell.set(false));
    let there = file.exists();
    UnreadSet {
        macs,
        file,
        unread,
        keeps,
        keeps_nothing,
        landed,
        signed,
        there,
    }
}

/// One-mesh W3 ([`unread_set`]): a pending set this Mac cannot read is never read as none, since
/// it may be the writes of a version a server stored. A publish that keeps a write refuses before
/// its version goes out, naming why, and one that keeps nothing (a removal's, an approval's) goes.
/// Nothing lands over it, and a vault pass signs nothing: any waiting write may be one of the
/// names it writes. The file stays and is named as unread, and only every server's refusal of its
/// version, found by the file's name, drops it.
///
/// NEGATIVE CONTROL: [`log_entries::UNREAD_AS_NONE`] (a set that does not open reads as none and
/// its file is removed, as before W3), run in the test: the publish keeps its write, the file goes
/// and the pass signs `late`, as the CONTROL asserts say; with the switch set for the first run
/// too, the test fails "a publish that keeps a write refuses over a set that does not open".
#[test]
fn a_pending_set_that_does_not_open_is_never_read_as_none() {
    let run = unread_set(false);
    let m22 = &run.macs.m22;
    let identity = read_identity(m22).unwrap().unwrap();
    assert!(
        run.keeps
            .as_ref()
            .is_err_and(|error| error.to_string().contains("does not open")),
        "a publish that keeps a write refuses over a set that does not open: {:?}",
        run.keeps
    );
    assert!(
        run.keeps_nothing.is_ok(),
        "a publish that keeps nothing goes: {:?}",
        run.keeps_nothing
    );
    assert!(run.landed.is_err(), "nothing lands over it");
    assert!(
        matches!(run.signed, Ok(0)),
        "nothing waiting is signed over it: {:?}",
        run.signed
    );
    assert!(run.there, "the file stays");
    assert_eq!(
        run.unread,
        (None, vec![run.file.display().to_string()]),
        "the file is named as unread"
    );
    assert!(
        log_entries::waiting_values(m22, &identity).contains_key("late"),
        "the waiting write stays"
    );
    log_entries::forget_waiting(m22, &[3; 32]);
    assert!(
        !run.file.exists(),
        "every server's refusal of its version drops it by its name"
    );
    run.macs.server.stop();

    let control = unread_set(true);
    assert!(
        control.keeps.is_ok() && !control.there,
        "CONTROL: a set read as none is dropped unseen"
    );
    assert!(
        matches!(control.signed, Ok(1)),
        "CONTROL: the pass signs over the set it could not read: {:?}",
        control.signed
    );
    control.macs.server.stop();
}

/// One-mesh W3: an admin's draft over waiting writes this Mac cannot read (`everyone.waiting`
/// does not open) refuses an edit that writes `[everyone]`, since any write it makes may be over
/// one of them; an edit that writes nothing there goes, and the file is named as unread and stays.
///
/// NEGATIVE CONTROL: [`log_entries::UNREAD_AS_NONE`] (the waiting writes read as none, as before
/// W3), run in the test: the `[everyone]` edit is published, as the CONTROL assert says; with the
/// switch set for the first run too, the test fails "an [everyone] edit over waiting writes that
/// do not read refuses".
#[test]
fn a_draft_over_waiting_writes_that_do_not_read_refuses_an_everyone_edit() {
    for control in [false, true] {
        let macs = two_macs(&[(Audience::Everyone, "shared", b"v1".as_slice())]);
        let m22 = &macs.m22;
        let identity = read_identity(m22).unwrap().unwrap();
        fsx::write_atomic(&log_entries::waiting_file(m22), b"not sealed", 0o600).unwrap();
        let admin = admin_open(m22).expect("an admin opens over it");
        let mut doc = admin.doc.clone();
        doc.set_entry(&Audience::Everyone, "shared", b"v2")
            .expect("edit");
        log_entries::UNREAD_AS_NONE.with(|cell| cell.set(control));
        let everyone = admin_publish(m22, &admin, &doc);
        log_entries::UNREAD_AS_NONE.with(|cell| cell.set(false));
        if control {
            assert!(
                everyone.is_ok(),
                "CONTROL: read as none, the edit is published: {:?}",
                everyone.err()
            );
            macs.server.stop();
            continue;
        }
        assert!(
            everyone
                .as_ref()
                .is_err_and(|error| error.to_string().contains("no [everyone] edit is drafted")),
            "an [everyone] edit over waiting writes that do not read refuses: {:?}",
            everyone.err()
        );
        assert_eq!(
            log_entries::unread_waiting(m22, &identity),
            (
                Some(log_entries::waiting_file(m22).display().to_string()),
                Vec::new()
            ),
            "the file is named as unread"
        );
        let admin = admin_open(m22).expect("an admin");
        let mut doc = admin.doc.clone();
        doc.set_entry(&Audience::Hosts(vec!["m7".to_owned()]), "only7", b"h")
            .expect("edit");
        admin_publish(m22, &admin, &doc).expect("an edit that writes nothing there goes");
        assert!(
            log_entries::waiting_file(m22).exists(),
            "the waiting writes stay"
        );
        macs.server.stop();
    }
}

/// One-mesh W3: a cached admin open, made under an author's own observation of the state (a
/// removal's `vault_admin_named`, an enrollment's pre-F15 fallback), lands no pending set and so
/// never takes the state lock: the open goes, and the set stays for a later open or sync.
///
/// NEGATIVE CONTROL: `admin_cached_unlocked` passing `land` to `admin_base` (restored with cp from
/// a backup plus touch, confirmed with cmp): in this build with debug assertions the open panics
/// "an exclusive take of … under this thread's own draft observation of it waits for good".
#[test]
fn a_cached_admin_open_under_an_authors_observation_lands_no_pending_set() {
    let macs = two_macs(&[(Audience::Everyone, "shared", b"v1".as_slice())]);
    let m22 = &macs.m22;
    let identity = read_identity(m22).unwrap().unwrap();
    let admin = admin_open(m22).expect("an admin");
    log_entries::wait_for_key(
        m22,
        &identity,
        &log_entries::Waiting::of_puts(&[("late", b"2")]),
        admin.seq,
        &admin.opened_heads[0],
        &["https://one.example".to_owned()],
        &[],
        None,
    )
    .expect("pending");
    assert!(
        log_entries::waiting_pending(m22),
        "CONTROL: a set is pending"
    );
    let (_, _, opened) = log_entries::author_and_commit_records(m22, |_store| {
        Ok((Vec::new(), enrollment::admin_cached_unlocked(m22).is_ok()))
    })
    .expect("the author");
    assert!(opened, "the cached open goes under the observation");
    assert!(log_entries::waiting_pending(m22), "the set stays");
    macs.server.stop();
}

/// One-mesh W3: a publish whose deadline ends before its first PUT drops the set it kept, so a
/// set whose servers are the ALab name never stays for a version no server was sent; and a
/// deadline that ended waits on nothing more: while another holds the state lock the set stays.
///
/// NEGATIVE CONTROL: `forget_unsent` with its body cut to `return` (restored with cp from a
/// backup plus touch, confirmed with cmp): the test fails "an unsent version's set goes".
#[test]
fn an_unsent_versions_pending_set_goes_where_the_lock_is_free() {
    let macs = two_macs(&[]);
    let m22 = &macs.m22;
    let identity = read_identity(m22).unwrap().unwrap();
    let keep = |version: u8| {
        log_entries::wait_for_key(
            m22,
            &identity,
            &log_entries::Waiting::of_puts(&[("late", b"2")]),
            u64::from(version),
            &[version; 32],
            &[crate::ALAB_DEFAULT_URL.to_owned()],
            &[],
            None,
        )
        .expect("pending");
    };
    keep(1);
    let busy = StateLock::take(m22).expect("the state lock");
    let lock_held = std::thread::scope(|scope| {
        scope
            .spawn(|| {
                log_entries::forget_unsent(m22, &[1; 32]);
                log_entries::waiting_pending(m22)
            })
            .join()
            .expect("the thread")
    });
    drop(busy);
    assert!(
        lock_held,
        "while another holds the state lock the set stays"
    );
    log_entries::forget_unsent(m22, &[1; 32]);
    assert!(
        !log_entries::waiting_pending(m22),
        "an unsent version's set goes"
    );
    macs.server.stop();
}

/// The pending sets that twelve publishes leave, each `[everyone]` write on a version no answer
/// settled, with A3's count bound where `by_count` ([`log_entries::PENDING_BY_COUNT`]).
fn twelve_pending(by_count: bool) -> (TwoMacs, Identity, usize) {
    let macs = two_macs(&[]);
    let identity = read_identity(&macs.m22).unwrap().unwrap();
    log_entries::PENDING_BY_COUNT.with(|cell| cell.set(by_count));
    for version in 1..=12u8 {
        let name = format!("n{version}");
        log_entries::wait_for_key(
            &macs.m22,
            &identity,
            &log_entries::Waiting::of_puts(&[(name.as_str(), &[version])]),
            u64::from(version),
            &[version; 32],
            &["https://a.example".to_owned()],
            &[],
            None,
        )
        .expect("pending");
    }
    log_entries::PENDING_BY_COUNT.with(|cell| cell.set(false));
    let kept = log_entries::waiting_and_pending(&macs.m22, &identity)
        .1
        .len();
    (macs, identity, kept)
}

/// One-mesh W3: no pending set goes by count. A set goes only on proof, as a held draft does (W2):
/// it lands once a server is seen to hold its version or a descendant; a landed publish of
/// sequence `seq` takes from each set of sequence `seq` or below the servers that took it, which
/// refuse that set's version as not newer for good, and drops a set left with none. A set of a
/// higher sequence stays, and so do one whose servers are unknown and one that names the ALab
/// name, which moves between the Macs that serve it; a set the landed version names as a parent
/// lands.
///
/// NEGATIVE CONTROL: [`log_entries::PENDING_BY_COUNT`] (A3's bound of eight, the oldest dropped
/// whatever its version), run in the test: the four oldest of twelve go, and the CONTROL assert
/// says so; with the switch set for the first run too, the test fails "every set whose version
/// may be stored stays".
#[test]
fn no_pending_set_goes_by_count_and_a_landed_publish_settles_them_by_its_servers() {
    let (macs, identity, kept) = twelve_pending(false);
    assert_eq!(kept, 12, "every set whose version may be stored stays");
    let m22 = &macs.m22;
    let a = "https://a.example".to_owned();
    let b = "https://b.example".to_owned();
    let alab = crate::ALAB_DEFAULT_URL.to_owned();
    let wait = |name: &str, version: u8, seq: u64, reach: &[String]| {
        log_entries::wait_for_key(
            m22,
            &identity,
            &log_entries::Waiting::of_puts(&[(name, b"x")]),
            seq,
            &[version; 32],
            reach,
            &[],
            None,
        )
        .expect("pending");
    };
    wait("only-a", 20, 5, std::slice::from_ref(&a));
    wait("a-and-b", 21, 5, &[a.clone(), b.clone()]);
    wait("higher", 22, 7, std::slice::from_ref(&a));
    wait("unknown", 23, 5, &["*".to_owned()]);
    wait("alab", 24, 5, std::slice::from_ref(&alab));
    wait("parent", 25, 4, std::slice::from_ref(&b));

    log_entries::land_published(
        m22,
        &identity,
        6,
        &[30; 32],
        &[[25; 32]],
        std::slice::from_ref(&a),
        None,
    );
    let reach: BTreeMap<[u8; 32], Vec<String>> = log_entries::pending_reach(m22, &identity)
        .into_iter()
        .collect();
    let (waiting, _) = log_entries::waiting_and_pending(m22, &identity);
    assert!(
        waiting.contains_key("parent"),
        "the set the landed version names as a parent lands"
    );
    for version in 1..=6u8 {
        assert!(
            !reach.contains_key(&[version; 32]),
            "a set of sequence {version} that reached only the server that took sequence 6 goes"
        );
    }
    for version in 7..=12u8 {
        assert!(
            reach.contains_key(&[version; 32]),
            "a set of a higher sequence, {version}, stays"
        );
    }
    assert!(!reach.contains_key(&[20; 32]), "only-a goes");
    assert_eq!(
        reach.get(&[21; 32]),
        Some(&vec![b.clone()]),
        "a-and-b stays on the server that may still hold it"
    );
    assert!(reach.contains_key(&[22; 32]), "higher stays");
    assert!(reach.contains_key(&[23; 32]), "unknown stays");
    assert!(
        reach.contains_key(&[24; 32]),
        "the ALab name settles nothing"
    );
    assert!(!reach.contains_key(&[25; 32]), "parent landed");
    macs.server.stop();

    let (control, _, kept) = twelve_pending(true);
    assert_eq!(kept, 8, "CONTROL: A3's bound drops the four oldest");
    control.server.stop();
}
