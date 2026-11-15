// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What a server counts of its rounds and its pulls ([`crate::serve_counts`]), driven through
//! the production round ([`Tending::round`]) and the production pull against vault servers on
//! loopback: counted, never timed.

use super::*;
use crate::discover::{DiscoveredVault, Watch};
use crate::envelope::{Locker, Unsigned, sign, signer_public, test_armor};
use std::net::TcpListener;
use std::sync::atomic::AtomicU64;

const HANG_DETECTOR: Duration = Duration::from_secs(60);
const SIGNER: [u8; 32] = [7u8; 32];

/// The ALab vault's envelope of `seq`.
fn alab(seq: u64) -> Vec<u8> {
    alab_holding(seq, 40)
}

/// The ALab vault's envelope of `seq`, its admin locker `held` bytes long.
fn alab_holding(seq: u64, held: usize) -> Vec<u8> {
    let unsigned = Unsigned {
        name: crate::ALAB_VAULT.to_owned(),
        seq,
        time: 1_792_530_000 + seq,
        handoff: Vec::new(),
        parents: Vec::new(),
        lockers: vec![
            Locker {
                label: "admin".to_owned(),
                armored: test_armor(&vec![seq as u8; held]),
            },
            Locker {
                label: "1".to_owned(),
                armored: test_armor(b"everyone"),
            },
        ],
    };
    sign(&unsigned, &SIGNER).expect("signed")
}

/// A vault server on loopback that pins the ALab vault to [`SIGNER`] and holds `seq` of it.
fn server(store: &Path, seq: u64) -> Running {
    let running = unpublished(store);
    publish(&running, seq);
    running
}

/// A vault server on loopback that pins the ALab vault to [`SIGNER`] and holds no version of
/// it yet: nothing has connected to it.
fn unpublished(store: &Path) -> Running {
    let options = Options {
        listen: "127.0.0.1:0".parse().expect("addr"),
        store: store.to_path_buf(),
        advertise: None,
        pins: vec![(
            crate::ALAB_VAULT.to_owned(),
            signer_public(&SIGNER).expect("public"),
        )],
        hold: Duration::from_millis(100),
        keep_awake: false,
    };
    start(TcpListener::bind("127.0.0.1:0").expect("bind"), &options).expect("started")
}

fn publish(running: &Running, seq: u64) {
    publish_bytes(running, &alab(seq));
}

fn publish_bytes(running: &Running, envelope: &[u8]) {
    let url = format!("http://{}/v1/{}", running.addr, crate::ALAB_VAULT);
    crate::transport::publish(&crate::transport::Source::Url(url), envelope).expect("stored");
}

/// What member `m5`'s `vault-tend` thread keeps, before its first round: it advertises for
/// `own`, under the name `m5`.
fn tending(own: &Running, bell: &Arc<Bell>) -> Tending {
    let decide: RoleDecider = Box::new(|_| Some("m5".to_owned()));
    Tending::begin(
        Arc::default(),
        Arc::new(AtomicBool::new(false)),
        Arc::clone(bell),
        false,
        Some((
            decide,
            vec!["m5".to_owned()],
            own.addr.port(),
            Arc::clone(&own.shared),
        )),
        None,
        None,
        Arc::clone(&own.shared.counts),
    )
}

/// A round's seams in these laws: the standing browse is `watch`, sharing is what `sharing`
/// says, this Mac has no address (so a round registers no advert and starts no child), nothing
/// answers a probe, and the pull is `pull`.
fn seams(watch: &Arc<Watch>, sharing: &Arc<AtomicBool>, pull: PullPeers) -> Seams {
    let watch = Arc::clone(watch);
    let sharing = Arc::clone(sharing);
    Seams {
        routes: None,
        browse: Some(Box::new(move |_, _| Arc::clone(&watch))),
        targets: Box::new(Vec::new),
        spawn: Box::new(|_, _, _, _| Err(io::Error::other("these laws register no advert"))),
        every: Duration::from_secs(3600),
        power: Box::new(|_| None),
        end_for_sleep: Arc::new(Registered::end),
        sleep_bound: SLEEP_YIELD_BOUND,
        probe: |_, _, _, _, _, _| None,
        pull,
        name_answered: Box::new(|_, _| false),
        sharing: Box::new(move || sharing.load(Ordering::SeqCst)),
    }
}

/// A watch that heard `names` and `alab-secrets`, and resolves each of `names` to the vault at
/// the same place of `peers` (`alab-secrets` to none), counting the names it is handed in
/// `handed`. As the production resolver does, it starts one thread for each name it is handed,
/// each run as a round's resolve thread is ([`crate::serve_counts::resolving`]).
fn watching(
    bell: &Arc<Bell>,
    names: &[&str],
    peers: &[SocketAddr],
    handed: &Arc<AtomicU64>,
) -> Arc<Watch> {
    let at: Vec<(String, SocketAddr)> = names
        .iter()
        .map(|name| (*name).to_owned())
        .zip(peers.iter().copied())
        .collect();
    let handed = Arc::clone(handed);
    let watch = Watch::idle_resolving_vaults(bell, &[], move |asked, _| {
        handed.fetch_add(asked.len() as u64, Ordering::SeqCst);
        let resolving = crate::serve_counts::resolving();
        std::thread::scope(|scope| {
            for _ in asked {
                scope.spawn(|| resolving.run(|| ()));
            }
        });
        asked
            .iter()
            .filter_map(|name| at.iter().find(|(known, _)| known == name))
            .map(|(_, addr)| DiscoveredVault {
                url: format!("http://{addr}/v1/{}", crate::ALAB_VAULT),
                txt: Some(VaultTxt {
                    vault: crate::ALAB_VAULT.to_owned(),
                    signer: crate::envelope::fingerprint(&signer_public(&SIGNER).expect("public")),
                    seq: 1,
                    head: [0; 32],
                    label: None,
                }),
            })
            .collect()
    });
    for name in names.iter().chain(&[crate::alab_advertised()]) {
        watch.hear(true, 4, name);
    }
    Arc::new(watch)
}

/// Every count of `counts` that is not 0, by name.
fn counted(counts: &ServeCounts) -> Vec<(&'static str, u64)> {
    Count::ALL
        .iter()
        .map(|what| (what.name(), counts.get(*what)))
        .filter(|(_, n)| *n != 0)
        .collect()
}

/// The law of a server's round counts, through the production round and the pull arm a real
/// server's rounds are given (the one [`Seams::this_mac`] names, taken from it here) against 3
/// vault servers on loopback. A round whose 3 peers hold the version this server holds counts
/// 3 pulls, 3 envelope 204s, 3 records fetches, no body and nothing stored, the 4 names it
/// handed its resolver for peers and the 1 for the `alab-secrets` holders with the 5 threads
/// that resolved them, 1 probe for the ALab name (this server does not hold it, and looks for
/// who does once a round), and 3 `vault-peer-pull` threads ended. With one peer a version
/// ahead, its envelope longer than the first read's bound, the next round counts 1 envelope
/// asked a second time and read whole, 1 body, 1 envelope stored and 1 pull that stored. Each
/// round is counted by what ended the nap before it, a round that finds sharing off pulls
/// nothing, and the counts' line is said as the 1st, 2nd and 4th round end.
///
/// Negative controls: delete `shared.counts.add(Count::Envelope204)` in `pull_from_peer`
/// (`envelope_204` is absent, wants 3); return 0 from a storing pull (delete the `stored += 1`
/// beside `Count::EnvelopesStored`: `pulls_stored` is absent, wants 1); spawn each pull without
/// `pull_threads.ending` (0 threads ended, wants 3); name a pull that pulls nothing in
/// `Seams::this_mac` (`pull: |_, _, _, _, _| 0`: `pulls`, `envelope_204` and `records_fetches`
/// are absent); delete `shared.counts.add(Count::EnvelopeWholeReads)` in `pull_from_peer`
/// (`envelope_whole_reads` is absent, wants 1); delete the `serve_counts::tending` guard in
/// `Tending::round` (0 resolve threads ended, wants 5).
#[test]
fn a_round_counts_its_names_its_pulls_and_what_each_peer_answered() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let own = server(&root.path().join("own"), 5);
    let peers: Vec<Running> = (0..3)
        .map(|n| server(&root.path().join(format!("peer{n}")), 5))
        .collect();
    let addrs: Vec<SocketAddr> = peers.iter().map(|peer| peer.addr).collect();
    let bell = Arc::new(Bell::default());
    let handed = Arc::new(AtomicU64::new(0));
    let watch = watching(&bell, &["m10", "m11", "m12"], &addrs, &handed);
    let sharing = Arc::new(AtomicBool::new(true));
    // The arm a real server's rounds pull through: nothing else of this Mac's own is used.
    let mut seams = seams(&watch, &sharing, Seams::this_mac().pull);
    let mut tending = tending(&own, &bell);
    let counts = Arc::clone(&own.shared.counts);
    let threads = || counts.pull_threads.ended.load(Ordering::Relaxed);
    let resolves = || counts.resolve_threads.ended.load(Ordering::Relaxed);

    // The first round: every peer holds what this server holds.
    let first = tending.round(&mut seams);
    assert!(!first.closed);
    let at_rest = vec![
        ("rounds", 1),
        ("round_first", 1),
        ("peer_names", 4),
        ("alab_names", 1),
        ("pulls", 3),
        ("envelope_204", 3),
        ("records_fetches", 3),
        ("alab_probes", 1),
    ];
    assert_eq!(counted(&counts), at_rest);
    assert_eq!(threads(), 3);
    // The resolver was handed what the round counted: 4 names for peers and 1 for holders,
    // and each thread it started for one was counted as it ended.
    assert_eq!(handed.load(Ordering::SeqCst), 5);
    assert_eq!(resolves(), 5);
    let line = first.said.expect("the first round's line");
    assert!(
        line.starts_with(
            "round counts: rounds=1 round_first=1 peer_names=4 alab_names=1 pulls=3 \
             envelope_204=3 records_fetches=3 alab_probes=1 "
        ),
        "{line}"
    );
    assert!(
        line.contains(" resolve_threads=5 ") && line.contains(" pull_threads=3 "),
        "{line}"
    );
    assert!(line.contains(" process_cpu_ns="), "{line}");

    // One peer is a version ahead, its envelope longer than a pull's first read: it is asked
    // again, read whole and stored.
    let longer = alab_holding(6, ENVELOPE_PEEK);
    assert!(longer.len() > ENVELOPE_PEEK);
    publish_bytes(&peers[1], &longer);
    tending.why = Some(Wake::Timer);
    let second = tending.round(&mut seams);
    assert_eq!(
        counted(&counts),
        [
            ("rounds", 2),
            ("round_first", 1),
            ("nap_timer", 1),
            ("peer_names", 8),
            ("alab_names", 2),
            ("pulls", 6),
            ("pulls_stored", 1),
            ("envelope_whole_reads", 1),
            ("envelope_204", 5),
            ("envelope_bodies", 1),
            ("envelopes_stored", 1),
            ("records_fetches", 6),
            ("alab_probes", 2),
        ]
    );
    assert_eq!(threads(), 6);
    assert_eq!(resolves(), 10);
    assert!(second.said.is_some(), "the second round's line");

    // After the PUT that stored it, every peer holds nothing newer.
    tending.why = Some(Wake::Put);
    let third = tending.round(&mut seams);
    assert!(third.said.is_none(), "no line as the third round ends");
    assert_eq!(counts.get(Count::NapPut), 1);
    assert_eq!(counts.get(Count::Envelope204), 8);
    assert_eq!(counts.get(Count::EnvelopeBodies), 1);
    assert_eq!(counts.get(Count::EnvelopeWholeReads), 1);
    assert_eq!(counts.get(Count::PullsStored), 1);
    assert_eq!(counts.get(Count::AlabProbes), 3);

    // Sharing is off: the round resolves and pulls nothing.
    sharing.store(false, Ordering::SeqCst);
    tending.why = Some(Wake::Browse);
    let fourth = tending.round(&mut seams);
    assert!(fourth.said.is_some(), "the fourth round's line");
    assert_eq!(counts.get(Count::Rounds), 4);
    assert_eq!(counts.get(Count::NapBrowse), 1);
    assert_eq!(counts.get(Count::RoundsHeld), 1);
    assert_eq!(counts.get(Count::PeerNames), 12);
    assert_eq!(counts.get(Count::Pulls), 9);
    assert_eq!(counts.get(Count::AlabProbes), 3);
    assert_eq!(threads(), 9);
    assert_eq!(resolves(), 15);

    for server in peers.into_iter().chain([own]) {
        server.stop();
    }
}

/// What the pull arm of [`the_round_hands_its_pull_arm_every_peer_it_resolved`] was handed, a
/// round at a time.
static HANDED_TO_THE_ARM: Mutex<Vec<usize>> = Mutex::new(Vec::new());

fn recording_pull(
    _: &Shared,
    peers: &[DiscoveredVault],
    _: u16,
    _: Option<&str>,
    _: &[Target],
) -> usize {
    HANDED_TO_THE_ARM.lock().expect("handed").push(peers.len());
    0
}

/// Three rounds over a watch that resolves 5 names hand the pull arm 5 peers each, and count
/// the 15 names for peers and the 3 for holders that the resolver was handed, with no request
/// made: the arm is a test's, so the pulls the default arm counts stay 0.
///
/// Negative controls: delete `counts.add_n(Count::PeerNames, peer_names)` in `Tending::round`
/// (`peer_names` reads 0, wants 18); delete the `serve_counts::tending` guard there (0 resolve
/// threads ended, wants 21).
#[test]
fn the_round_hands_its_pull_arm_every_peer_it_resolved() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let own = server(&root.path().join("own"), 5);
    let names = ["m10", "m11", "m12", "m13", "m14"];
    // No server listens at these: the arm asks none.
    let addrs: Vec<SocketAddr> = (0..5)
        .map(|n| SocketAddr::from(([127, 0, 0, 1], 9 + n)))
        .collect();
    let bell = Arc::new(Bell::default());
    let handed = Arc::new(AtomicU64::new(0));
    let watch = watching(&bell, &names, &addrs, &handed);
    let sharing = Arc::new(AtomicBool::new(true));
    let mut seams = seams(&watch, &sharing, recording_pull);
    let mut tending = tending(&own, &bell);
    for _ in 0..3 {
        assert!(!tending.round(&mut seams).closed);
        tending.why = Some(Wake::Timer);
    }
    assert_eq!(*HANDED_TO_THE_ARM.lock().expect("handed"), [5, 5, 5]);
    let counts = &own.shared.counts;
    // Each round hands its resolver the 5 names and `alab-secrets` for peers, and
    // `alab-secrets` for holders.
    assert_eq!(counts.get(Count::PeerNames), 18);
    assert_eq!(counts.get(Count::AlabNames), 3);
    assert_eq!(handed.load(Ordering::SeqCst), 21);
    assert_eq!(counts.resolve_threads.ended.load(Ordering::Relaxed), 21);
    assert_eq!(counts.get(Count::Pulls), 0);
    assert_eq!(counts.get(Count::NapTimer), 2);
    own.stop();
}

/// Each `vault-conn` thread is counted as it ends: a server tells this law as each of its
/// connection threads ends, counted by then, and once it has told of as many as it admitted
/// connections, that many threads are counted. The wait is for each thread's own end.
///
/// Negative control: run `handle` without `conn_threads.ending` in the `vault-conn` thread
/// (every thread ends and none is counted: `left: 0`, `right:` the connections admitted).
#[test]
fn each_connection_thread_is_counted_as_it_ends() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let running = unpublished(&root.path().join("store"));
    let (ended, ends) = std::sync::mpsc::channel();
    *running.shared.conn_ended.lock().expect("the seam") = Some(ended);
    publish(&running, 5);
    publish(&running, 6);
    // Each publish was answered, so every connection it made was admitted before now.
    let served = running.shared.served.load(Ordering::SeqCst) as u64;
    assert!(served >= 2, "{served} connections");
    for _ in 0..served {
        ends.recv_timeout(HANG_DETECTOR)
            .expect("never: each connection's thread ended");
    }
    assert_eq!(
        running
            .shared
            .counts
            .conn_threads
            .ended
            .load(Ordering::Relaxed),
        served
    );
    running.stop();
}

/// A `vault-tend` thread that advertises for no server (one that only keeps this Mac awake, as
/// the system job of a host with no label runs) counts its rounds into the counts of the
/// server its children were made for ([`Children::counting`], as [`serve`] makes them): the
/// counts that server's connections count into, so the line it says holds both. The thread is
/// the production one ([`Children::tend_role_seamed`]); the law hears its first round begin,
/// which is after that round was counted.
///
/// Negative control: count into fresh counts where the thread advertises for none
/// (`Arc::default()` for `Arc::clone(&self.counts)` in `tend_role_seamed`: `left: 0`,
/// `right: 1`).
#[test]
fn a_tend_thread_that_advertises_nothing_counts_into_its_servers_counts() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let own = server(&root.path().join("own"), 5);
    let children = Children::counting(&own.shared.counts);
    let (began, begun) = std::sync::mpsc::channel();
    let seams = Seams {
        routes: None,
        browse: None,
        targets: Box::new(Vec::new),
        spawn: Box::new(|_, _, _, _| Err(io::Error::other("this law registers no advert"))),
        every: Duration::from_secs(3600),
        power: Box::new(|_| None),
        end_for_sleep: Arc::new(Registered::end),
        sleep_bound: SLEEP_YIELD_BOUND,
        probe: |_, _, _, _, _, _| None,
        pull: |_, _, _, _, _| 0,
        name_answered: Box::new(|_, _| false),
        // A round reads this once it has counted itself.
        sharing: Box::new(move || {
            let _ = began.send(());
            true
        }),
    };
    children.tend_role_seamed(false, None, None, seams);
    begun
        .recv_timeout(HANG_DETECTOR)
        .expect("never: the tend thread's first round began");
    let counts = &own.shared.counts;
    assert_eq!(counts.get(Count::Rounds), 1);
    assert_eq!(counts.get(Count::RoundFirst), 1);
    drop(children);
    own.stop();
}
