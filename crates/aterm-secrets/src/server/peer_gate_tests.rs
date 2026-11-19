// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The gate on a vault-tend round's pulls ([`pull_due`], [`pull_pass`]): what a peer's TXT makes
//! due, the records digests answering servers prove, the pacing of each group, and the sweeps
//! that pull each server whatever its TXT says.

use super::tests::{envelope_with, options, record_entrant};
use super::*;
use crate::envelope::signer_public;
use crate::membership_log::{Body, Log, Record, encode_records};

const HANG_DETECTOR: Duration = Duration::from_secs(60);
const OLD: [u8; 32] = [7u8; 32];
const NEW: [u8; 32] = [8u8; 32];

fn txt(seq: u64, rec: Option<[u8; 32]>) -> VaultTxt {
    VaultTxt {
        vault: "team".to_owned(),
        signer: crate::envelope::fingerprint(&signer_public(&OLD).expect("public")),
        seq,
        head: [0; 32],
        label: Some("m9".to_owned()),
        rec,
    }
}

/// What a server taking every part of `team` holds of it: its records at the digest `rec`.
fn holding(rec: [u8; 32]) -> Own {
    Own {
        seq: Some(5),
        rec,
        takes_envelope: true,
        takes_records: true,
        follows: [0; 32],
    }
}

/// The instance at `host` as a browse hands it to a pass, with `txt`: what [`pull_due`] judges.
fn named(host: &str, txt: Option<&VaultTxt>) -> crate::discover::DiscoveredVault {
    crate::discover::DiscoveredVault {
        url: format!("http://{host}:7484/v1/team"),
        txt: txt.cloned(),
    }
}

/// What a gated pass pulls of a vault a TXT names: its envelope where the TXT names a higher seq
/// than the one held (or none is held and the vault is taken), its records where the TXT names a
/// `rec` other than the one held and every proven one, and at every visit where it names no
/// `rec` (an older build's TXT, which says nothing of its records), whether or not this server
/// takes the vault's envelopes; nothing for a peer with no TXT, nor of a vault this server takes
/// nothing of.
///
/// NEGATIVE CONTROL (2026-11-15): with `pull_due`'s rec-less arm back to step 6's
/// `None => envelope`, the rec-less TXT naming the held version pulls nothing
/// (`left: Due { envelope: false, records: false }`, "rec-less, equal").
#[test]
fn a_txt_makes_due_only_what_it_names_past_what_is_held() {
    let own = Own {
        seq: Some(5),
        rec: [1; 32],
        takes_envelope: true,
        takes_records: true,
        follows: [0; 32],
    };
    let proven = |rec: &[u8; 32]| *rec == [2; 32];
    let due =
        |txt: Option<&VaultTxt>, own: &Own| pull_due(&named("m9.local", txt), false, own, proven);
    let (envelope, records) = (
        Due {
            envelope: true,
            records: false,
        },
        Due {
            envelope: false,
            records: true,
        },
    );
    assert_eq!(due(Some(&txt(5, Some([1; 32]))), &own), Due::NONE, "equal");
    assert_eq!(due(Some(&txt(4, Some([1; 32]))), &own), Due::NONE, "older");
    assert_eq!(due(Some(&txt(6, Some([1; 32]))), &own), envelope, "newer");
    assert_eq!(
        due(Some(&txt(5, Some([3; 32]))), &own),
        records,
        "other rec"
    );
    assert_eq!(
        due(Some(&txt(5, Some([2; 32]))), &own),
        Due::NONE,
        "proven rec"
    );
    assert_eq!(due(Some(&txt(6, Some([3; 32]))), &own), Due::BOTH);
    assert_eq!(due(Some(&txt(5, None)), &own), records, "rec-less, equal");
    assert_eq!(due(Some(&txt(6, None)), &own), Due::BOTH, "rec-less, newer");
    assert_eq!(due(None, &own), Due::NONE, "no TXT");
    let none_held = crate::membership_log::empty_heads_digest();
    let empty = Own {
        seq: None,
        rec: none_held,
        takes_envelope: true,
        takes_records: true,
        follows: [0; 32],
    };
    assert_eq!(
        due(Some(&txt(1, Some([1; 32]))), &empty),
        Due::BOTH,
        "holds none"
    );
    assert_eq!(
        due(Some(&txt(1, Some(none_held))), &empty),
        envelope,
        "a peer holding no records either"
    );
    // Records of a vault this server holds no envelope of and pins no key for.
    let records_only = Own {
        seq: None,
        rec: [1; 32],
        takes_envelope: false,
        takes_records: true,
        follows: [0; 32],
    };
    assert_eq!(due(Some(&txt(9, None)), &records_only), records);
    assert_eq!(due(Some(&txt(9, Some([1; 32]))), &records_only), Due::NONE);
    assert_eq!(due(Some(&txt(9, Some([3; 32]))), &records_only), records);
    let stranger = Own {
        seq: None,
        rec: none_held,
        takes_envelope: false,
        takes_records: false,
        follows: [0; 32],
    };
    assert_eq!(due(Some(&txt(9, Some([9; 32]))), &stranger), Due::NONE);
    assert_eq!(due(Some(&txt(9, None)), &stranger), Due::NONE);
}

/// A TXT naming no `rec` is excused from records only on an `alab-secrets` instance where the
/// instance its label names, resolving to its endpoints, named a `rec` for the vault that this
/// server held since or proved there (`named_elsewhere`), whatever label `pull_due` is handed with
/// it: which instance that is, that a TXT naming no label never has one, and which `rec` it must
/// have named, is how a round finds `named_elsewhere` ([`PullGate::names_rec`], bound by
/// `a_named_rec_stands_for_its_label_at_exactly_the_endpoints_it_resolved_to_until_a_catch_up`,
/// `another_servers_rec_at_the_same_endpoints_excuses_no_alab_secrets_records`,
/// `a_noted_rec_stands_only_where_this_server_holds_it_or_proved_it_there`,
/// `a_noted_rec_this_server_held_stands_after_it_takes_newer_records`,
/// `a_noted_rec_a_round_did_not_take_keeps_the_alab_secrets_records_due` and
/// `a_noted_rec_this_server_held_excuses_the_alab_secrets_records_after_newer_ones`). On
/// `alab-secrets` with none beside it, labelled or not, and on any other instance, beside one
/// or not, its records are due at every visit. A TXT naming a `rec` is judged by it wherever it
/// is.
///
/// NEGATIVE CONTROLS (2026-11-15): with the label taken as the proof
/// (`None => txt.label.is_none() || !PullGate::alab_instance(&peer.url)`), "alab-secrets alone,
/// label Some("m9")" fails; with no exception (`None => true`), "alab-secrets beside a rec,
/// label Some("m9")"; with no look at the instance (`None => !named_elsewhere`), "own-label,
/// beside true, label Some("m9")"; with the label required beside the group's `rec`
/// (`None => txt.label.is_none() || !(named_elsewhere && …)`), "alab-secrets beside a rec,
/// label None".
#[test]
fn a_rec_less_txt_is_excused_only_on_alab_secrets_beside_a_named_rec() {
    let own = Own {
        seq: Some(5),
        rec: [1; 32],
        takes_envelope: true,
        takes_records: true,
        follows: [0; 32],
    };
    let records = |host: &str, label: Option<&str>, rec: Option<[u8; 32]>, beside: bool| {
        let advertised = VaultTxt {
            label: label.map(str::to_owned),
            ..txt(5, rec)
        };
        pull_due(&named(host, Some(&advertised)), beside, &own, |_| false).records
    };
    for label in [Some("m9"), None] {
        assert!(
            !records("alab-secrets.local", label, None, true),
            "alab-secrets beside a rec, label {label:?}"
        );
        assert!(
            records("alab-secrets.local", label, None, false),
            "alab-secrets alone, label {label:?}"
        );
        for beside in [false, true] {
            assert!(
                records("m9.local", label, None, beside),
                "own-label, beside {beside}, label {label:?}"
            );
            assert!(
                !records("alab-secrets.local", label, Some([1; 32]), beside),
                "alab-secrets naming the held rec, beside {beside}"
            );
        }
    }
}

/// A server on loopback pinning vault `team` to [`OLD`], holding its envelope at `seq`, with
/// `resolve` its peer host resolver, the hang detector its pull budget.
fn serving(root: &Path, seq: u64, resolve: fn(&str, Duration) -> Option<Vec<IpAddr>>) -> Running {
    let mut opts = options(root, Duration::from_millis(200));
    opts.pins = vec![("team".to_owned(), signer_public(&OLD).expect("public"))];
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let running = launch(
        listener,
        &opts,
        Tuning {
            peer_pull: HANG_DETECTOR,
            resolve,
            ..Tuning::default()
        },
    )
    .expect("started");
    publish(&running, seq);
    running
}

/// The peer, as [`start_with_peer_pull`] starts one, holding `team` at `seq`.
fn peer(root: &Path, seq: u64) -> Running {
    peer_of(root, &OLD, seq)
}

/// [`peer`], with `team` pinned to and signed by `seed`.
fn peer_of(root: &Path, seed: &[u8; 32], seq: u64) -> Running {
    let mut opts = options(root, Duration::from_millis(200));
    opts.pins = vec![("team".to_owned(), signer_public(seed).expect("public"))];
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let running = start_with_peer_pull(listener, &opts, HANG_DETECTOR).expect("started");
    publish_bytes(&running, &envelope_with(seed, seq, Vec::new(), "team"));
    running
}

fn publish(running: &Running, seq: u64) {
    publish_bytes(running, &envelope_with(&OLD, seq, Vec::new(), "team"));
}

fn publish_bytes(running: &Running, envelope: &[u8]) {
    crate::transport::publish(
        &crate::transport::Source::Url(format!("http://{}/v1/team", running.addr)),
        envelope,
    )
    .expect("published");
}

/// The envelope of `team` at `seq` signed by [`OLD`], longer than a pull's first read
/// ([`ENVELOPE_PEEK`]).
fn long_envelope(seq: u64) -> Vec<u8> {
    use crate::envelope::{Locker, Unsigned, sign, test_armor};
    let unsigned = Unsigned {
        name: "team".to_owned(),
        seq,
        time: 1_792_530_000 + seq,
        handoff: Vec::new(),
        parents: Vec::new(),
        lockers: vec![Locker {
            label: "admin".to_owned(),
            armored: test_armor(&vec![7u8; ENVELOPE_PEEK]),
        }],
    };
    let bytes = sign(&unsigned, &OLD).expect("signed");
    assert!(bytes.len() > ENVELOPE_PEEK);
    bytes
}

fn post(running: &Running, records: &[&Record]) {
    crate::transport::post_records(
        &format!("http://{}/v1/team", running.addr),
        &encode_records(records.iter().copied()),
    )
    .expect("posted");
}

/// The founder's log of vault `team`, and how it grows ([`Founded::grow`]).
struct Founded {
    log: Log,
    writer: [u8; 32],
    incarnation: [u8; 16],
    founder: crate::membership_log::MemberId,
}

impl Founded {
    fn new() -> Self {
        let (writer, incarnation, founder, entrant) = record_entrant(1, "m1");
        let genesis = Record::found(&OLD, vec![entrant], 1_000).expect("genesis");
        Self {
            log: Log::new(genesis).expect("log"),
            writer,
            incarnation,
            founder,
        }
    }

    fn grow(&mut self) -> Record {
        let record = self
            .log
            .draft(
                &self.founder,
                2_000 + u64::try_from(self.log.held_count()).expect("small"),
                Body::Name {
                    name: format!("n{}", self.log.held_count()),
                },
            )
            .sign(&self.writer, self.incarnation)
            .expect("signed");
        self.log.insert(record.clone()).expect("held");
        record
    }
}

fn digest(running: &Running) -> Option<[u8; 32]> {
    running.stored_records("team").heads_digest()
}

/// The peer `at` as a browse hands it to a round, with `txt`.
fn browsed(host: &str, at: &Running, txt: VaultTxt) -> crate::discover::DiscoveredVault {
    crate::discover::DiscoveredVault {
        url: format!("http://{host}:{}/v1/team", at.addr.port()),
        txt: Some(txt),
    }
}

/// What `peer` answered: its envelope and records GETs.
fn asked(peer: &Running) -> (usize, usize) {
    (peer.envelope_gets(), peer.records_gets())
}

/// `own`'s rounds swept each of `lists` at `at` ([`PullGate::swept`]), its next sweep due
/// `sweep_after(0)` later: until then a gated pass pulls those groups only as their TXT says.
fn swept(own: &Running, lists: &[&[SocketAddr]], at: Instant) {
    let mut gate = own.shared.lock_gate();
    for list in lists {
        gate.swept(list, at, 0);
    }
}

/// A gated round pulls a peer only for what its TXT names past what this server holds, on two
/// loopback servers: an equal TXT costs the peer no GET; a TXT a seq ahead costs it exactly one
/// envelope GET (and the version is stored); a TXT naming another `rec` exactly one records GET
/// (and the records are taken); this server's own `alab-secrets` instance gets no request.
///
/// NEGATIVE CONTROL (2026-11-15): with the gate forced open (`pull_due` returning
/// [`Due::BOTH`]), the equal TXT costs the peer an envelope GET and a records GET
/// (`left: (1, 1)`, `right: (0, 0)`).
#[test]
fn a_gated_round_pulls_a_peer_only_for_what_its_txt_names_past_what_is_held() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let own = serving(&root.path().join("own"), 5, |_, _| None);
    let peer = peer(&root.path().join("peer"), 5);
    let mut founded = Founded::new();
    let first = founded.grow();
    for server in [&own, &peer] {
        post(server, &[founded.log.genesis(), &first]);
    }
    let own_port = own.addr.port();
    let pass = |peers: &[crate::discover::DiscoveredVault], at: Instant| {
        pull_pass(
            &own.shared,
            peers,
            own_port,
            Some("m1"),
            &[],
            Pass::Gated(at),
        )
    };
    let t0 = Instant::now();
    swept(&own, &[&[peer.addr]], t0);
    let rec = digest(&peer);
    assert_eq!(rec, digest(&own));

    pass(&[browsed("127.0.0.1", &peer, txt(5, rec))], t0);
    assert_eq!(asked(&peer), (0, 0), "an equal TXT is pulled for nothing");

    publish(&peer, 6);
    pass(&[browsed("127.0.0.1", &peer, txt(6, rec))], t0);
    assert_eq!(asked(&peer), (1, 0), "a seq ahead: one envelope GET");
    assert_eq!(own.held_seq("team"), Some(6));

    let second = founded.grow();
    post(&peer, &[&second]);
    let rec = digest(&peer);
    assert_ne!(rec, digest(&own));
    pass(
        &[browsed("127.0.0.1", &peer, txt(6, rec))],
        t0 + PULL_SPACING,
    );
    assert_eq!(asked(&peer), (1, 1), "another rec: one records GET");
    assert_eq!(digest(&own), rec, "and its records are taken");

    pass(
        &[browsed("127.0.0.1", &own, txt(6, Some([0xee; 32])))],
        t0 + PULL_SPACING * 2,
    );
    assert_eq!(
        asked(&own),
        (0, 0),
        "this server's own instance gets no request"
    );
    own.stop();
    peer.stop();
}

/// A records pull this server judged whole proves the digest the server that answered stated
/// ([`crate::transport::RECORDS_HEADS_HEADER`]), never the one the TXT claimed: a TXT naming a
/// `rec` nobody holds is pulled again at the next round, while one naming the proven digest is
/// not, though this server's own digest differs from it (it refused the record behind it).
///
/// NEGATIVE CONTROL (2026-11-15): with the memo taking the TXT's `rec` as the gate makes it due
/// (the mapper's design) in place of the digest the answering server stated, the answering
/// server's digest is not proven ("the answering server's digest" fails).
#[test]
fn a_records_pull_proves_what_its_answering_server_states_not_what_the_txt_claims() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let own = serving(&root.path().join("own"), 5, |_, _| None);
    let peer = peer(&root.path().join("peer"), 5);
    let mut founded = Founded::new();
    let first = founded.grow();
    post(&own, &[founded.log.genesis()]);
    post(&peer, &[founded.log.genesis(), &first]);
    let own_port = own.addr.port();
    let pass = |rec, at| {
        pull_pass(
            &own.shared,
            &[browsed("127.0.0.1", &peer, txt(5, Some(rec)))],
            own_port,
            Some("m1"),
            &[],
            Pass::Gated(at),
        )
    };
    let t0 = Instant::now();
    swept(&own, &[&[peer.addr]], t0);
    let claimed = [0xc1; 32];
    let stated = digest(&peer).expect("rooted");
    pass(claimed, t0);
    assert_eq!(peer.records_gets(), 1);
    assert_eq!(digest(&own), Some(stated), "the records were taken");
    {
        let gate = own.shared.lock_gate();
        let follows = [0; 32];
        assert!(
            gate.proved("team", &stated, &[peer.addr], &follows),
            "the answering server's digest"
        );
        assert!(
            !gate.proved("team", &claimed, &[peer.addr], &follows),
            "never the TXT's"
        );
    }
    pass(claimed, t0 + PULL_SPACING);
    assert_eq!(peer.records_gets(), 2, "a claimed rec is pulled again");

    // A digest proven where this server's own differs: what it refused stays refused.
    own.shared.lock_gate().prove(Proof {
        vault: "team".to_owned(),
        digest: [0xd2; 32],
        at: peer.addr,
        follows: None,
    });
    pass([0xd2; 32], t0 + PULL_SPACING * 2);
    assert_eq!(peer.records_gets(), 2, "a proven rec is not pulled");
    own.stop();
    peer.stop();
}

/// A gated pass of this server over the peer `at`, whose TXT names the held version and `rec`.
fn records_pass(own: &Running, at: &Running, rec: Option<[u8; 32]>, when: Instant) {
    pull_pass(
        &own.shared,
        &[browsed("127.0.0.1", at, txt(5, rec))],
        own.addr.port(),
        Some("m1"),
        &[],
        Pass::Gated(when),
    );
}

/// An empty records answer (this server holds all the peer holds, and more) proves the digest
/// the answering server stated, for that server alone: the next round asks it nothing while its
/// TXT names that digest, though this server's own differs; another server stating the same
/// digest is still asked, since a statement binds only the endpoint that made it.
///
/// NEGATIVE CONTROLS (2026-11-15): with an empty answer proving nothing, the second round asks
/// the peer again (`left: 2`, `right: 1`); with a proof standing for every endpoint (step 6's
/// `(vault, digest)` key), the other server is not asked (`left: 0`, `right: 1`).
#[test]
fn an_empty_answer_proves_its_servers_digest_for_that_server_alone() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let own = serving(&root.path().join("own"), 5, |_, _| None);
    let behind = peer(&root.path().join("behind"), 5);
    let twin = peer(&root.path().join("twin"), 5);
    let mut founded = Founded::new();
    let first = founded.grow();
    post(&own, &[founded.log.genesis(), &first]);
    for server in [&behind, &twin] {
        post(server, &[founded.log.genesis()]);
    }
    let theirs = digest(&behind);
    let ours = digest(&own);
    assert_eq!(theirs, digest(&twin));
    assert_ne!(theirs, ours);
    let t0 = Instant::now();
    swept(&own, &[&[behind.addr], &[twin.addr]], t0);
    records_pass(&own, &behind, theirs, t0);
    assert_eq!(asked(&behind), (0, 1), "a rec this server does not hold");
    records_pass(&own, &behind, theirs, t0 + PULL_SPACING);
    assert_eq!(behind.records_gets(), 1, "proven at that server");
    records_pass(&own, &twin, theirs, t0 + PULL_SPACING);
    assert_eq!(twin.records_gets(), 1, "another server stating it");
    assert_eq!(digest(&own), ours);
    own.stop();
    behind.stop();
    twin.stop();
}

/// A records answer every record of which this server, holding a genesis, refuses (another
/// network's genesis: a `400` verdict) proves the digest its server stated for good: the next
/// round asks it nothing while its TXT names that digest.
///
/// NEGATIVE CONTROL (2026-11-15): with only a `204` verdict proving, the second round asks the
/// peer again (`left: 2`, `right: 1`).
#[test]
fn a_refused_answer_proves_its_servers_digest_where_the_store_holds_a_genesis() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let own = serving(&root.path().join("own"), 5, |_, _| None);
    let other = [0x66u8; 32];
    let foreign = peer_of(&root.path().join("foreign"), &other, 5);
    let founded = Founded::new();
    post(&own, &[founded.log.genesis()]);
    let (_, _, _, entrant) = record_entrant(2, "m2");
    let theirs = Record::found(&other, vec![entrant], 1_000).expect("genesis");
    post(&foreign, &[&theirs]);
    let ours = digest(&own);
    let t0 = Instant::now();
    swept(&own, &[&[foreign.addr]], t0);
    records_pass(&own, &foreign, digest(&foreign), t0);
    assert_eq!(foreign.records_gets(), 1);
    assert_eq!(digest(&own), ours, "another network's genesis is refused");
    records_pass(&own, &foreign, digest(&foreign), t0 + PULL_SPACING);
    assert_eq!(foreign.records_gets(), 1, "what it refused stays refused");
    own.stop();
    foreign.stop();
}

/// A proof made while this server's records store held no genesis stands only while the store
/// follows the same keys ([`Proof::follows`]): a genesis signed by a key it does not follow yet
/// is refused, and the peer asked nothing more, until this server holds a version whose handoff
/// names that key; then it is asked again and the genesis taken.
///
/// NEGATIVE CONTROLS (2026-11-15): with a proof standing whatever keys the store follows, the
/// round after the rotation asks nothing (`left: 1`, `right: 2`); with a proof made the same way
/// whatever the store held (step 6's), likewise.
#[test]
fn a_proof_made_without_a_genesis_lapses_once_the_store_follows_other_keys() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let own = serving(&root.path().join("own"), 5, |_, _| None);
    let rotated = peer(&root.path().join("rotated"), 5);
    let old = signer_public(&OLD).expect("public");
    let new = signer_public(&NEW).expect("public");
    let handoff = vec![(
        old,
        crate::envelope::endorse(&OLD, "team", &new).expect("endorsed"),
    )];
    let next = envelope_with(&NEW, 6, handoff, "team");
    publish_bytes(&rotated, &next);
    let (_, _, _, entrant) = record_entrant(2, "m2");
    let genesis = Record::found(&NEW, vec![entrant], 1_000).expect("genesis");
    post(&rotated, &[&genesis]);
    let theirs = digest(&rotated);
    assert!(theirs.is_some());
    let t0 = Instant::now();
    swept(&own, &[&[rotated.addr]], t0);
    records_pass(&own, &rotated, theirs, t0);
    assert_eq!(rotated.records_gets(), 1);
    assert_eq!(digest(&own), None, "a genesis of a key it does not follow");
    records_pass(&own, &rotated, theirs, t0 + PULL_SPACING);
    assert_eq!(
        rotated.records_gets(),
        1,
        "proven while it follows the same keys"
    );
    publish_bytes(&own, &next);
    records_pass(&own, &rotated, theirs, t0 + PULL_SPACING * 2);
    assert_eq!(
        rotated.records_gets(),
        2,
        "asked again once it follows the new key"
    );
    assert_eq!(digest(&own), theirs, "and the genesis taken");
    own.stop();
    rotated.stop();
}

/// A round pulls one group at most once per [`PULL_SPACING`], however many rounds ring within
/// it and whatever their TXT makes due; the first round past it pulls again.
///
/// NEGATIVE CONTROL (2026-11-15): with [`PullGate::pace`] always answering true, the round one
/// second later pulls the peer again (`left: 2`, `right: 1`).
#[test]
fn a_round_pulls_a_group_at_most_once_per_spacing() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let own = serving(&root.path().join("own"), 5, |_, _| None);
    let peer = peer(&root.path().join("peer"), 6);
    let own_port = own.addr.port();
    let pass = |seq, at| {
        pull_pass(
            &own.shared,
            &[browsed("127.0.0.1", &peer, txt(seq, None))],
            own_port,
            Some("m1"),
            &[],
            Pass::Gated(at),
        )
    };
    let t0 = Instant::now();
    swept(&own, &[&[peer.addr]], t0);
    pass(6, t0);
    assert_eq!(peer.envelope_gets(), 1);
    publish(&peer, 7);
    pass(7, t0 + Duration::from_secs(1));
    assert_eq!(peer.envelope_gets(), 1, "within the spacing: held back");
    pass(7, t0 + PULL_SPACING);
    assert_eq!(peer.envelope_gets(), 2, "past it: pulled");
    assert_eq!(own.held_seq("team"), Some(7));
    own.stop();
    peer.stop();
}

/// Pacing keys a group by its exact list of endpoints, as [`grouped`] does: a list sharing only
/// some endpoints with another is paced apart from it, and only the same list within
/// [`PULL_SPACING`] is held back.
///
/// NEGATIVE CONTROL (2026-11-15): with `pace` refusing a list any of whose endpoints a list
/// paced before it named (the earlier `any` check), the copied server's own list is held back
/// behind the forged one (`assertion failed: the copied server's own list`).
#[test]
fn pacing_keys_a_group_by_its_exact_endpoints() {
    let at = |n: u8| SocketAddr::from(([127, 0, 0, n], 7484));
    let mut gate = PullGate::default();
    let t0 = Instant::now();
    assert!(gate.pace(&[at(9), at(1)], t0), "the forged list");
    assert!(gate.pace(&[at(1)], t0), "the copied server's own list");
    assert!(
        !gate.pace(&[at(1)], t0 + Duration::from_secs(1)),
        "the same list within the spacing"
    );
    assert!(gate.pace(&[at(1)], t0 + PULL_SPACING), "past it");
}

/// The host of instance `evil.local`, a forged one, resolves to the loopback peer and then an
/// address of its own; every other host resolves to nothing.
fn forged_host(host: &str, _: Duration) -> Option<Vec<IpAddr>> {
    (host == "evil.local").then(|| {
        vec![
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            IpAddr::V4(Ipv4Addr::new(127, 0, 0, 2)),
        ]
    })
}

/// An instance whose host resolves to a server's address and one of its own, first in a gated
/// round, takes no pull from the server it copies: the forged group pulls the server for what
/// its own TXT makes due (its envelope), and the server's own group still pulls it for what
/// the server's TXT makes due (its records), though the forged pull reached the server first.
///
/// NEGATIVE CONTROL (2026-11-15): with `pace` refusing a list any of whose endpoints a list
/// paced before it named (the earlier `any` check), the server's own group is held back and its
/// records are never asked (`left: (1, 0)`, `right: (1, 1)`).
#[test]
fn a_forged_instance_sharing_an_endpoint_takes_no_pull_from_the_server_it_copies() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let own = serving(&root.path().join("own"), 5, forged_host);
    let peer = peer(&root.path().join("peer"), 6);
    let mut founded = Founded::new();
    let first = founded.grow();
    post(&own, &[founded.log.genesis()]);
    post(&peer, &[founded.log.genesis(), &first]);
    let (own_rec, peer_rec) = (digest(&own), digest(&peer));
    assert_ne!(own_rec, peer_rec);
    let forged_list = [
        peer.addr,
        SocketAddr::from(([127, 0, 0, 2], peer.addr.port())),
    ];
    swept(&own, &[&[peer.addr], &forged_list], Instant::now());
    pull_pass(
        &own.shared,
        &[
            // Forged: a newer version and this server's own records digest, both public.
            browsed("evil.local", &peer, txt(6, own_rec)),
            // The server itself: the version this server holds, and records it lacks.
            browsed("127.0.0.1", &peer, txt(5, peer_rec)),
        ],
        own.addr.port(),
        Some("m1"),
        &[],
        Pass::Gated(Instant::now()),
    );
    assert_eq!(asked(&peer), (1, 1));
    assert_eq!(own.held_seq("team"), Some(6));
    assert_eq!(digest(&own), peer_rec);
    own.stop();
    peer.stop();
}

/// Three peers whose TXT names the same version, longer than a pull's first read: each first
/// read is made while every envelope slot is held elsewhere, so none waits for a slot; then, one
/// slot at a time, the version is read whole once, and the pulls that get the slot after it was
/// stored ask nothing more: 3 first reads and 1 whole read in all.
///
/// NEGATIVE CONTROLS (2026-11-15): with an envelope pull a TXT made due taking its slot before
/// its first read (step 6's order), no first read is made while the slots are held ("timed out
/// after 60s waiting for: every peer's first read"); with no look at what is held once a pull
/// holds its slot, the two later pulls ask again (`left: 6`, `right: 4`).
#[test]
fn first_reads_hold_no_slot_and_a_version_stored_is_read_whole_once() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let own = serving(&root.path().join("own"), 5, |_, _| None);
    let peers: Vec<Running> = (0..3)
        .map(|n| peer(&root.path().join(format!("peer{n}")), 5))
        .collect();
    let long = long_envelope(6);
    for peer in &peers {
        publish_bytes(peer, &long);
    }
    let named: Vec<crate::discover::DiscoveredVault> = peers
        .iter()
        .map(|peer| browsed("127.0.0.1", peer, txt(6, digest(&own))))
        .collect();
    let lists: Vec<[SocketAddr; 1]> = peers.iter().map(|peer| [peer.addr]).collect();
    let lists: Vec<&[SocketAddr]> = lists.iter().map(|list| list.as_slice()).collect();
    swept(&own, &lists, Instant::now());
    let gets = || peers.iter().map(Running::envelope_gets).sum::<usize>();
    std::thread::scope(|scope| {
        let mut held = vec![
            own.shared.envelope_pulls.take(),
            own.shared.envelope_pulls.take(),
        ];
        let pass = scope.spawn(|| {
            pull_pass(
                &own.shared,
                &named,
                own.addr.port(),
                Some("m1"),
                &[],
                Pass::Gated(Instant::now()),
            )
        });
        aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "every peer's first read, with every envelope slot held",
            || (gets() == 3).then_some(()),
        );
        // One slot for the pass: its whole reads come one at a time.
        held.pop();
        assert_eq!(pass.join().expect("the pass"), 1, "one pull stored it");
        drop(held);
    });
    assert_eq!(gets(), 4, "3 first reads and 1 whole read");
    assert_eq!(own.held_seq("team"), Some(6));
    assert_eq!(own.shared.counts.get(Count::EnvelopeWholeReads), 1);
    own.stop();
    for peer in peers {
        peer.stop();
    }
}

/// A holder's `alab-secrets` instance is gated as any instance is where its server's own-label
/// instance, resolving to the same endpoints, names a `rec` for the vault: its TXT names that
/// label (`m9`, from [`txt`]) and no `rec`, by design, so the records are due as `m9`'s TXT
/// says; a newer version the `alab-secrets` TXT names is pulled alone, and a TXT naming what is
/// held costs it no request; this server's own instances (resolving to it) are not pulled. The
/// holder is swept as every server is ([`PullGate::sweep_due`]).
///
/// NEGATIVE CONTROLS (2026-11-15): with no `alab-secrets` exception in `pull_due`
/// (`None => true`), and with no instance taken as naming a `rec` (`PullGate::names_rec`
/// answering false), the equal TXT costs a records GET (`left: (0, 1)`, `right: (0, 0)`).
#[test]
fn an_alab_secrets_instance_is_gated_and_names_no_records() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let own = serving(&root.path().join("own"), 5, |_, _| {
        Some(vec![IpAddr::V4(Ipv4Addr::LOCALHOST)])
    });
    let holder = peer(&root.path().join("holder"), 5);
    let founded = Founded::new();
    for server in [&own, &holder] {
        post(server, &[founded.log.genesis()]);
    }
    let own_port = own.addr.port();
    let t0 = Instant::now();
    swept(&own, &[&[holder.addr]], t0);
    let pass = |at: &Running, seq, when| {
        pull_pass(
            &own.shared,
            &[
                browsed("alab-secrets.local", at, txt(seq, None)),
                // Its server's own-label instance: the version held, and the records it holds.
                browsed("m9.local", at, txt(5, digest(at))),
            ],
            own_port,
            Some("m1"),
            &[],
            Pass::Gated(when),
        );
    };
    assert_eq!(digest(&holder), digest(&own));
    pass(&holder, 5, t0);
    assert_eq!(asked(&holder), (0, 0), "a TXT naming what is held");
    publish(&holder, 6);
    pass(&holder, 6, t0 + PULL_SPACING);
    assert_eq!(asked(&holder), (1, 0), "a newer version, alone");
    assert_eq!(own.held_seq("team"), Some(6));
    pass(&own, 9, t0 + PULL_SPACING * 2);
    assert_eq!(asked(&own), (0, 0), "the instances this server holds");
    own.stop();
    holder.stop();
}

/// A holder's `alab-secrets` instance naming its server's label has its records pulled at every
/// round where the instance that label names, resolving to its endpoints, named no `rec` for its
/// vault, in it or a round within [`NAMED_FOR`] before it ([`PullGate::names_rec`]): beside its
/// own-label instance naming another vault (`family`, which this server takes nothing of; a
/// round's resolve leaves such an instance out, and a pass handed one notes it for that vault
/// alone), and in the next round, whose batch leaves that instance out, each round asks the
/// holder for its records and nothing else, and the second takes a record posted to it between
/// them, well before the holder's next sweep. A label shows no instance named the records.
///
/// NEGATIVE CONTROLS (2026-11-15): with the label taken as the proof
/// (`None => txt.label.is_none() || !PullGate::alab_instance(&peer.url)`), with
/// `PullGate::names_rec` reading a `rec` for any vault, and with every rec-less `alab-secrets`
/// TXT excused (`None => !PullGate::alab_instance(&peer.url)`), the first round asks the holder
/// nothing (`left: (0, 0)`, `right: (0, 1)`).
#[test]
fn a_labelled_alab_secrets_instance_with_no_rec_named_beside_it_is_pulled_every_round() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let own = serving(&root.path().join("own"), 5, |_, _| {
        Some(vec![IpAddr::V4(Ipv4Addr::LOCALHOST)])
    });
    let holder = peer(&root.path().join("holder"), 5);
    let mut founded = Founded::new();
    for server in [&own, &holder] {
        post(server, &[founded.log.genesis()]);
    }
    let own_port = own.addr.port();
    let t0 = Instant::now();
    swept(&own, &[&[holder.addr]], t0);
    let alab = browsed("alab-secrets.local", &holder, txt(5, None));
    // Its server's own-label instance, carrying the TXT of another vault it advertises there.
    let family = crate::discover::DiscoveredVault {
        url: format!("http://m9.local:{}/v1/family", holder.addr.port()),
        txt: Some(VaultTxt {
            vault: "family".to_owned(),
            ..txt(5, Some([0xfa; 32]))
        }),
    };
    let pass = |peers: &[crate::discover::DiscoveredVault], when| {
        pull_pass(
            &own.shared,
            peers,
            own_port,
            Some("m1"),
            &[],
            Pass::Gated(when),
        );
    };
    pass(&[alab.clone(), family], t0);
    assert_eq!(
        asked(&holder),
        (0, 1),
        "beside an own-label instance naming another vault: its records alone"
    );
    let first = founded.grow();
    post(&holder, &[&first]);
    assert_ne!(digest(&own), digest(&holder));
    pass(&[alab], t0 + PULL_SPACING);
    assert_eq!(
        asked(&holder),
        (0, 2),
        "with that instance left out of the round: asked again"
    );
    assert_eq!(digest(&own), digest(&holder), "and takes what was posted");
    own.stop();
    holder.stop();
}

/// A holder whose only registration is `alab-secrets` (its TXT names no label, and no other
/// instance resolves to its endpoints) has its records pulled at every visit, as an older
/// build's rec-less TXT has: browsed as `alab-secrets.local` alone, its TXT naming the version
/// held, each round past the spacing asks it for its records and nothing else, and takes a
/// record posted to it between two rounds, well before the holder's next sweep.
///
/// NEGATIVE CONTROL (2026-11-15): with `pull_due` making records due by none for every rec-less
/// `alab-secrets` TXT, whatever its group holds (`None => !PullGate::alab_instance(&peer.url)`,
/// the exception's first form), the first round asks the holder nothing (`left: (0, 0)`,
/// `right: (0, 1)`).
#[test]
fn an_alab_secrets_instance_naming_no_label_has_its_records_pulled_at_every_visit() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let own = serving(&root.path().join("own"), 5, |_, _| {
        Some(vec![IpAddr::V4(Ipv4Addr::LOCALHOST)])
    });
    let holder = peer(&root.path().join("holder"), 5);
    let mut founded = Founded::new();
    for server in [&own, &holder] {
        post(server, &[founded.log.genesis()]);
    }
    let own_port = own.addr.port();
    let t0 = Instant::now();
    swept(&own, &[&[holder.addr]], t0);
    let only = VaultTxt {
        label: None,
        ..txt(5, None)
    };
    let pass = |when| {
        pull_pass(
            &own.shared,
            &[browsed("alab-secrets.local", &holder, only.clone())],
            own_port,
            Some("m1"),
            &[],
            Pass::Gated(when),
        );
    };
    pass(t0);
    assert_eq!(
        asked(&holder),
        (0, 1),
        "the version held: its records alone"
    );
    let first = founded.grow();
    post(&holder, &[&first]);
    assert_ne!(digest(&own), digest(&holder));
    pass(t0 + PULL_SPACING);
    assert_eq!(asked(&holder), (0, 2), "the next round asks again");
    assert_eq!(digest(&own), digest(&holder), "and takes what was posted");
    own.stop();
    holder.stop();
}

/// Two `alab-secrets` instances resolving to the same endpoints (as two holders' do after a
/// heal, both at `alab-secrets.local:7484` and told apart by their TXT's label), neither TXT
/// naming a `rec` and nothing else resolving there: neither stands for the other's records
/// (neither names a `rec`, and neither is on the host the other's label names), so each round
/// past the spacing asks the server there for its records and nothing else.
///
/// NEGATIVE CONTROL (2026-11-15): with [`PullGate::note_named`] noting an instance whose TXT
/// names no `rec` (no `txt.rec.is_some()` filter) and [`PullGate::names_rec`] taking an
/// instance on any host (no look at the label), each is taken as naming the records beside the
/// other and the first round asks nothing (`left: (0, 0)`, `right: (0, 1)`); either alone
/// leaves this test passing, since both rules hold the case.
#[test]
fn two_alab_secrets_instances_naming_no_rec_have_their_records_pulled_every_round() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let own = serving(&root.path().join("own"), 5, |_, _| {
        Some(vec![IpAddr::V4(Ipv4Addr::LOCALHOST)])
    });
    let holder = peer(&root.path().join("holder"), 5);
    let founded = Founded::new();
    for server in [&own, &holder] {
        post(server, &[founded.log.genesis()]);
    }
    let own_port = own.addr.port();
    let t0 = Instant::now();
    swept(&own, &[&[holder.addr]], t0);
    let healed = |label: &str| {
        browsed(
            "alab-secrets.local",
            &holder,
            VaultTxt {
                label: Some(label.to_owned()),
                ..txt(5, None)
            },
        )
    };
    let pass = |when| {
        pull_pass(
            &own.shared,
            &[healed("m7"), healed("m9")],
            own_port,
            Some("m1"),
            &[],
            Pass::Gated(when),
        );
    };
    pass(t0);
    assert_eq!(asked(&holder), (0, 1), "two rec-less TXTs: the records");
    pass(t0 + PULL_SPACING);
    assert_eq!(asked(&holder), (0, 2), "and again the next round");
    own.stop();
    holder.stop();
}

/// Where the gate takes a `rec` as named for an `alab-secrets` TXT ([`PullGate::note_named`],
/// [`PullGate::names_rec`]): by the instance on the host its `label` names (`m9.local` for
/// `m9`, from [`txt`]) alone, for the vault that instance's TXT named, at exactly the endpoints
/// it resolved to, until a round resolves that instance at other endpoints or any catch-up
/// comes, an address change's as well as a wake's. Another instance noted at the same endpoints
/// (`m7.local`) stands for nothing, nor does any for a TXT naming no label.
///
/// NEGATIVE CONTROLS (2026-11-15): with `names_rec` taking a list that shares an endpoint
/// (`endpoints.iter().any(|at| named.endpoints.contains(at))`), "a list sharing an endpoint"
/// fails; with `names_rec` matching by endpoints and vault alone (no look at the host), "another
/// instance at the same endpoints" fails; with a TXT naming no label taken as naming any
/// (`txt.label.as_deref().is_none_or(…)` in place of the `let … else`), "a TXT naming no label"
/// fails; with `catch_up` forgetting the noted instances only after a wake (`if woke`), "a
/// catch-up for new addresses" fails.
#[test]
fn a_named_rec_stands_for_its_label_at_exactly_the_endpoints_it_resolved_to_until_a_catch_up() {
    let at = |n: u8| SocketAddr::from(([127, 0, 0, n], 7484));
    let endpoints =
        |n: u8| -> Vec<(SocketAddr, String)> { vec![(at(n), format!("http://{}/v1/team", at(n)))] };
    let m9 = named("m9.local", Some(&txt(5, Some(NEW))));
    let m7 = named("m7.local", Some(&txt(5, Some(NEW))));
    let alab = txt(5, None);
    let unlabelled = VaultTxt {
        label: None,
        ..txt(5, None)
    };
    let family = VaultTxt {
        vault: "family".to_owned(),
        ..txt(5, None)
    };
    let (one, two) = (endpoints(1), endpoints(2));
    let held = holding(NEW);
    let mut gate = PullGate::default();
    let t0 = Instant::now();
    gate.note_named([(&m7, one.as_slice())], t0);
    assert!(
        !gate.names_rec(&[at(1)], &alab, &held),
        "another instance at the same endpoints"
    );
    gate.note_named([(&m9, one.as_slice())], t0);
    assert!(gate.names_rec(&[at(1)], &alab, &held), "where it resolved");
    assert!(!gate.names_rec(&[at(1)], &family, &held), "another vault");
    assert!(
        !gate.names_rec(&[at(1)], &unlabelled, &held),
        "a TXT naming no label"
    );
    assert!(
        !gate.names_rec(&[at(1), at(2)], &alab, &held),
        "a list sharing an endpoint"
    );
    gate.note_named([(&m9, two.as_slice())], t0 + PULL_SPACING);
    assert!(
        !gate.names_rec(&[at(1)], &alab, &held),
        "once a round resolved it elsewhere"
    );
    assert!(
        gate.names_rec(&[at(2)], &alab, &held),
        "where it resolves now"
    );
    gate.catch_up(false);
    assert!(
        !gate.names_rec(&[at(2)], &alab, &held),
        "a catch-up for new addresses"
    );
    gate.note_named([(&m9, two.as_slice())], t0 + PULL_SPACING * 2);
    assert!(gate.names_rec(&[at(2)], &alab, &held), "noted again");
    gate.catch_up(true);
    assert!(!gate.names_rec(&[at(2)], &alab, &held), "after a wake");
}

/// What a round's resolve of an instance whose host resolved to no endpoint (a lookup that timed
/// out or failed) does to what an earlier round noted of it ([`PullGate::note_named`]): it
/// stands, as for an instance the round left out, while the TXT the round resolved still names
/// a `rec` for the vault, and goes once that TXT names none.
///
/// NEGATIVE CONTROLS (2026-11-15): with every instance the round resolved forgotten, whatever
/// its endpoints (`seen == instance` alone), "its host resolving to no endpoint" fails; with an
/// instance whose host resolved to none never forgotten (`seen == instance &&
/// !endpoints.is_empty()`), "its TXT naming no rec, at no endpoint" fails.
#[test]
fn an_instance_resolving_to_no_endpoint_leaves_what_was_noted_of_it_unless_it_names_no_rec() {
    let at = SocketAddr::from(([127, 0, 0, 1], 7484));
    let one = vec![(at, format!("http://{at}/v1/team"))];
    let none: Vec<(SocketAddr, String)> = Vec::new();
    let alab = txt(5, None);
    let held = holding(NEW);
    let mut gate = PullGate::default();
    let t0 = Instant::now();
    let naming = named("m9.local", Some(&txt(5, Some(NEW))));
    gate.note_named([(&naming, one.as_slice())], t0);
    gate.note_named([(&naming, none.as_slice())], t0 + PULL_SPACING);
    assert!(
        gate.names_rec(&[at], &alab, &held),
        "its host resolving to no endpoint"
    );
    let rec_less = named("m9.local", Some(&txt(5, None)));
    gate.note_named([(&rec_less, none.as_slice())], t0 + PULL_SPACING * 2);
    assert!(
        !gate.names_rec(&[at], &alab, &held),
        "its TXT naming no rec, at no endpoint"
    );
}

/// A `rec` noted of the instance an `alab-secrets` TXT's label names stands for its records
/// ([`PullGate::names_rec`]) only where this server holds it, or a records pull it finished at
/// one of the group's endpoints proved it, the store following the keys it followed then
/// ([`PullGate::proved`]): one it lacks, proven nowhere or only at another endpoint, stands for
/// nothing, so the `alab-secrets` TXT keeps the records due.
///
/// NEGATIVE CONTROLS (2026-11-16): with `names_rec` excusing whatever `rec` was noted (no look
/// at `named.held` or the proofs), "a rec this server lacks" fails; with it looking at
/// `named.held` alone (no proof), "proven there" fails.
#[test]
fn a_noted_rec_stands_only_where_this_server_holds_it_or_proved_it_there() {
    let at = |n: u8| SocketAddr::from(([127, 0, 0, n], 7484));
    let one = vec![(at(1), format!("http://{}/v1/team", at(1)))];
    let alab = txt(5, None);
    let m9 = named("m9.local", Some(&txt(5, Some(NEW))));
    let noted = || {
        let mut gate = PullGate::default();
        gate.note_named([(&m9, one.as_slice())], Instant::now());
        gate
    };
    assert!(noted().names_rec(&[at(1)], &alab, &holding(NEW)), "held");
    let mut gate = noted();
    let lacking = holding(OLD);
    assert!(
        !gate.names_rec(&[at(1)], &alab, &lacking),
        "a rec this server lacks"
    );
    let proof = |at: SocketAddr, follows: Option<[u8; 32]>| Proof {
        vault: "team".to_owned(),
        digest: NEW,
        at,
        follows,
    };
    gate.prove(proof(at(2), None));
    assert!(
        !gate.names_rec(&[at(1)], &alab, &lacking),
        "proven at another endpoint"
    );
    gate.prove(proof(at(1), Some([9; 32])));
    assert!(
        !gate.names_rec(&[at(1)], &alab, &lacking),
        "proven there while the store followed other keys"
    );
    gate.prove(proof(at(1), None));
    assert!(gate.names_rec(&[at(1)], &alab, &lacking), "proven there");
}

/// A `rec` noted of the instance an `alab-secrets` TXT's label names that a round found this
/// server holding ([`PullGate::note_held`], as [`PullGate::names_rec`] decides) stands for its
/// records after this server takes newer ones, since its records only grow; one no round found
/// it holding stands for nothing however far this server has moved, and a round holding the
/// same digest of another vault holds nothing of this one.
///
/// NEGATIVE CONTROLS (2026-11-16): with `names_rec` comparing the noted `rec` with this round's
/// `own.rec` alone (`named.rec == own.rec` in place of `named.held`, the rule of eeb4bedc5f9),
/// "held, then newer records taken" fails; with `note_held` latching every note of the vault
/// whatever `rec` it named, "a rec no round found held" fails; with it latching notes of any
/// vault, "another vault's records at that digest" fails.
#[test]
fn a_noted_rec_this_server_held_stands_after_it_takes_newer_records() {
    let at = SocketAddr::from(([127, 0, 0, 1], 7484));
    let one = vec![(at, format!("http://{at}/v1/team"))];
    let alab = txt(5, None);
    let family = VaultTxt {
        vault: "family".to_owned(),
        ..txt(5, None)
    };
    let m9 = named("m9.local", Some(&txt(5, Some(OLD))));
    let mut gate = PullGate::default();
    gate.note_named([(&m9, one.as_slice())], Instant::now());
    assert!(
        !gate.names_rec(&[at], &alab, &holding(NEW)),
        "a rec no round found held"
    );
    assert!(
        !gate.names_rec(&[at], &family, &holding(OLD)),
        "family, noted nowhere"
    );
    assert!(
        !gate.names_rec(&[at], &alab, &holding(NEW)),
        "another vault's records at that digest"
    );
    assert!(gate.names_rec(&[at], &alab, &holding(OLD)), "held");
    assert!(
        gate.names_rec(&[at], &alab, &holding(NEW)),
        "held, then newer records taken"
    );
}

/// A server that answers where another server was noted naming the records (a DHCP lease handed
/// on, or this Mac moving between two labs on one subnet without a catch-up): through the
/// production pull on loopback, after a round resolves `m7.local` naming the records this server
/// holds at the holder's endpoint, the next round's `alab-secrets` TXT there, labelled `m9`,
/// has its records pulled, since `m7` is not the instance it names; once a round resolves
/// `m9.local` naming them, the next round's `alab-secrets` TXT alone asks nothing; and one
/// naming no label is pulled beside that note too.
///
/// NEGATIVE CONTROLS (2026-11-15), each the holder's (envelope, records) GETs: with
/// `names_rec` matching by endpoints and vault alone (no look at the host), the round after
/// `m7`'s asks nothing (`left: (0, 0)`, `right: (0, 1)`); with `names_rec` answering false,
/// the round beside `m9` naming the records asks them (`left: (0, 2)`, `right: (0, 1)`); with a
/// TXT naming no label taken as naming any (`is_none_or`), the unlabelled round asks nothing
/// (`left: (0, 1)`, `right: (0, 2)`).
#[test]
fn another_servers_rec_at_the_same_endpoints_excuses_no_alab_secrets_records() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let own = serving(&root.path().join("own"), 5, |_, _| {
        Some(vec![IpAddr::V4(Ipv4Addr::LOCALHOST)])
    });
    let holder = peer(&root.path().join("holder"), 5);
    let founded = Founded::new();
    for server in [&own, &holder] {
        post(server, &[founded.log.genesis()]);
    }
    let own_port = own.addr.port();
    let t0 = Instant::now();
    swept(&own, &[&[holder.addr]], t0);
    let held = digest(&holder);
    assert_eq!(held, digest(&own));
    let alab = |label: Option<&str>| {
        browsed(
            "alab-secrets.local",
            &holder,
            VaultTxt {
                label: label.map(str::to_owned),
                ..txt(5, None)
            },
        )
    };
    let pass = |peers: &[crate::discover::DiscoveredVault], when| {
        pull_pass(
            &own.shared,
            peers,
            own_port,
            Some("m1"),
            &[],
            Pass::Gated(when),
        );
    };
    pass(&[browsed("m7.local", &holder, txt(5, held))], t0);
    assert_eq!(asked(&holder), (0, 0), "m7 naming the records held");
    pass(&[alab(Some("m9"))], t0 + PULL_SPACING);
    assert_eq!(asked(&holder), (0, 1), "m9's alab-secrets after m7's note");
    pass(
        &[alab(Some("m9")), browsed("m9.local", &holder, txt(5, held))],
        t0 + PULL_SPACING * 2,
    );
    assert_eq!(asked(&holder), (0, 1), "beside m9 naming the records held");
    pass(&[alab(Some("m9"))], t0 + PULL_SPACING * 3);
    assert_eq!(asked(&holder), (0, 1), "the round after m9's note");
    pass(&[alab(None)], t0 + PULL_SPACING * 4);
    assert_eq!(asked(&holder), (0, 2), "a TXT naming no label");
    own.stop();
    holder.stop();
}

/// A round that resolves the holder's own-label instance `m9` naming records this server lacks
/// makes them due, and where that round does not take them (the pacing holds its pull back, as
/// in a round a ring brings within [`PULL_SPACING`] of the group's last pull; a pull that fails
/// leaves the same note behind), what it noted stands for nothing: through the production pull
/// on loopback, the next round, whose batch leaves `m9` out, asks the holder its records through
/// its `alab-secrets` TXT and takes them, and the round after it, the records `m9` named now
/// held, asks nothing.
///
/// NEGATIVE CONTROL (2026-11-16): with `names_rec` excusing whatever `rec` was noted (no look at
/// `own.rec` or the proofs, the rule of 1d2bd27aa5e), the round after the one paced out asks
/// nothing (`left: (0, 0)`, `right: (0, 1)`).
#[test]
fn a_noted_rec_a_round_did_not_take_keeps_the_alab_secrets_records_due() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let own = serving(&root.path().join("own"), 5, |_, _| {
        Some(vec![IpAddr::V4(Ipv4Addr::LOCALHOST)])
    });
    let holder = peer(&root.path().join("holder"), 5);
    let mut founded = Founded::new();
    for server in [&own, &holder] {
        post(server, &[founded.log.genesis()]);
    }
    let first = founded.grow();
    post(&holder, &[&first]);
    let lacked = digest(&holder);
    assert_ne!(lacked, digest(&own));
    let own_port = own.addr.port();
    let t0 = Instant::now();
    swept(&own, &[&[holder.addr]], t0);
    // A round a moment before pulled the holder's group, so the next within the spacing may not.
    assert!(own.shared.lock_gate().pace(&[holder.addr], t0));
    let alab = browsed("alab-secrets.local", &holder, txt(5, None));
    let pass = |peers: &[crate::discover::DiscoveredVault], when| {
        pull_pass(
            &own.shared,
            peers,
            own_port,
            Some("m1"),
            &[],
            Pass::Gated(when),
        );
    };
    pass(
        &[alab.clone(), browsed("m9.local", &holder, txt(5, lacked))],
        t0 + Duration::from_secs(1),
    );
    assert_eq!(
        asked(&holder),
        (0, 0),
        "m9 naming records this server lacks, paced out"
    );
    pass(std::slice::from_ref(&alab), t0 + PULL_SPACING);
    assert_eq!(
        asked(&holder),
        (0, 1),
        "the next round, its batch without m9: the records"
    );
    assert_eq!(digest(&own), lacked, "and takes them");
    pass(&[alab], t0 + PULL_SPACING * 2);
    assert_eq!(asked(&holder), (0, 1), "the records m9 named held: nothing");
    own.stop();
    holder.stop();
}

/// A lab that converged, then changed again: through the production pull on loopback, a round
/// resolves the holder's own-label instance `m9` naming the records this server holds, and asks
/// nothing; both servers then take a newer record (a rotation's next, an enrolment), and the
/// rounds after it, whose batch leaves `m9` out, ask the holder nothing through its
/// `alab-secrets` TXT: what `m9` named this server held, and holds still under the newer
/// records, so a holder whose own-label instance last named an older digest is not asked its
/// records every round until the batch reaches it again.
///
/// NEGATIVE CONTROL (2026-11-16): with `names_rec` comparing the noted `rec` with this round's
/// `own.rec` alone (`named.rec == own.rec` in place of `named.held`, the rule of eeb4bedc5f9),
/// the round after the newer record asks the records (`left: (0, 1)`, `right: (0, 0)`), and so
/// would each paced round after it, each pull proving the holder's newer digest and never the
/// one `m9` named.
#[test]
fn a_noted_rec_this_server_held_excuses_the_alab_secrets_records_after_newer_ones() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let own = serving(&root.path().join("own"), 5, |_, _| {
        Some(vec![IpAddr::V4(Ipv4Addr::LOCALHOST)])
    });
    let holder = peer(&root.path().join("holder"), 5);
    let mut founded = Founded::new();
    let first = founded.grow();
    for server in [&own, &holder] {
        post(server, &[founded.log.genesis(), &first]);
    }
    let noted = digest(&holder);
    assert_eq!(noted, digest(&own));
    let own_port = own.addr.port();
    let t0 = Instant::now();
    swept(&own, &[&[holder.addr]], t0);
    let alab = browsed("alab-secrets.local", &holder, txt(5, None));
    let pass = |peers: &[crate::discover::DiscoveredVault], when| {
        pull_pass(
            &own.shared,
            peers,
            own_port,
            Some("m1"),
            &[],
            Pass::Gated(when),
        );
    };
    pass(
        &[alab.clone(), browsed("m9.local", &holder, txt(5, noted))],
        t0,
    );
    assert_eq!(asked(&holder), (0, 0), "m9 naming the records both hold");
    let second = founded.grow();
    for server in [&own, &holder] {
        post(server, &[&second]);
    }
    assert_ne!(digest(&own), noted);
    assert_eq!(digest(&own), digest(&holder));
    pass(std::slice::from_ref(&alab), t0 + PULL_SPACING);
    assert_eq!(
        asked(&holder),
        (0, 0),
        "the next round, its batch without m9, both holding a newer record"
    );
    pass(&[alab], t0 + PULL_SPACING * 2);
    assert_eq!(asked(&holder), (0, 0), "and the round after it");
    own.stop();
    holder.stop();
}

/// When a round sweeps a group ([`PullGate::sweep_due`]): one no round's sweep finished, one
/// whose period ([`sweep_after`], drawn as its sweep finished) passed, and every one after a
/// catch-up; each group by its own sweep, whatever another's. A catch-up after a wake also
/// forgets the pacing, which an [`Instant`] that stood still through the sleep would stretch.
///
/// NEGATIVE CONTROLS (2026-11-15): with `sweep_due` ignoring catch-ups, the group just swept is
/// not due after one ("a catch-up came since"); with `catch_up` keeping the pacing after a wake,
/// the group paced a second before is held back ("after a wake").
#[test]
fn a_group_is_swept_at_first_sight_after_each_catch_up_and_past_its_period() {
    let at = |n: u8| SocketAddr::from(([127, 0, 0, n], 7484));
    let (one, two) = ([at(1)], [at(2)]);
    let second = Duration::from_secs(1);
    let mut gate = PullGate::default();
    let t0 = Instant::now();
    assert!(gate.sweep_due(&one, t0), "never swept");
    gate.swept(&one, t0, 0);
    assert!(!gate.sweep_due(&one, t0 + second), "just swept");
    assert!(gate.sweep_due(&two, t0 + second), "another group");
    let period = sweep_after(0);
    assert!(!gate.sweep_due(&one, t0 + period - Duration::from_millis(1)));
    assert!(gate.sweep_due(&one, t0 + period), "its period passed");
    gate.swept(&one, t0 + second, u64::MAX);
    let longest = sweep_after(u64::MAX);
    assert!(!gate.sweep_due(&one, t0 + second + longest - Duration::from_millis(1)));
    gate.catch_up(false);
    assert!(
        gate.sweep_due(&one, t0 + second * 2),
        "a catch-up came since"
    );
    gate.swept(&one, t0 + second * 2, 0);
    assert!(
        !gate.sweep_due(&one, t0 + second * 3),
        "swept since the catch-up"
    );

    assert!(gate.pace(&two, t0));
    assert!(!gate.pace(&two, t0 + second));
    gate.catch_up(false);
    assert!(!gate.pace(&two, t0 + second), "a catch-up of a Mac awake");
    gate.catch_up(true);
    assert!(gate.pace(&two, t0 + second), "after a wake");
}

/// More servers than a round resolves: each round resolves a rotating batch of at most
/// [`crate::discover::MAX_CANDIDATES`], and each server is swept by the first round whose batch
/// holds it, its TXT naming nothing this server lacks; a server swept is not swept again before
/// its period, and is once it passed.
///
/// NEGATIVE CONTROL (2026-11-15): with a server first seen swept only while no round swept any
/// (step 6's sweep of a round's batch alone), the servers the second round first resolves are
/// asked nothing (`left: (0, 0)`, `right: (1, 1)`).
#[test]
fn every_server_is_swept_by_the_first_round_whose_batch_resolves_it() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let own = serving(&root.path().join("own"), 5, |_, _| None);
    let batch = crate::discover::MAX_CANDIDATES;
    let peers: Vec<Running> = (0..batch + 2)
        .map(|n| peer(&root.path().join(format!("peer{n}")), 5))
        .collect();
    let none_held = Some(crate::membership_log::empty_heads_digest());
    let round = |range: std::ops::Range<usize>, when: Instant| {
        let named: Vec<crate::discover::DiscoveredVault> = range
            .map(|n| browsed("127.0.0.1", &peers[n % peers.len()], txt(5, none_held)))
            .collect();
        assert!(named.len() <= batch);
        pull_pass(
            &own.shared,
            &named,
            own.addr.port(),
            Some("m1"),
            &[],
            Pass::Gated(when),
        );
    };
    let t0 = Instant::now();
    round(0..batch, t0);
    for (n, peer) in peers.iter().enumerate() {
        assert_eq!(
            asked(peer),
            (usize::from(n < batch), usize::from(n < batch)),
            "peer{n}"
        );
    }
    let later = t0 + Duration::from_secs(30);
    round(batch..2 * batch, later);
    for (n, peer) in peers.iter().enumerate() {
        assert_eq!(
            asked(peer),
            (1, 1),
            "peer{n}, first resolved by round {}",
            1 + n / batch
        );
    }
    round(2..batch + 2, later + sweep_after(u64::MAX));
    for (n, peer) in peers.iter().enumerate() {
        let swept = 1 + usize::from(n >= 2);
        assert_eq!(asked(peer), (swept, swept), "peer{n} past its period");
    }
    own.stop();
    for peer in peers {
        peer.stop();
    }
}

/// A sweep the pacing holds back (a gated pull of that group came less than [`PULL_SPACING`]
/// before) stays due, and the next round past the spacing makes it: the records a stale TXT
/// hid are taken.
///
/// NEGATIVE CONTROL (2026-11-15): with a sweep marked as the round selects it, before pacing and
/// whatever its pull finished, the round past the spacing asks nothing (`left: (1, 0)`,
/// `right: (2, 1)`).
#[test]
fn a_sweep_the_pacing_held_back_is_made_by_the_next_round_past_it() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let own = serving(&root.path().join("own"), 5, |_, _| None);
    let peer = peer(&root.path().join("peer"), 5);
    let mut founded = Founded::new();
    let first = founded.grow();
    post(&own, &[founded.log.genesis()]);
    post(&peer, &[founded.log.genesis(), &first]);
    // A stale TXT: it names this server's records, not the peer's.
    let stale = digest(&own);
    let t0 = Instant::now();
    swept(&own, &[&[peer.addr]], t0);
    let due = t0 + sweep_after(0);
    let pass = |seq, when| records_pass_at(&own, &peer, seq, stale, when);
    publish(&peer, 6);
    pass(6, due - Duration::from_secs(2));
    assert_eq!(asked(&peer), (1, 0), "a gated pull, its envelope alone");
    pass(6, due);
    assert_eq!(asked(&peer), (1, 0), "the sweep, held back by the pacing");
    pass(6, due + Duration::from_secs(3));
    assert_eq!(asked(&peer), (2, 1), "made past the spacing");
    assert_eq!(digest(&own), digest(&peer));
    own.stop();
    peer.stop();
}

/// [`records_pass`], the TXT naming `seq`.
fn records_pass_at(own: &Running, at: &Running, seq: u64, rec: Option<[u8; 32]>, when: Instant) {
    pull_pass(
        &own.shared,
        &[browsed("127.0.0.1", at, txt(seq, rec))],
        own.addr.port(),
        Some("m1"),
        &[],
        Pass::Gated(when),
    );
}

/// Rounds catch up at the first round, after a wake, and when this Mac's addresses change,
/// compared as a set: the same addresses in another order (the routed one listed first moves
/// when the route flips) are no change. A sweep's period is [`ANTI_ENTROPY_EVERY`] within ±20%.
///
/// NEGATIVE CONTROL (2026-11-15): with the addresses compared in order (step 6's), the same
/// addresses in another order catch up ("the same addresses in another order").
#[test]
fn rounds_catch_up_at_the_first_round_a_wake_and_new_addresses() {
    assert_eq!(sweep_after(0), ANTI_ENTROPY_EVERY * 4 / 5);
    assert_eq!(sweep_after(u64::MAX), ANTI_ENTROPY_EVERY * 6 / 5);
    assert_eq!(
        sweep_after(u64::MAX / 2),
        ANTI_ENTROPY_EVERY - Duration::from_millis(1)
    );
    let target = |scope: &str, last: u8| Target {
        scope: Some(scope.to_owned()),
        ip: Ipv4Addr::new(10, 0, 0, last),
    };
    let here = vec![target("en0", 5), target("en1", 7)];
    let flipped = vec![target("en1", 7), target("en0", 5)];
    let moved = vec![target("en0", 6)];
    let mut catch_up = CatchUp::default();
    assert!(catch_up.due(None, &here), "the first round");
    assert!(!catch_up.due(Some(Wake::Timer), &here));
    assert!(catch_up.due(Some(Wake::Resumed), &here), "a wake");
    assert!(
        !catch_up.due(Some(Wake::Route), &flipped),
        "the same addresses in another order"
    );
    assert!(catch_up.due(Some(Wake::Route), &moved), "new addresses");
    assert!(!catch_up.due(Some(Wake::Route), &moved));
    assert!(!catch_up.due(Some(Wake::Timer), &moved));
}

/// `alab-secrets` rings a server's standing browse as any peer's instance does, unless this
/// server holds the name: a round notes it as its own only while it registers it, and forgets
/// it once it does not.
///
/// NEGATIVE CONTROL (2026-11-15): with `forget_own` keeping the name, the holder's Add after the
/// name was given up rings nothing (`left: Timer`, `right: Browse`).
#[test]
fn alab_secrets_rings_the_browse_unless_this_server_holds_it() {
    let bell = Arc::new(Bell::default());
    let watch = crate::discover::Watch::idle(&bell, &["m5"]);
    let rung = || bell.nap(Duration::ZERO);
    watch.hear(true, 4, crate::alab_advertised());
    assert_eq!(rung(), Wake::Browse, "another server's holder");
    watch.hear(false, 4, crate::alab_advertised());
    watch.note_own(crate::alab_advertised());
    watch.hear(true, 4, crate::alab_advertised());
    assert_eq!(
        rung(),
        Wake::Timer,
        "this server's own while it holds the name"
    );
    watch.hear(false, 4, crate::alab_advertised());
    watch.forget_own(crate::alab_advertised());
    watch.hear(true, 4, crate::alab_advertised());
    assert_eq!(rung(), Wake::Browse, "once it gave the name up");
}

/// What the real gate does from a state of [`aterm_models_swarm::peer_gate_model`]: the
/// environment's moves as the model makes them, and `Gate` as [`pull_due`] decides it given
/// `named_elsewhere`, over a server holding `seq` 5 (when `held`) and records digest `[1; 32]`,
/// of a TXT naming `seq` 4, 5 or 6 (`adv`) and `rec` none, `[1; 32]`, `[2; 32]` (proven) or
/// `[3; 32]`, on `m9.local` with `named_elsewhere` either way, on `alab-secrets.local` with it
/// false, or on `alab-secrets.local` with it true (`inst`). Each is decided with the TXT naming
/// label `m9` and naming none, and every way of a state must agree: neither a label nor
/// `named_elsewhere` on an instance other than `alab-secrets` moves a pull. `named_elsewhere` is
/// an input here: how a round finds it ([`PullGate::note_named`], [`PullGate::names_rec`],
/// which answers it only for the instance the TXT's label names, so never for a TXT naming
/// none) is not modelled.
fn gate_transition(state: &aterm_spec::interp::State, action: &str) -> aterm_spec::interp::State {
    let mut next = state.clone();
    let mut set = |var: &'static str, value: i64| {
        next.insert(var, value);
    };
    match action {
        "Lower" => set("adv", 0),
        "Higher" => set("adv", 2),
        "Unheld" => set("held", 0),
        "Stranger" => {
            set("takes", 0);
            set("held", 0);
        }
        "NoRec" => set("rec", 0),
        "ProvenRec" => set("rec", 2),
        "OtherRec" => set("rec", 3),
        "AlabAlone" => set("inst", 1),
        "AlabNamedElsewhere" => set("inst", 2),
        "Gate" => {
            let takes = state["takes"] == 1;
            let own = Own {
                seq: (state["held"] == 1).then_some(5),
                rec: [1; 32],
                takes_envelope: takes,
                takes_records: takes,
                follows: [0; 32],
            };
            let rec = match state["rec"] {
                0 => None,
                n => Some([u8::try_from(n).expect("small"); 32]),
            };
            let ways: &[(&str, bool)] = match state["inst"] {
                0 => &[("m9.local", false), ("m9.local", true)],
                1 => &[("alab-secrets.local", false)],
                _ => &[("alab-secrets.local", true)],
            };
            let seq = 4 + u64::try_from(state["adv"]).expect("small");
            let dues: Vec<Due> = ways
                .iter()
                .flat_map(|&(host, named_elsewhere)| {
                    [Some("m9"), None].map(|label| {
                        let advertised = VaultTxt {
                            label: label.map(str::to_owned),
                            ..txt(seq, rec)
                        };
                        pull_due(
                            &named(host, Some(&advertised)),
                            named_elsewhere,
                            &own,
                            |rec| *rec == [2; 32],
                        )
                    })
                })
                .collect();
            assert!(
                dues.windows(2).all(|pair| pair[0] == pair[1]),
                "every way of {state:?} decides alike: {dues:?}"
            );
            let due = dues[0];
            set("phase", 1);
            set("env", i64::from(due.envelope));
            set("recs", i64::from(due.records));
        }
        _ => panic!("gate action {action}"),
    }
    next
}

/// Every move the gate's model reaches is one [`pull_due`] (all a round asks of a peer's TXT)
/// makes given `named_elsewhere`: a newer or first envelope is pulled, records a TXT names that
/// this server neither holds nor proved, a `rec`-less TXT's records at every visit unless it is
/// an `alab-secrets` instance's and `named_elsewhere` holds, and nothing a TXT shows held or
/// proven, nor of a vault this server takes nothing of. It binds `pull_due` alone: which
/// instances make `named_elsewhere` hold for a round, those it resolved and those the gate
/// remembers ([`PullGate::note_named`], [`PullGate::names_rec`]), is bound by the round tests
/// (`an_alab_secrets_instance_is_gated_and_names_no_records`,
/// `a_labelled_alab_secrets_instance_with_no_rec_named_beside_it_is_pulled_every_round`,
/// `two_alab_secrets_instances_naming_no_rec_have_their_records_pulled_every_round`,
/// `a_named_rec_stands_for_its_label_at_exactly_the_endpoints_it_resolved_to_until_a_catch_up`,
/// `an_instance_resolving_to_no_endpoint_leaves_what_was_noted_of_it_unless_it_names_no_rec`,
/// `another_servers_rec_at_the_same_endpoints_excuses_no_alab_secrets_records`,
/// `a_noted_rec_stands_only_where_this_server_holds_it_or_proved_it_there`,
/// `a_noted_rec_this_server_held_stands_after_it_takes_newer_records`,
/// `a_noted_rec_a_round_did_not_take_keeps_the_alab_secrets_records_due`,
/// `a_noted_rec_this_server_held_excuses_the_alab_secrets_records_after_newer_ones` and
/// `round_counts_tests::a_holders_own_label_instance_out_of_the_batch_gates_its_records_for_a_sweep_period`).
/// That the model holds its laws and each mutant breaks one is
/// `crates/aterm-models/tests/non_vacuity.rs`'s, which sweeps every registered model.
///
/// NEGATIVE CONTROLS (2026-11-15): with `pull_due`'s rec-less arm back to step 6's
/// `None => envelope`, the model rejects a real `Gate` (the loop's assertion fails) from a state
/// whose TXT names no `rec` and no newer version (`inst: 0`, `recs: 0` where the model has 1);
/// with every rec-less `alab-secrets` TXT excused (`None => !PullGate::alab_instance(…)`), from
/// the state whose TXT is on `alab-secrets` with `named_elsewhere` false (`inst: 1`, `recs: 0`
/// where the model has 1); with no `alab-secrets` exception (`None => true`), from the state
/// with it true (`inst: 2`, `recs: 1` where the model has 0). The ways of one state disagree
/// ("every way of … decides alike" fails) with the label taken as the proof (`inst: 1`, the
/// labelled TXT pulling no records), with no look at the instance (`None => !named_elsewhere`,
/// `inst: 0` with `named_elsewhere` pulling none) and with the label required where
/// `named_elsewhere` holds (`inst: 2`, the TXT naming no label pulling them).
#[test]
fn the_gate_conforms_to_its_model() {
    let model = aterm_models_swarm::peer_gate_model();
    let mut gated = 0;
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let actual = gate_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&actual),
                    "{}: {state:?} -> {actual:?}",
                    action.name
                );
                gated += usize::from(action.name == "Gate");
            }
        }
    }
    assert!(gated > 8, "{gated} gates");
    // A rec-less TXT on an own-label instance, and on an `alab-secrets` one with
    // `named_elsewhere` false.
    for inst in [0, 1] {
        let mut state = model.init_state();
        state.insert("rec", 0);
        state.insert("inst", inst);
        let mut bad = gate_transition(&state, "Gate");
        bad.insert("recs", 0);
        assert!(!model.successors("Gate", &state).contains(&bad), "{inst}");
        assert!(!model.check_invariant("RecLessProgress", &bad), "{inst}");
    }
}
