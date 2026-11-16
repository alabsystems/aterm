// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The records heads digest a vault server advertises on its own name (`rec=`,
//! [`VaultTxt::rec`]): how a TXT carries it, which advert names it, the round its growth rings,
//! and how often its change alone starts a registration again ([`REC_REREGISTER_GAP`]).

use super::tests::{envelope_with, options, record_entrant, run};
use super::*;
use crate::membership_log::{Body, Log, Record, Store, encode_records};

const OLD: [u8; 32] = [7u8; 32];

fn txt(rec: Option<[u8; 32]>) -> VaultTxt {
    VaultTxt {
        vault: "alab".to_owned(),
        signer: "0123-4567-89ab-cdef-0123".to_owned(),
        seq: 9,
        head: [3; 32],
        label: Some("m5".to_owned()),
        rec,
    }
}

fn strings(txt: &VaultTxt) -> Vec<String> {
    std::iter::once("path=/v1".to_owned())
        .chain(txt.txt_pairs())
        .collect()
}

/// A TXT names `rec` as 64 lowercase hex digits and reads it back; an older server's TXT, with
/// no `rec`, reads with none and the rest intact, and so does one whose `rec` is not 64
/// lowercase hex digits.
///
/// NEGATIVE CONTROL (2026-11-15): with `txt_pairs` writing no `rec` pair, the round trip reads
/// `rec: None` where `Some([0xab; 32])` was written.
#[test]
fn a_txt_carries_its_records_digest_and_an_older_one_reads_none() {
    let with = txt(Some([0xab; 32]));
    let written = strings(&with);
    assert!(
        written.contains(&format!("rec={}", "ab".repeat(32))),
        "{written:?}"
    );
    assert!(written.iter().all(|pair| pair.len() <= 255));
    assert_eq!(
        VaultTxt::from_txt(written.iter().map(String::as_str)),
        Some(with.clone())
    );
    let older: Vec<&str> = written
        .iter()
        .map(String::as_str)
        .filter(|pair| !pair.starts_with("rec="))
        .collect();
    assert_eq!(VaultTxt::from_txt(older.iter().copied()), Some(txt(None)));
    for bad in ["AB".repeat(32), "ab".repeat(31), "zz".repeat(32)] {
        let pairs: Vec<String> = older
            .iter()
            .map(|pair| (*pair).to_owned())
            .chain([format!("rec={bad}")])
            .collect();
        assert_eq!(
            VaultTxt::from_txt(pairs.iter().map(String::as_str)),
            Some(txt(None)),
            "rec={bad}"
        );
    }
}

/// A founder's genesis and its keys: the log, its writer and incarnation, and its member.
fn founded() -> (Log, [u8; 32], [u8; 16], crate::membership_log::MemberId) {
    let (writer, incarnation, founder, entrant) = record_entrant(1, "m1");
    let genesis = Record::found(&[7; 32], vec![entrant], 1_000).expect("genesis");
    (
        Log::new(genesis).expect("log"),
        writer,
        incarnation,
        founder,
    )
}

/// The founder's next record in `log`, held there.
fn grow(
    log: &mut Log,
    writer: &[u8; 32],
    incarnation: [u8; 16],
    founder: &crate::membership_log::MemberId,
) -> Record {
    let record = log
        .draft(
            founder,
            2_000 + u64::try_from(log.held_count()).expect("small"),
            Body::Name {
                name: format!("n{}", log.held_count()),
            },
        )
        .sign(writer, incarnation)
        .expect("signed");
    log.insert(record.clone()).expect("held");
    record
}

/// The server's own name advertises the digest of the records it holds of the vault its TXT
/// names, the empty log's while it holds no genesis (no records slot, or one with none), so a
/// TXT with no `rec` is an older build's; `alab-secrets` never names one.
///
/// NEGATIVE CONTROLS (2026-11-15): with `advertised_vault` giving `alab-secrets` the digest too,
/// its TXT carries `rec` (`left: Some([..])`, `right: None`); with the own name naming no `rec`
/// where no genesis is held (step 6's `heads_digest`), "no records slot" reads `Some(None)`.
#[test]
fn the_own_name_advertises_its_records_digest_and_alab_secrets_does_not() {
    let raw = envelope_with(&OLD, 5, Vec::new(), "team");
    let parsed = crate::envelope::parse(&raw).expect("envelope");
    let mut state = State::default();
    state.vaults.insert(
        "team".to_owned(),
        Slot::new(Held {
            seq: 5,
            signer: parsed.signer,
            head: parsed.digest(),
            parented: false,
            handoff: Vec::new(),
            bytes: Arc::new(raw),
            alab: false,
        }),
    );
    let rec = |state: &State, advertised: &str| {
        advertised_vault(state, advertised, Some("m22"), &[]).map(|txt| txt.rec)
    };
    let empty = crate::membership_log::empty_heads_digest();
    assert_eq!(rec(&state, "m22"), Some(Some(empty)), "no records slot");
    state.records.insert(
        "team".to_owned(),
        RecordSlot::new(Store::for_vault("team").expect("store")),
    );
    assert_eq!(rec(&state, "m22"), Some(Some(empty)), "no genesis");
    assert_eq!(rec(&state, crate::alab_advertised()), Some(None));

    let (mut log, writer, incarnation, founder) = founded();
    grow(&mut log, &writer, incarnation, &founder);
    let mut store = Store::for_vault("team").expect("store");
    store.insert_many(log.records().cloned());
    let digest = store.heads_digest().expect("rooted");
    assert_eq!(digest, log.heads_digest());
    assert_ne!(digest, empty);
    state
        .records
        .insert("team".to_owned(), RecordSlot::new(store));
    assert_eq!(rec(&state, "m22"), Some(Some(digest)));
    assert_eq!(rec(&state, crate::alab_advertised()), Some(None));
}

/// A records POST that grows the held log rings the advert's bell once, and so does a pull
/// whose records grow it, so the next round's TXT names the new digest.
///
/// NEGATIVE CONTROLS: a POST of records the server already holds, and one of a record that only
/// waits on a parent it lacks, ring nothing (the heads did not move). With the ring taken out
/// of `apply_records` (2026-11-15), the growing POST leaves the bell silent
/// (`left: None`, `right: Some(Put)`).
#[test]
fn records_that_grow_the_log_ring_the_advertiser_and_others_do_not() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let running = run(&options(root.path(), Duration::from_millis(200)));
    let bell = Arc::new(Bell::default());
    *running
        .shared
        .advertiser
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = Some(Arc::clone(&bell));
    let url = format!("http://{}/v1/team", running.addr);

    let (mut log, writer, incarnation, founder) = founded();
    crate::transport::post_records(&url, &encode_records([log.genesis()])).expect("post genesis");
    assert_eq!(bell.clear(), Some(Wake::Put), "a genesis roots the log");
    assert_eq!(bell.clear(), None, "rung once");

    let first = grow(&mut log, &writer, incarnation, &founder);
    crate::transport::post_records(&url, &encode_records([&first])).expect("post one");
    assert_eq!(bell.clear(), Some(Wake::Put), "one more record");
    assert_eq!(bell.clear(), None, "rung once");

    crate::transport::post_records(&url, &encode_records([&first])).expect("post again");
    assert_eq!(bell.clear(), None, "a POST that adds nothing rings nothing");

    let _skipped = grow(&mut log, &writer, incarnation, &founder);
    let waiting = grow(&mut log, &writer, incarnation, &founder);
    crate::transport::post_records(&url, &encode_records([&waiting])).expect("post waiting");
    assert_eq!(
        bell.clear(),
        None,
        "a record waiting on a parent rings nothing"
    );

    let peer_root = aterm_tempfile::tempdir().expect("tempdir");
    let peer = run(&options(peer_root.path(), Duration::from_millis(200)));
    let peer_url = format!("http://{}/v1/team", peer.addr);
    crate::transport::post_records(&peer_url, &encode_records(log.records()))
        .expect("post the peer the whole log");
    let pulled = running.pull_from_peers(&[crate::discover::DiscoveredVault {
        url: peer_url,
        txt: None,
    }]);
    assert_eq!(pulled, 1, "the pull grew the log");
    assert_eq!(
        bell.clear(),
        Some(Wake::Put),
        "a pull that grows the log rings"
    );
    assert_eq!(bell.clear(), None, "rung once");
    peer.stop();
    running.stop();
}

/// A change of the own name's TXT in `rec` alone starts its registration again only once
/// [`REC_REREGISTER_GAP`] passed since a round last started one; a new `seq` starts it at once.
///
/// NEGATIVE CONTROL (2026-11-15): with `reregister_slot` keeping no registration whose TXT
/// differs (the gap ignored), the `rec` change within the gap starts another child.
#[test]
fn a_change_of_rec_alone_registers_again_only_past_the_gap() {
    let sleep = aterm_uds::deadline::stand_in(Path::new("/bin/sleep"));
    let sleep = sleep.to_str().expect("a UTF-8 path");
    let spawn = |_: &Target, _: Option<&VaultTxt>| quiet_advert(sleep, &["60".to_owned()]);
    let children = Children::default();
    let mut set = children.lock();
    let here = vec![Target {
        scope: None,
        ip: Ipv4Addr::new(10, 0, 0, 2),
    }];
    let pid = |set: &ChildSet| {
        set.dns
            .held
            .iter()
            .map(|(_, _, child)| child.id())
            .collect::<Vec<_>>()
    };
    let t0 = Instant::now();
    let round = |set: &mut ChildSet, want: VaultTxt, now: Instant| {
        reregister_slot(
            &mut set.dns,
            "m5",
            Some((here.clone(), Some(want))),
            Some(Wake::Put),
            now,
            &spawn,
        );
    };
    round(&mut set, txt(Some([1; 32])), t0);
    let first = pid(&set);
    assert_eq!(first.len(), 1);

    let within = t0 + REC_REREGISTER_GAP - Duration::from_millis(1);
    round(&mut set, txt(Some([2; 32])), within);
    assert_eq!(pid(&set), first, "rec alone, within the gap: kept");
    assert_eq!(set.dns.held[0].1, Some(txt(Some([1; 32]))));

    round(&mut set, txt(Some([2; 32])), t0 + REC_REREGISTER_GAP);
    let second = pid(&set);
    assert_ne!(second, first, "rec alone, past the gap: registered again");
    assert_eq!(set.dns.held[0].1, Some(txt(Some([2; 32]))));

    let mut newer = txt(Some([2; 32]));
    newer.seq += 1;
    round(&mut set, newer.clone(), t0 + REC_REREGISTER_GAP);
    assert_ne!(pid(&set), second, "a new seq registers at once");
    assert_eq!(set.dns.held[0].1, Some(newer));
    for (_, _, child) in &mut set.dns.held {
        child.end();
    }
}
