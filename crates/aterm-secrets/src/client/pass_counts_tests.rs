// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What a subscriber's pass counts ([`crate::pass_counts`]), driven through the production pass
//! against vault servers on loopback: counted, never timed.

use super::log_entries_tests::{join_host, start_loopback_server};
use super::*;
use crate::pass_counts::PassCounts;
use std::sync::Arc;

/// A member (`m9`) of a `team` vault at seq 2, its source a server on loopback, `peers` more
/// servers that each hold the same seq 2, and one more that holds no vault.
struct Lab {
    root: aterm_tempfile::TempDir,
    admin: Paths,
    member: Paths,
    source: Option<crate::server::Running>,
    peers: Vec<crate::server::Running>,
    /// Every peer: those that hold the vault, the one that holds none, then each [`Lab::serve`]
    /// added.
    found: Vec<crate::discover::DiscoveredVault>,
    /// The bytes the source, each holding peer and the member hold.
    held: Vec<u8>,
}

fn lab(peers: usize) -> Lab {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let source = start_loopback_server(&root.path().join("source"));
    let url = format!("http://{}/v1/team", source.addr);
    let admin = Paths::for_home(&root.path().join("admin"));
    create(&admin, &url, None, Some("m3")).expect("create the vault");
    let member = Paths::for_home(&root.path().join("m9"));
    let recipient = machine_identity(&member).unwrap().recipient().to_bech32();
    let opened = admin_open(&admin).unwrap();
    let mut doc = opened.doc.clone();
    doc.add_machine("m9", &recipient).unwrap();
    doc.set_entry(&Audience::Everyone, "shared-key", b"v1")
        .unwrap();
    assert_eq!(admin_publish(&admin, &opened, &doc).unwrap().seq, 2);
    let held = std::fs::read(admin.cache_file()).unwrap();
    let peers: Vec<_> = (0..=peers)
        .map(|n| start_loopback_server(&root.path().join(format!("peer{n}"))))
        .collect();
    let found: Vec<_> = peers
        .iter()
        .map(|peer| crate::discover::DiscoveredVault {
            url: format!("http://{}/v1/team", peer.addr),
            txt: None,
        })
        .collect();
    for peer in &found[..found.len() - 1] {
        transport::publish(&Source::Url(peer.url.clone()), &held).expect("seed a peer");
    }
    let pin = config(&admin).unwrap().unwrap().signer.unwrap();
    join_host(
        &member,
        &format!("{url}#{}", crate::b64::encode_padded(&pin)),
        "m9",
    );
    assert_eq!(load_state(&member).unwrap().seq, 2);
    Lab {
        root,
        admin,
        member,
        source: Some(source),
        peers,
        found,
        held,
    }
}

impl Lab {
    /// One more peer, a server on loopback that holds `bytes` as the vault.
    fn serve(&mut self, name: &str, bytes: &[u8]) {
        let server = start_loopback_server(&self.root.path().join(name));
        let url = format!("http://{}/v1/team", server.addr);
        transport::publish(&Source::Url(url.clone()), bytes).expect("seed a peer");
        self.peers.push(server);
        self.found
            .push(crate::discover::DiscoveredVault { url, txt: None });
    }

    fn stop(self) {
        for server in self.source.into_iter().chain(self.peers) {
            server.stop();
        }
    }
}

/// Every count of `counts` that is not 0, by name.
fn counted(counts: &PassCounts) -> Vec<(&'static str, u64)> {
    Count::ALL
        .iter()
        .map(|what| (what.name(), counts.get(*what)))
        .filter(|(_, n)| *n != 0)
        .collect()
}

/// One pass of the member over the lab's peers, counted alone: its counts, and the threads
/// that ended in each phase they were started in.
fn pass(at: &Lab, repaired: &mut Repaired) -> (Arc<PassCounts>, SyncOutcome) {
    let found = |_: &Config| at.found.clone();
    let found = PeerDiscovery::supplied(&found);
    let counts = Arc::new(PassCounts::default());
    let metered = pass_counts::metered(&counts);
    let (_, outcome) = subscriber_pass(&at.member, repaired, &found).expect("a pass");
    drop(metered);
    (counts, outcome)
}

/// [`counted`], less what depends on what the process already keeps: what the pass parses and
/// what it asks of the held vault's entry and of signatures are counted too, and an entry
/// another test's vaults pushed out of the process's cache of parsed vaults is parsed and asked
/// again, as a signature this process has not verified before is computed.
fn sites(counts: &PassCounts) -> Vec<(&'static str, u64)> {
    let kept = [
        "parses",
        "entry_verifies",
        "signature_asks",
        "signatures_computed",
    ];
    counted(counts)
        .into_iter()
        .filter(|(name, _)| !kept.contains(name))
        .collect()
}

/// `base` at `advance` versions later, descending from `parent`, signed with `seed`.
fn signed_from(base: &[u8], seed: &[u8; 32], advance: u64, parent: [u8; 32]) -> Vec<u8> {
    let base = envelope::parse(base).unwrap();
    envelope::sign(
        &Unsigned {
            name: base.name,
            seq: base.seq + advance,
            time: base.time,
            handoff: base.handoff,
            parents: vec![parent],
            lockers: base.lockers,
        },
        seed,
    )
    .unwrap()
}

fn threads(counts: &PassCounts) -> Vec<(&'static str, u64)> {
    Phase::ALL
        .iter()
        .map(|phase| {
            let ended = counts.threads(*phase).ended.load(Ordering::Relaxed);
            (phase.name(), ended)
        })
        .filter(|(_, n)| *n != 0)
        .collect()
}

/// The marks into each phase the pass's thread passed, by the phase's name, less those it
/// passed none of.
fn entries(counts: &PassCounts) -> Vec<(&'static str, u64)> {
    Phase::ALL
        .iter()
        .map(|phase| (phase.name(), counts.entered(*phase)))
        .filter(|(_, n)| *n != 0)
        .collect()
}

/// The law of a pass's counts: a production pass ([`subscriber_pass`]) of a member whose source
/// and 3 of 4 peers hold the vault it holds (the 4th holds none) counts one pass, the source's
/// 204 with its seq, one records fan-out and one envelope probe of 4 peers each, 3 bodies of the
/// held length and one 404, and each thread it started in the phase that started it: the records
/// fan-out's 3 `vault-read` threads and the one that asked the source, and the probe's 3. Every
/// body is the held bytes, so the held vault's entry answers each verification (3 at the probe, 3
/// of the peers and 1 of the held vault in selection, 1 for the admin text) and no bytes are
/// verified whole: `verifies` and each `fresh_` site are absent, against 8 of each before the
/// entry answered. The pass's thread passes each of the pass's phase marks once, and the mark
/// into choosing peers once for the records fan-out and once for the probe, so the time a phase
/// is said to have taken is the time between its own marks. A source that then holds a newer
/// version is counted as a body, verified whole. Counted, never timed.
///
/// Negative controls: delete `pass_counts::add_n(Count::ProbePeers, urls.len())` in
/// `probe_peers_for_sync` (`probe_peers` is absent, wants 4); run the fan-out's threads without
/// `started.run` in `fan_out::each_at_once` (`probe_bodies` reads 1 and `probe_missing` is
/// absent: only the loop thread's own share of the probe is counted); find no entry in
/// `chain_cache::verify_as_held` (`held.filter(|_| false)`: `verifies` reads 6 with
/// `fresh_probe` 3 and `fresh_select_peer` 3, and `memo_probe` and `memo_select_peer` are
/// absent); delete `let _select = pass_counts::within(Phase::Select);` in
/// `sync_repairing_inner` (`("select", 1)` is absent from the phases entered).
#[test]
fn a_pass_counts_what_it_asked_what_each_peer_answered_and_who_verified() {
    let _answering = ServersAnswering::set();
    let at = lab(3);
    let held = at.held.len() as u64;
    let mut repaired = None;
    // Twice: the counts of a pass at rest are the same pass after pass.
    for _ in 0..2 {
        let (counts, outcome) = pass(&at, &mut repaired);
        assert_eq!(outcome, SyncOutcome::Unchanged { seq: 2 });
        let want = vec![
            ("passes", 1),
            ("primary_204_seq", 1),
            ("records_fanouts", 1),
            ("records_peers", 4),
            ("probes", 1),
            ("probe_peers", 4),
            ("probe_bodies", 3),
            ("probe_bytes", 3 * held),
            ("probe_missing", 1),
            ("memo_probe", 3),
            ("memo_select_held", 1),
            ("memo_select_peer", 3),
            ("memo_admin", 1),
        ];
        assert_eq!(sites(&counts), want);
        assert_eq!(threads(&counts), [("records", 4), ("probe", 3)]);
        assert_eq!(
            entries(&counts),
            [
                ("records", 1),
                ("peers", 2),
                ("removal", 1),
                ("primary", 1),
                ("admin", 1),
                ("probe", 1),
                ("select", 1),
                ("apply", 1),
                ("rest", 1),
            ]
        );
    }
    // The source holds a newer version: its answer is a body, verified as the source's.
    let opened = admin_open(&at.admin).unwrap();
    let mut doc = opened.doc.clone();
    doc.set_entry(&Audience::Everyone, "shared-key", b"v2")
        .unwrap();
    assert_eq!(admin_publish(&at.admin, &opened, &doc).unwrap().seq, 3);
    let (counts, outcome) = pass(&at, &mut repaired);
    assert!(
        matches!(outcome, SyncOutcome::Applied { seq: 3, .. }),
        "{outcome:?}"
    );
    assert_eq!(counts.get(Count::PrimaryBody), 1);
    assert_eq!(counts.get(Count::PrimarySeq204), 0);
    assert!(counts.get(Count::FreshPrimary) >= 1);
    at.stop();
}

/// The law of which bodies the held vault's entry answers for, through the production pass:
/// beside 3 peers that serve the held bytes, a peer that serves the held seq with another body
/// (signed by the vault's key) and a peer that serves a body a stranger signed are each verified
/// whole at the probe, on every pass, and the stranger's is refused on every
/// pass (it is never among the peers selection reads); the other body is verified whole in
/// selection too. Neither becomes an entry of the process's cache of parsed vaults. A peer that
/// then serves a valid next version is verified whole at the probe and in selection, and is
/// applied.
///
/// Negative controls: take the entry for any body of its seq in `chain_cache::verify_as_held`
/// (`held.filter(|held| seq_of(held.bytes()) == seq_of(bytes))`, each seq read by
/// `envelope::parse`): the other body is answered by the entry (`memo_probe` 4 with
/// `fresh_probe` 1, wants 3 and 2); keep each body verified whole
/// (`CACHE.parsed(SecretBytes::from(bytes), None)` in its whole arm): the other body is an entry
/// (`chain_cache::keeps(&fork)`).
#[test]
fn a_body_that_is_not_the_held_bytes_is_verified_whole_on_every_pass() {
    let _answering = ServersAnswering::set();
    let mut at = lab(3);
    let seed = admin_open(&at.admin).unwrap().seed;
    let head = load_state(&at.member).unwrap().digest.unwrap();
    let fork = signed_from(&at.held, &seed, 0, [0x55; 32]);
    assert_ne!(fork, at.held);
    let strange = signed_from(&at.held, &[19; 32], 1, head);
    // The peer that held no vault serves the other body, and one more the stranger's.
    transport::publish(&Source::Url(at.found[3].url.clone()), &fork).expect("seed the fork");
    at.serve("stranger", &strange);
    let bytes = 3 * at.held.len() + fork.len() + strange.len();
    let mut repaired = None;
    for _ in 0..2 {
        let (counts, outcome) = pass(&at, &mut repaired);
        assert_eq!(outcome, SyncOutcome::Unchanged { seq: 2 });
        let want = vec![
            ("passes", 1),
            ("primary_204_seq", 1),
            ("records_fanouts", 1),
            ("records_peers", 5),
            ("probes", 1),
            ("probe_peers", 5),
            ("probe_bodies", 5),
            ("probe_bytes", bytes as u64),
            ("verifies", 3),
            ("fresh_probe", 2),
            ("fresh_select_peer", 1),
            ("memo_probe", 3),
            ("memo_select_held", 1),
            ("memo_select_peer", 3),
            ("memo_admin", 1),
        ];
        assert_eq!(sites(&counts), want);
        assert!(!chain_cache::keeps(&fork));
        assert!(!chain_cache::keeps(&strange));
    }
    // A holder serves the next version: verified whole where it is read, and applied.
    let next = signed_from(&at.held, &seed, 1, head);
    transport::publish(&Source::Url(at.found[0].url.clone()), &next).expect("publish the next");
    let (counts, outcome) = pass(&at, &mut repaired);
    assert!(
        matches!(outcome, SyncOutcome::Applied { seq: 3, .. }),
        "{outcome:?}"
    );
    assert_eq!(counts.get(Count::FreshProbe), 3);
    assert_eq!(counts.get(Count::MemoProbe), 2);
    assert_eq!(counts.get(Count::FreshSelectPeer), 2);
    assert_eq!(counts.get(Count::MemoSelectPeer), 2);
    assert_eq!(counts.get(Count::MemoSelectHeld), 1);
    at.stop();
}

/// A probed peer whose body is longer than the probe's first read is asked a second time, for
/// the whole of it, and that second request is counted: beside a peer that holds no vault, the
/// one peer that serves the held vault, here longer than `fan_out::ENVELOPES`' peek, is one
/// whole read and one body of the held length, on each of two passes.
///
/// Negative control: delete `pass_counts::add(Count::ProbeWholeReads)` in
/// `probe_peers_for_sync` (`probe_whole_reads` is absent, wants 1).
#[test]
fn a_probed_body_longer_than_the_first_read_is_counted_as_a_second_request() {
    let _answering = ServersAnswering::set();
    let at = lab(1);
    // The vault grows past the peek: the source and the holding peer serve the longer version,
    // and the member takes it from its source.
    let opened = admin_open(&at.admin).unwrap();
    let mut doc = opened.doc.clone();
    doc.set_entry(
        &Audience::Everyone,
        "long",
        &vec![b'x'; fan_out::ENVELOPES.peek],
    )
    .unwrap();
    assert_eq!(admin_publish(&at.admin, &opened, &doc).unwrap().seq, 3);
    let held = std::fs::read(at.admin.cache_file()).unwrap();
    assert!(held.len() > fan_out::ENVELOPES.peek);
    transport::publish(&Source::Url(at.found[0].url.clone()), &held).expect("seed the peer");
    let mut repaired = None;
    let (_, outcome) = pass(&at, &mut repaired);
    assert!(
        matches!(outcome, SyncOutcome::Applied { seq: 3, .. }),
        "{outcome:?}"
    );
    for _ in 0..2 {
        let (counts, outcome) = pass(&at, &mut repaired);
        assert_eq!(outcome, SyncOutcome::Unchanged { seq: 3 });
        let want = vec![
            ("passes", 1),
            ("primary_204_seq", 1),
            ("records_fanouts", 1),
            ("records_peers", 2),
            ("probes", 1),
            ("probe_peers", 2),
            ("probe_whole_reads", 1),
            ("probe_bodies", 1),
            ("probe_bytes", held.len() as u64),
            ("probe_missing", 1),
            ("memo_probe", 1),
            ("memo_select_held", 1),
            ("memo_select_peer", 1),
            ("memo_admin", 1),
        ];
        assert_eq!(sites(&counts), want);
    }
    at.stop();
}

/// A floor that verifies the held vault's bytes whole is counted as a site of its own
/// (`fresh_floor`), through the production [`floor`]: the lab's member, whose state holds its
/// membership and the held version's digest, takes its floor from the held entry and verifies
/// nothing; a state with no membership stored, and one with no digest, each verify the held
/// bytes whole once.
///
/// Negative control: delete `pass_counts::add(Count::FreshFloor)` in `floor` (`left: (0, 1)`,
/// `right: (1, 1)`).
#[test]
fn a_floor_that_verifies_the_held_bytes_is_counted_as_its_own_site() {
    let _answering = ServersAnswering::set();
    let at = lab(0);
    let config = config(&at.member).unwrap().unwrap();
    let state = load_state(&at.member).unwrap();
    assert_ne!(state.membership, Stored::None);
    assert!(state.digest.is_some());
    let counts = Arc::new(PassCounts::default());
    let metered = pass_counts::metered(&counts);
    let read = || (counts.get(Count::FreshFloor), counts.get(Count::Verifies));
    let held = floor(&at.member, &state, &config.name, &config).expect("a floor");
    assert_eq!(read(), (0, 0));
    let unjoined = State {
        membership: Stored::None,
        ..state.clone()
    };
    let verified = floor(&at.member, &unjoined, &config.name, &config).expect("a floor");
    assert_eq!(read(), (1, 1));
    assert_eq!(format!("{verified:?}"), format!("{held:?}"));
    let undigested = State {
        digest: None,
        ..state
    };
    floor(&at.member, &undigested, &config.name, &config).expect("a floor");
    assert_eq!(read(), (2, 2));
    drop(metered);
    at.stop();
}

/// Each answer of a source and of a probed peer is one count, its own.
///
/// Negative control: class a 204 that named no seq with one that named it in `answer_count`
/// (`(Fetched::NoNewer(_), _) => Count::PrimarySeq204` first): the bare 204 and the held-out
/// request read `PrimarySeq204`.
#[test]
fn each_answer_is_counted_as_what_it_was() {
    let body = || Fetched::Body(vec![1, 2, 3]);
    assert_eq!(answer_count(&body(), false), Count::PrimaryBody);
    assert_eq!(
        answer_count(&Fetched::Missing, false),
        Count::PrimaryMissing
    );
    for replied in [false, true] {
        assert_eq!(
            answer_count(&Fetched::NoNewer(Some(7)), replied),
            Count::PrimarySeq204
        );
    }
    assert_eq!(
        answer_count(&Fetched::NoNewer(None), true),
        Count::PrimaryBare204
    );
    assert_eq!(
        answer_count(&Fetched::NoNewer(None), false),
        Count::PrimaryHeldOut
    );
    assert_eq!(source_answer(&Err(Error::new("away"))), Count::PrimaryError);
    let pulled = |fetched| Ok(transport::Pulled::Fetched(fetched));
    assert_eq!(peer_answer(&pulled(body())), Count::ProbeBodies);
    assert_eq!(
        peer_answer(&pulled(Fetched::NoNewer(Some(7)))),
        Count::ProbeNoNewer
    );
    assert_eq!(
        peer_answer(&pulled(Fetched::NoNewer(None))),
        Count::ProbeNoNewer
    );
    assert_eq!(peer_answer(&pulled(Fetched::Missing)), Count::ProbeMissing);
    assert_eq!(
        peer_answer(&Ok(transport::Pulled::Larger)),
        Count::ProbeUnanswered
    );
    assert_eq!(
        peer_answer(&Err(Error::new("away"))),
        Count::ProbeUnanswered
    );
}

/// The subscriber's loop says its counts in its log as its first and its second pass end: the
/// production loop ([`subscriber_loop_until`]) over the lab, stopped after two passes, says the
/// two lines at each, every total and the last passes', each with the passes and the source's
/// answers. The loop's own phase marks are in them: by the end of the second pass its thread
/// passed the mark before a pass twice, the one after a pass twice, and the one into the pause
/// between the two once. That pause is ended by the bell (a listener's ring, rung where the loop
/// asks whether to stop before it pauses), so nothing here waits a pause out.
///
/// Negative controls: delete the `log(&line)` of the lines in `subscriber_loop_projecting` (no
/// line holds `pass counts`: 0 lines, wants 4); delete the `pass_counts::enter(Phase::Pause)`
/// before a healthy pass's pause (the second pass's lines hold no `pause_entries`, wants 1).
#[test]
fn the_loop_says_its_counts_in_its_log_as_a_due_pass_ends() {
    use std::sync::atomic::AtomicUsize;
    let _answering = ServersAnswering::set();
    let at = lab(1);
    let found = |_: &Config| at.found.clone();
    let found = PeerDiscovery::supplied(&found);
    let bell = crate::wake::Bell::new();
    let logged = std::sync::Mutex::new(Vec::<String>::new());
    // The loop asks at the top of each round and again once that round's pass has ended: the
    // second ask is the one before the first pause, the fourth ends the loop.
    let asked = AtomicUsize::new(0);
    subscriber_loop_until(
        &at.member,
        &|line| logged.lock().expect("log").push(line.to_owned()),
        &|| {
            let asks = asked.fetch_add(1, Ordering::Relaxed);
            if asks == 1 {
                bell.ring(crate::wake::Wake::Listener);
            }
            asks >= 3
        },
        &Converging {
            start: &|_, _| None,
            routes: &crate::server::route_sources,
        },
        &Hearing {
            found: &found,
            bell: &bell,
            routes: false,
            ask: &|_| Ok(0),
            members: std::sync::Mutex::new(MembersAsked::default()),
            hold: &|| {},
            away: &|| false,
        },
    )
    .expect("the loop ends cleanly");
    assert_eq!(asked.load(Ordering::Relaxed), 4);
    let logged = logged.into_inner().expect("log");
    let said: Vec<&String> = logged
        .iter()
        .filter(|line| line.contains("pass counts"))
        .collect();
    assert_eq!(said.len(), 4, "{logged:?}");
    assert!(
        said[0].starts_with("pass counts: passes=1 primary_204_seq=1 "),
        "{}",
        said[0]
    );
    assert!(
        said[1].starts_with("pass counts, last 1 passes: passes=1 primary_204_seq=1 "),
        "{}",
        said[1]
    );
    for line in &said[..2] {
        assert!(line.contains(" probe_peers=2 probe_bodies=1 "), "{line}");
        assert!(line.contains(" before_entries=1 "), "{line}");
        assert!(line.contains(" after_entries=1 "), "{line}");
        assert!(!line.contains(" pause_entries="), "{line}");
    }
    assert!(
        said[2].starts_with("pass counts: passes=2 primary_204_seq=2 "),
        "{}",
        said[2]
    );
    assert!(
        said[3].starts_with("pass counts, last 2 passes: passes=2 primary_204_seq=2 "),
        "{}",
        said[3]
    );
    for line in &said[2..] {
        assert!(line.contains(" probe_peers=4 probe_bodies=2 "), "{line}");
        assert!(line.contains(" before_entries=2 "), "{line}");
        assert!(line.contains(" rest_entries=2 "), "{line}");
        assert!(line.contains(" after_entries=2 "), "{line}");
        assert!(line.contains(" pause_entries=1 "), "{line}");
    }
    at.stop();
}
