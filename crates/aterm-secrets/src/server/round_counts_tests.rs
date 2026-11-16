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

/// The label a holder's `alab-secrets` TXT names in these laws: its own-label instance's name,
/// which its server registers on the host `m9.local` ([`vault_advert`]).
const HOLDER_LABEL: &str = "m9";

/// [`server`], resolving `alab-secrets.local` and the holder's own-label host `m9.local`
/// ([`HOLDER_LABEL`]) to loopback and no other host.
fn server_resolving_holders(store: &Path, seq: u64) -> Running {
    let running = launch(
        TcpListener::bind("127.0.0.1:0").expect("bind"),
        &alab_options(store),
        Tuning {
            resolve: |host, _| {
                (host == "alab-secrets.local" || host.strip_suffix(".local") == Some(HOLDER_LABEL))
                    .then(|| vec![IpAddr::V4(Ipv4Addr::LOCALHOST)])
            },
            ..Tuning::default()
        },
    )
    .expect("started");
    publish(&running, seq);
    running
}

/// A vault server on loopback that pins the ALab vault to [`SIGNER`] and holds no version of
/// it yet: nothing has connected to it.
fn unpublished(store: &Path) -> Running {
    start(
        TcpListener::bind("127.0.0.1:0").expect("bind"),
        &alab_options(store),
    )
    .expect("started")
}

/// The options of a server on loopback storing at `store` that pins the ALab vault to
/// [`SIGNER`].
fn alab_options(store: &Path) -> Options {
    Options {
        listen: "127.0.0.1:0".parse().expect("addr"),
        store: store.to_path_buf(),
        advertise: None,
        pins: vec![(
            crate::ALAB_VAULT.to_owned(),
            signer_public(&SIGNER).expect("public"),
        )],
        hold: Duration::from_millis(100),
        keep_awake: false,
    }
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
    let decide: RoleDecider = Box::new(|_, _| Some("m5".to_owned()));
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
/// answers a probe, the pull is `pull`, and each round starts [`PULL_SPACING`] after the one
/// before, so no round's pulls are held back by the last's.
fn seams(watch: &Arc<Watch>, sharing: &Arc<AtomicBool>, pull: PullPeers) -> Seams {
    seams_probing(watch, sharing, pull, |_, _, _, _, _, _| None)
}

/// [`seams`], with `probe` the probe.
fn seams_probing(
    watch: &Arc<Watch>,
    sharing: &Arc<AtomicBool>,
    pull: PullPeers,
    probe: ProbeAlab,
) -> Seams {
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
        probe,
        pull,
        name_answered: Box::new(|_, _| false),
        sharing: Box::new(move || sharing.load(Ordering::SeqCst)),
        now: Box::new({
            let start = Instant::now();
            let mut rounds = 0u32;
            move || {
                rounds += 1;
                start + PULL_SPACING * rounds
            }
        }),
    }
}

/// A watch that heard `names` and `alab-secrets`, and resolves each of `names` to the vault at
/// the same place of `peers` (by its address, but for [`HOLDER_LABEL`], which it names by the
/// host its server registers it on, `m9.local`, at that address's port), its TXT naming the
/// `seq` at that place of `seqs` as it resolves and the records the servers here hold (none: the
/// empty log's digest), and `alab-secrets` to the holder at `holder` (by the host
/// `alab-secrets.local`, its TXT naming `seq` 5, the label [`HOLDER_LABEL`] and, as every
/// `alab-secrets` TXT, no records) or to none, counting the names it is handed in `handed`. As
/// the production resolver does, it starts one thread for each name it is handed, each run as a
/// round's resolve thread is ([`crate::serve_counts::resolving`]).
fn watching(
    bell: &Arc<Bell>,
    names: &[&str],
    peers: &[SocketAddr],
    seqs: &Arc<Vec<AtomicU64>>,
    handed: &Arc<AtomicU64>,
    holder: Option<SocketAddr>,
) -> Arc<Watch> {
    let at: Vec<(String, SocketAddr)> = names
        .iter()
        .map(|name| (*name).to_owned())
        .zip(peers.iter().copied())
        .collect();
    let seqs = Arc::clone(seqs);
    let handed = Arc::clone(handed);
    let watch = Watch::idle_resolving_vaults(bell, &[], move |asked, _| {
        handed.fetch_add(asked.len() as u64, Ordering::SeqCst);
        let resolving = crate::serve_counts::resolving();
        std::thread::scope(|scope| {
            for _ in asked {
                scope.spawn(|| resolving.run(|| ()));
            }
        });
        let txt = |seq, label: Option<&str>, rec| {
            Some(VaultTxt {
                vault: crate::ALAB_VAULT.to_owned(),
                signer: crate::envelope::fingerprint(&signer_public(&SIGNER).expect("public")),
                seq,
                head: [0; 32],
                label: label.map(str::to_owned),
                rec,
            })
        };
        let none_held = Some(crate::membership_log::empty_heads_digest());
        asked
            .iter()
            .filter_map(|name| {
                if let Some(holder) = holder.filter(|_| name == crate::alab_advertised()) {
                    return Some(DiscoveredVault {
                        url: format!(
                            "http://alab-secrets.local:{}/v1/{}",
                            holder.port(),
                            crate::ALAB_VAULT
                        ),
                        txt: txt(5, Some(HOLDER_LABEL), None),
                    });
                }
                let place = at.iter().position(|(known, _)| known == name)?;
                let (known, addr) = &at[place];
                let host = if known == HOLDER_LABEL {
                    format!("{known}.local:{}", addr.port())
                } else {
                    addr.to_string()
                };
                Some(DiscoveredVault {
                    url: format!("http://{host}/v1/{}", crate::ALAB_VAULT),
                    txt: txt(
                        seqs.get(place).map_or(1, |seq| seq.load(Ordering::SeqCst)),
                        None,
                        none_held,
                    ),
                })
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
/// vault servers on loopback. The first round, which sweeps each peer it sees for the first time
/// whatever its TXT says ([`PullGate::sweep_due`]), with its 3 peers holding the version this
/// server holds, counts 3
/// pulls, 3 envelope 204s, 3 records fetches, no body and nothing stored, the 4 names it handed
/// its resolver for peers and the 1 for the `alab-secrets` holders with the 5 threads that
/// resolved them, 1 probe for the ALab name (this server does not hold it, and looks for who
/// does once a round), and 3 `vault-peer-pull` threads ended. The rounds after it are gated
/// ([`pull_due`]): with one peer a version ahead and its TXT saying so, its envelope longer
/// than the first read's bound, the next round pulls that peer alone, counting 1 pull, 1
/// envelope asked a second time and read whole, 1 body, 1 envelope stored and 1 pull that
/// stored, and no records fetch (its TXT names the records this server holds); once every TXT
/// names what this server holds, a round makes no request at all (ATERM-KI-1108-2). Each round is counted by what ended the nap before it, a round that
/// finds sharing off pulls nothing, and the counts' line is said as the 1st, 2nd and 4th round
/// end.
///
/// Negative controls: delete `shared.counts.add(Count::Envelope204)` in `pull_from_peer`
/// (`envelope_204` is absent, wants 3); return 0 from a storing pull (delete the `stored += 1`
/// beside `Count::EnvelopesStored`: `pulls_stored` is absent, wants 1); spawn each pull without
/// `pull_threads.ending` (0 threads ended, wants 3); name a pull that pulls nothing in
/// `Seams::this_mac` (`pull: |_, _, _, _, _, _| 0`: `pulls`, `envelope_204` and
/// `records_fetches` are absent); delete `shared.counts.add(Count::EnvelopeWholeReads)` in
/// `pull_from_peer` (`envelope_whole_reads` is absent, wants 1); delete the
/// `serve_counts::tending` guard in `Tending::round` (0 resolve threads ended, wants 5); open
/// the gate (`pull_due` returning [`Due::BOTH`]: the second round pulls all 3 peers, `pulls`
/// 6, wants 4).
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
    let seqs: Arc<Vec<AtomicU64>> = Arc::new((0..3).map(|_| AtomicU64::new(5)).collect());
    let watch = watching(&bell, &["m10", "m11", "m12"], &addrs, &seqs, &handed, None);
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

    // One peer is a version ahead and its TXT says so, its envelope longer than a pull's first
    // read: the gated round pulls it alone, asks again, reads it whole and stores it.
    let longer = alab_holding(6, ENVELOPE_PEEK);
    assert!(longer.len() > ENVELOPE_PEEK);
    publish_bytes(&peers[1], &longer);
    seqs[1].store(6, Ordering::SeqCst);
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
            ("pulls", 4),
            ("pulls_stored", 1),
            ("envelope_whole_reads", 1),
            ("envelope_204", 3),
            ("envelope_bodies", 1),
            ("envelopes_stored", 1),
            ("records_fetches", 3),
            ("alab_probes", 2),
        ]
    );
    assert_eq!(threads(), 4);
    assert_eq!(resolves(), 10);
    assert!(second.said.is_some(), "the second round's line");

    // After the PUT that stored it, every TXT names what this server holds or older: the round
    // asks no peer anything.
    tending.why = Some(Wake::Put);
    let third = tending.round(&mut seams);
    assert!(third.said.is_none(), "no line as the third round ends");
    assert_eq!(counts.get(Count::NapPut), 1);
    assert_eq!(counts.get(Count::Pulls), 4, "an idle round pulls nothing");
    assert_eq!(counts.get(Count::Envelope204), 3);
    assert_eq!(counts.get(Count::RecordsFetches), 3);
    assert_eq!(counts.get(Count::EnvelopeBodies), 1);
    assert_eq!(counts.get(Count::EnvelopeWholeReads), 1);
    assert_eq!(counts.get(Count::PullsStored), 1);
    assert_eq!(counts.get(Count::AlabProbes), 3);
    assert_eq!(threads(), 4);

    // Sharing is off: the round resolves and pulls nothing.
    sharing.store(false, Ordering::SeqCst);
    tending.why = Some(Wake::Browse);
    let fourth = tending.round(&mut seams);
    assert!(fourth.said.is_some(), "the fourth round's line");
    assert_eq!(counts.get(Count::Rounds), 4);
    assert_eq!(counts.get(Count::NapBrowse), 1);
    assert_eq!(counts.get(Count::RoundsHeld), 1);
    assert_eq!(counts.get(Count::PeerNames), 12);
    assert_eq!(counts.get(Count::Pulls), 4);
    assert_eq!(counts.get(Count::AlabProbes), 3);
    assert_eq!(threads(), 4);
    assert_eq!(resolves(), 15);

    for server in peers.into_iter().chain([own]) {
        server.stop();
    }
}

/// What the pull arm of [`the_round_hands_its_pull_arm_every_peer_it_resolved`] was handed, a
/// round at a time: how many peers, and how many catch-ups the server's gate had counted
/// ([`PullGate::catch_up`]).
static HANDED_TO_THE_ARM: Mutex<Vec<(usize, u64)>> = Mutex::new(Vec::new());

fn recording_pull(
    shared: &Shared,
    peers: &[DiscoveredVault],
    _: u16,
    _: Option<&str>,
    _: &[Target],
    _: Pass,
) -> usize {
    let catch_ups = shared.lock_gate().catch_ups;
    HANDED_TO_THE_ARM
        .lock()
        .expect("handed")
        .push((peers.len(), catch_ups));
    0
}

/// Three rounds over a watch that resolves 5 names and a holder hand the pull arm 7 peers each
/// (the 5, and the holder once as a peer and once as a holder, whatever the round's cut of its
/// peers), the first a catch-up ([`CatchUp`]) and the others not, and count the 18 names for
/// peers and the 3 for holders that the resolver was handed, with no request made: the arm is a
/// test's, so the pulls the default arm counts stay 0. A round with sharing off hands the arm
/// nothing, and the first round with it on again catches up on what it did not pull meanwhile.
///
/// Negative controls: delete `counts.add_n(Count::PeerNames, peer_names)` in `Tending::round`
/// (`peer_names` reads 0, wants 18); delete the `serve_counts::tending` guard there (0 resolve
/// threads ended, wants 21); keep the catch-up state while sharing is off (drop
/// `self.catch_up = CatchUp::default()` in `Tending::round`: the round after it catches up
/// nothing, `(7, 1)` for `(7, 2)`); hand the arm the round's peers alone, without its holders
/// (`(6, 1)` for `(7, 1)`).
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
    let holder = SocketAddr::from(([127, 0, 0, 1], 14));
    let watch = watching(
        &bell,
        &names,
        &addrs,
        &Arc::new(Vec::new()),
        &handed,
        Some(holder),
    );
    let sharing = Arc::new(AtomicBool::new(true));
    let mut seams = seams(&watch, &sharing, recording_pull);
    let mut tending = tending(&own, &bell);
    for _ in 0..3 {
        assert!(!tending.round(&mut seams).closed);
        tending.why = Some(Wake::Timer);
    }
    assert_eq!(
        *HANDED_TO_THE_ARM.lock().expect("handed"),
        [(7, 1), (7, 1), (7, 1)]
    );
    let counts = &own.shared.counts;
    // Each round hands its resolver the 5 names and `alab-secrets` for peers, and
    // `alab-secrets` for holders.
    assert_eq!(counts.get(Count::PeerNames), 18);
    assert_eq!(counts.get(Count::AlabNames), 3);
    assert_eq!(handed.load(Ordering::SeqCst), 21);
    assert_eq!(counts.resolve_threads.ended.load(Ordering::Relaxed), 21);
    assert_eq!(counts.get(Count::Pulls), 0);
    assert_eq!(counts.get(Count::NapTimer), 2);
    sharing.store(false, Ordering::SeqCst);
    assert!(!tending.round(&mut seams).closed);
    sharing.store(true, Ordering::SeqCst);
    assert!(!tending.round(&mut seams).closed);
    assert_eq!(
        HANDED_TO_THE_ARM.lock().expect("handed")[3..],
        [(7, 2)],
        "sharing on again: a catch-up"
    );
    own.stop();
}

/// Where [`probing_holder`] finds the holder.
static PROBED_HOLDER: Mutex<Option<SocketAddr>> = Mutex::new(None);

/// A probe of `alab-secrets` made as the production probe makes one ([`probe_verified`]), at the
/// holder [`PROBED_HOLDER`] names.
fn probing_holder(
    target: &str,
    _: u16,
    _: Option<&str>,
    _: &[Target],
    _: bool,
    shared: &Shared,
) -> Option<u64> {
    let at = (*PROBED_HOLDER.lock().expect("holder"))?;
    if target != crate::alab_advertised() {
        return None;
    }
    probe_verified(&format!("http://{at}/v1/{}", crate::ALAB_VAULT), shared)
}

/// An idle lab with a holder (ATERM-KI-1108-2): through the production round and pull, against
/// a holder and a peer on loopback holding what this server holds, the first round sweeps both,
/// and each round after it asks the holder one conditional request, the probe that looks for who
/// holds `alab-secrets` (`?after=<seq>`, answered 204), and nothing else: no pull of the holder,
/// whose `alab-secrets` TXT names what this server holds, its label `m9` and no records, and
/// whose own-label instance (`m9`, on the host `m9.local` its label names, in the round beside
/// it) names the records this server holds, nor of the peer.
///
/// NEGATIVE CONTROL (2026-11-15): with no `alab-secrets` exception in `pull_due`
/// (`None => true`), the rec-less `alab-secrets` TXT read as an older build's, each idle round
/// asks the holder its records too (`left: (3, 2)`, `right: (3, 1)`).
#[test]
fn an_idle_round_asks_the_holder_nothing_beyond_its_probe() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let own = server_resolving_holders(&root.path().join("own"), 5);
    let holder = server(&root.path().join("holder"), 5);
    let peer = server(&root.path().join("peer"), 5);
    *PROBED_HOLDER.lock().expect("holder") = Some(holder.addr);
    let bell = Arc::new(Bell::default());
    let handed = Arc::new(AtomicU64::new(0));
    let seqs: Arc<Vec<AtomicU64>> = Arc::new(vec![AtomicU64::new(5), AtomicU64::new(5)]);
    // `m9` is the holder's own-label instance, resolving to the endpoint its `alab-secrets`
    // instance does: one group, whose records `m9`'s TXT makes due.
    let watch = watching(
        &bell,
        &["m10", HOLDER_LABEL],
        &[peer.addr, holder.addr],
        &seqs,
        &handed,
        Some(holder.addr),
    );
    let sharing = Arc::new(AtomicBool::new(true));
    let mut seams = seams_probing(&watch, &sharing, Seams::this_mac().pull, probing_holder);
    let mut tending = tending(&own, &bell);
    let counts = Arc::clone(&own.shared.counts);
    let asked = |server: &Running| (server.envelope_gets(), server.records_gets());

    // The first round sweeps both, then probes the holder, reading its version whole.
    assert!(!tending.round(&mut seams).closed);
    assert_eq!(counts.get(Count::Pulls), 2);
    assert_eq!(asked(&peer), (1, 1));
    assert_eq!(
        asked(&holder),
        (2, 1),
        "its sweep and the probe's whole read"
    );

    for round in 2..=3 {
        tending.why = Some(Wake::Timer);
        assert!(!tending.round(&mut seams).closed);
        assert_eq!(
            asked(&holder),
            (round + 1, 1),
            "round {round}: the probe alone"
        );
        assert_eq!(asked(&peer), (1, 1), "round {round}");
    }
    assert_eq!(
        counts.get(Count::Pulls),
        2,
        "the idle rounds pulled nothing"
    );
    assert_eq!(counts.get(Count::AlabProbes), 3);
    for server in [own, holder, peer] {
        server.stop();
    }
}

/// What the holder's own-label instance `m9` of
/// [`a_holders_own_label_instance_out_of_the_batch_gates_its_records_for_a_sweep_period`]
/// resolves to.
#[derive(Clone, Copy, Debug)]
enum OwnLabel {
    /// A TXT naming the ALab vault and the records the servers there hold.
    Records,
    /// A TXT naming the ALab vault and no `rec` (an older build's).
    NoRec,
    /// Nothing: it answers no resolve, or answers with a TXT naming another vault, which a
    /// round's resolve leaves out as the production one does (`resolve_instances` keeps only
    /// what `resolved_vault(event, ALAB_VAULT)` answers, `None` for another vault:
    /// `discover_tests::browse_and_resolve_read_active_instances_and_order_txt_hints_behind_defaults`).
    Unresolved,
}

/// A lab whose browse hears more names than a round resolves (ATERM-KI-1108-2): beside 30 other
/// names, `alab-secrets` and its holder's own-label instance `m9` (on the host `m9.local`, the
/// one its `alab-secrets` TXT's label names, [`PullGate::names_rec`]), so a round's rotating
/// batch of 16 ([`crate::discover::MAX_CANDIDATES`]) holds `m9` in every other round, while the
/// holders come every round. Through the production round and pull against the holder on
/// loopback, holding what this server holds, past the first round's sweep (the holder's next
/// sweep held off), its `alab-secrets` TXT naming no records: after a round that resolved `m9`
/// naming those records, the next round, whose batch leaves it out, asks the holder nothing;
/// with `m9` resolving to nothing (it answers no resolve, or names another vault, which the
/// round's resolve leaves out), a round within [`NAMED_FOR`] of the last that resolved it
/// naming the records asks nothing and each from then on asks the holder its records; and once
/// a round resolves `m9` naming no `rec`, the rounds after it without `m9` ask the records too.
/// The holder is asked no envelope past the sweep.
///
/// NEGATIVE CONTROLS (2026-11-15), each the holder's (envelope, records) GETs: with no memory
/// past the round deciding (`note_named` clearing what earlier rounds noted, the group rule of
/// 876a62565c2), the round whose batch leaves `m9` out asks the records (`left: (1, 2)`,
/// `right: (1, 1)`); with no bound (`note_named` keeping what it noted whatever its age), the
/// round at [`NAMED_FOR`] asks nothing (`left: (1, 1)`, `right: (1, 2)`); with no forget
/// (`note_named` keeping what an earlier round noted of an instance this round resolved), and
/// with no rec check (`note_named` noting a TXT naming none), the round after the one resolving
/// `m9` naming no `rec`, whose batch leaves it out, asks nothing (`left: (1, 4)`,
/// `right: (1, 5)`); the round resolving it asks the records whatever was noted, since that
/// TXT, an older build's, makes them due itself.
#[test]
fn a_holders_own_label_instance_out_of_the_batch_gates_its_records_for_a_sweep_period() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let own = server_resolving_holders(&root.path().join("own"), 5);
    let holder = server(&root.path().join("holder"), 5);
    let bell = Arc::new(Bell::default());
    let m9 = Arc::new(Mutex::new(OwnLabel::Records));
    let watch = Watch::idle_resolving_vaults(&bell, &[], {
        let m9 = Arc::clone(&m9);
        let at = holder.addr;
        move |asked, _| {
            let txt = |rec| {
                Some(VaultTxt {
                    vault: crate::ALAB_VAULT.to_owned(),
                    signer: crate::envelope::fingerprint(&signer_public(&SIGNER).expect("public")),
                    seq: 5,
                    head: [0; 32],
                    label: Some(HOLDER_LABEL.to_owned()),
                    rec,
                })
            };
            asked
                .iter()
                .filter_map(|name| {
                    if name == crate::alab_advertised() {
                        return Some(DiscoveredVault {
                            url: format!(
                                "http://alab-secrets.local:{}/v1/{}",
                                at.port(),
                                crate::ALAB_VAULT
                            ),
                            txt: txt(None),
                        });
                    }
                    if name != HOLDER_LABEL {
                        return None;
                    }
                    let rec = match *m9.lock().expect("m9") {
                        OwnLabel::Records => Some(crate::membership_log::empty_heads_digest()),
                        OwnLabel::NoRec => None,
                        OwnLabel::Unresolved => return None,
                    };
                    // By its own host, which resolves to the endpoint `alab-secrets.local` does:
                    // one group.
                    Some(DiscoveredVault {
                        url: format!(
                            "http://{HOLDER_LABEL}.local:{}/v1/{}",
                            at.port(),
                            crate::ALAB_VAULT
                        ),
                        txt: txt(rec),
                    })
                })
                .collect()
        }
    });
    // Sorted, `alab-secrets` and `m9` come first: each odd round's batch holds them.
    let others: Vec<String> = (0..30).map(|n| format!("p{n:02}")).collect();
    for name in others
        .iter()
        .map(String::as_str)
        .chain([HOLDER_LABEL, crate::alab_advertised()])
    {
        watch.hear(true, 4, name);
    }
    let watch = Arc::new(watch);
    let sharing = Arc::new(AtomicBool::new(true));
    let mut seams = seams(&watch, &sharing, Seams::this_mac().pull);
    let t0 = Instant::now();
    let clock = Arc::new(Mutex::new(t0));
    seams.now = Box::new({
        let clock = Arc::clone(&clock);
        move || *clock.lock().expect("clock")
    });
    let mut tending = tending(&own, &bell);
    let mut round = |at: Instant| {
        *clock.lock().expect("clock") = at;
        assert!(!tending.round(&mut seams).closed);
        tending.why = Some(Wake::Timer);
    };
    let asked = || (holder.envelope_gets(), holder.records_gets());
    // The holder's next sweep comes `sweep_after(0)` past `at`: the rounds before it are gated.
    let sweep_held = |at: Instant| own.shared.lock_gate().swept(&[holder.addr], at, 0);
    let set = |to: OwnLabel| *m9.lock().expect("m9") = to;

    round(t0);
    assert_eq!(asked(), (1, 1), "the first round's sweep, m9 in its batch");
    sweep_held(t0);
    round(t0 + PULL_SPACING);
    assert_eq!(asked(), (1, 1), "a batch without m9, after one with it");
    let noted = t0 + PULL_SPACING * 2;
    round(noted);
    assert_eq!(asked(), (1, 1), "m9 in the batch again");

    set(OwnLabel::Unresolved);
    sweep_held(noted + NAMED_FOR - Duration::from_secs(2));
    round(noted + NAMED_FOR - Duration::from_secs(1));
    assert_eq!(
        asked(),
        (1, 1),
        "within NAMED_FOR of the round that last resolved m9 naming the records"
    );
    let mut at = noted + NAMED_FOR;
    round(at);
    assert_eq!(asked(), (1, 2), "at NAMED_FOR: the records");
    at += PULL_SPACING;
    round(at);
    assert_eq!(asked(), (1, 3), "and each round after");

    // The next round is odd: its batch holds `m9`, the one after it does not.
    set(OwnLabel::Records);
    at += PULL_SPACING;
    round(at);
    at += PULL_SPACING;
    round(at);
    assert_eq!(asked(), (1, 3), "m9 naming the records, then out");
    set(OwnLabel::NoRec);
    at += PULL_SPACING;
    round(at);
    assert_eq!(
        asked(),
        (1, 4),
        "m9 naming no rec, which makes them due itself"
    );
    at += PULL_SPACING;
    round(at);
    assert_eq!(asked(), (1, 5), "then out, after naming no rec");
    own.stop();
    holder.stop();
}

/// A first round that resolves no peer (its browse heard nothing yet, as every server's first
/// round after a fleet-wide update does) leaves each server to be swept by the first round that
/// resolves it, there a later round's (step 6 spent its catch-up on the empty round).
///
/// NEGATIVE CONTROL (2026-11-15): with a server first seen swept never (`sweep_due` false where
/// no sweep of it finished), the round that first resolves the peers pulls neither
/// (`left: 0`, `right: 2`).
#[test]
fn a_server_first_heard_after_an_empty_first_round_is_swept() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let own = server(&root.path().join("own"), 5);
    let peers: Vec<Running> = (0..2)
        .map(|n| server(&root.path().join(format!("peer{n}")), 5))
        .collect();
    let addrs: Vec<SocketAddr> = peers.iter().map(|peer| peer.addr).collect();
    let bell = Arc::new(Bell::default());
    let handed = Arc::new(AtomicU64::new(0));
    let seqs: Arc<Vec<AtomicU64>> = Arc::new((0..2).map(|_| AtomicU64::new(5)).collect());
    let names = ["m10", "m11"];
    let watch = watching(&bell, &names, &addrs, &seqs, &handed, None);
    for name in names {
        watch.hear(false, 4, name);
    }
    let sharing = Arc::new(AtomicBool::new(true));
    let mut seams = seams(&watch, &sharing, Seams::this_mac().pull);
    let mut tending = tending(&own, &bell);
    let counts = Arc::clone(&own.shared.counts);
    assert!(!tending.round(&mut seams).closed);
    assert_eq!(
        counts.get(Count::Pulls),
        0,
        "the first round resolved no peer"
    );
    for name in names {
        watch.hear(true, 4, name);
    }
    tending.why = Some(Wake::Browse);
    assert!(!tending.round(&mut seams).closed);
    assert_eq!(
        counts.get(Count::Pulls),
        2,
        "each swept as it is first resolved"
    );
    for peer in &peers {
        assert_eq!((peer.envelope_gets(), peer.records_gets()), (1, 1));
    }
    for server in peers.into_iter().chain([own]) {
        server.stop();
    }
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
        pull: |_, _, _, _, _, _| 0,
        now: Box::new(Instant::now),
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
