// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Whom a subscriber's gated pass asks ([`Gate`]), driven through the production pass against
//! vault servers on loopback, and when its sweeps come ([`Sweeps`]), on instants the tests
//! choose: counted, never timed.

use super::super::log_entries_tests::split_admins;
use super::super::pass_counts_tests::{Lab, lab, signed_from};
use super::*;
use crate::discover::DiscoveredVault;
use crate::pass_counts::PassCounts;
use std::sync::Arc;

/// One pass of the subscriber at `paths` over the peers `found`, gated by `gating` (none: as
/// every pass asked before the gate), counted alone: its counts and its outcome.
fn run(
    paths: &Paths,
    found: &[DiscoveredVault],
    repaired: &mut Repaired,
    gating: Option<Gating<'_>>,
) -> (Arc<PassCounts>, crate::Result<SyncOutcome>) {
    let found = |_: &Config| found.to_vec();
    let found = PeerDiscovery::supplied(&found);
    let counts = Arc::new(PassCounts::default());
    let metered = pass_counts::metered(&counts);
    let said = std::cell::Cell::new(SourceSaid::AtOnce);
    let passed =
        subscriber_pass_saying(paths, repaired, &found, &said, gating).map(|(_, outcome)| outcome);
    drop(metered);
    (counts, passed)
}

/// [`run`], gated by `sweeps`, its routes unmoved.
fn gated(
    paths: &Paths,
    found: &[DiscoveredVault],
    repaired: &mut Repaired,
    sweeps: &mut Sweeps,
) -> (Arc<PassCounts>, crate::Result<SyncOutcome>) {
    run(
        paths,
        found,
        repaired,
        Some(Gating {
            sweeps,
            moved: false,
        }),
    )
}

/// What `counts` holds of each of `what`, in order.
fn read(counts: &PassCounts, what: &[Count]) -> Vec<u64> {
    what.iter().map(|what| counts.get(*what)).collect()
}

/// A subscriber's sweeps at rest: its catch-up ran at `now`, and its next sweep is at least 12
/// minutes off.
fn resting(now: Instant) -> Sweeps {
    let mut sweeps = Sweeps::default();
    sweeps.caught_up(now, 0);
    sweeps
}

/// The URL of a port on loopback that nothing listens on: one a listener held and let go.
fn dead_url() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().expect("addr");
    drop(listener);
    format!("http://{addr}/v1/team")
}

/// The law of a resting gated pass: a member whose source and 3 of 4 peers hold the vault it
/// holds asks every peer on every pass without the gate (10 requests: the source's records and
/// its held request, and each peer's records and envelope). The gated loop's first pass catches
/// up, as many requests, but asks each peer only for a version newer than the member's, which
/// each holder answers with a `204` and no body; every pass after asks its source alone, 2
/// requests, and none of a peer. Routes that moved make the next pass a catch-up again.
///
/// Negative controls: `gate_ignored` (`Turn::SourceFirst` in `gated_pass` runs
/// `sync_found(.., Some(&Gate::CatchUp))`: the resting pass reads `[1, 10, 4, 4]`, wants
/// `[1, 2, 0, 0]`); `whole_probe` (`let probe_after = None;` in `sync_repairing_inner`: the
/// catch-up reads 3 bodies and no `204`, wants `[0, 3, 1]`).
#[test]
fn a_resting_gated_pass_asks_only_its_source() {
    let _answering = ServersAnswering::set();
    let at = lab(3);
    let mut repaired = None;
    let (counts, outcome) = run(&at.member, &at.found, &mut repaired, None);
    assert_eq!(outcome.expect("a pass"), SyncOutcome::Unchanged { seq: 2 });
    let asked = [
        Count::Requests,
        Count::RecordsPeers,
        Count::ProbePeers,
        Count::ProbeBodies,
    ];
    assert_eq!(read(&counts, &asked), [10, 4, 4, 3], "before the gate");

    let mut sweeps = Sweeps::default();
    let (counts, outcome) = gated(&at.member, &at.found, &mut repaired, &mut sweeps);
    assert_eq!(outcome.expect("a pass"), SyncOutcome::Unchanged { seq: 2 });
    let catching_up = [
        Count::CatchUps,
        Count::Requests,
        Count::RecordsPeers,
        Count::ProbePeers,
    ];
    assert_eq!(read(&counts, &catching_up), [1, 10, 4, 4], "the first pass");
    let answered = [Count::ProbeBodies, Count::ProbeNoNewer, Count::ProbeMissing];
    assert_eq!(
        read(&counts, &answered),
        [0, 3, 1],
        "no peer sends its body"
    );

    let resting = [
        Count::GatedPasses,
        Count::Requests,
        Count::RecordsPeers,
        Count::ProbePeers,
    ];
    for pass in 2..=3 {
        let (counts, outcome) = gated(&at.member, &at.found, &mut repaired, &mut sweeps);
        assert_eq!(outcome.expect("a pass"), SyncOutcome::Unchanged { seq: 2 });
        assert_eq!(read(&counts, &resting), [1, 2, 0, 0], "pass {pass}");
        assert_eq!(counts.get(Count::PrimarySeq204), 1, "pass {pass}");
    }

    let (counts, _) = run(
        &at.member,
        &at.found,
        &mut repaired,
        Some(Gating {
            sweeps: &mut sweeps,
            moved: true,
        }),
    );
    assert_eq!(read(&counts, &catching_up), [1, 10, 4, 4], "routes moved");
    at.stop();
}

/// An admin's gated pass reads each peer's whole envelope where it asks peers at all: its check
/// for heads that diverged at one seq (`heads_diverged`) needs each one, which a `204` at its seq
/// does not carry. The lab's admin, beside the member's `204`s, reads 3 bodies at its catch-up.
///
/// Negative control: `admin_after` (`let probe_after = gate.and_then(..)`, without the
/// `.filter(|_| !is_local_admin)`: the admin reads `[0, 3]`, wants `[3, 0]`).
#[test]
fn an_admins_gated_probe_still_reads_each_peers_head() {
    let _answering = ServersAnswering::set();
    let at = lab(3);
    let answered = [Count::ProbeBodies, Count::ProbeNoNewer];
    let mut sweeps = Sweeps::default();
    let (counts, outcome) = gated(&at.admin, &at.found, &mut None, &mut sweeps);
    assert_eq!(outcome.expect("a pass"), SyncOutcome::Unchanged { seq: 2 });
    assert_eq!(counts.get(Count::CatchUps), 1);
    assert_eq!(read(&counts, &answered), [3, 0], "the admin");
    let mut sweeps = Sweeps::default();
    let (counts, _) = gated(&at.member, &at.found, &mut None, &mut sweeps);
    assert_eq!(read(&counts, &answered), [0, 3], "the member");
    at.stop();
}

/// A source that does not answer leaves a resting pass to the servers found, for records and
/// for the envelope, as before the gate: one peer holds the next version, the source is gone,
/// and the pass applies the peer's version.
///
/// Negative control: `skip_peers_on_a_silent_source` (`Err(_) | Ok(Fetched::Missing) => false`
/// in `source_fell_short`: the pass ends `Err("… Connection refused …")`, wants
/// `Applied { seq: 3 }`).
#[test]
fn a_failing_source_still_fails_over_to_the_servers_found() {
    let _answering = ServersAnswering::set();
    let mut at = lab(3);
    let seed = admin_open(&at.admin).unwrap().seed;
    let head = load_state(&at.member).unwrap().digest.unwrap();
    let next = signed_from(&at.held, &seed, 1, head);
    transport::publish(&Source::Url(at.found[0].url.clone()), &next).expect("publish the next");
    at.source.take().expect("the source").stop();
    let mut sweeps = resting(Instant::now());
    let (counts, outcome) = gated(&at.member, &at.found, &mut None, &mut sweeps);
    assert!(
        matches!(outcome, Ok(SyncOutcome::Applied { seq: 3, .. })),
        "{outcome:?}"
    );
    let asked = [Count::GatedPasses, Count::RecordsPeers, Count::ProbePeers];
    assert_eq!(read(&counts, &asked), [1, 4, 4]);
    at.stop();
}

/// A source that fails is answered for by the servers found at this Mac's version, as before the
/// gate: the source is gone and the holders serve the member's version. A gated pass reads each
/// peer's whole envelope, since a `204` carries no head to stand for the source's answer, takes
/// the holders' head at the member's version, ends `Unchanged`, and keeps its network refresh
/// succeeded: the loop's first pass, a catch-up, and a resting pass after it alike.
///
/// Negative control: `after_on_a_silent_source` (no `admits_exact` filter on `probe_after` in
/// `sync_repairing_inner`: each holder answers a `204`, and the pass ends `Err("fetching …
/// Connection refused …")`, wants `Unchanged { seq: 2 }`).
#[test]
fn a_failing_source_is_answered_for_by_the_servers_at_its_version() {
    let _answering = ServersAnswering::set();
    let mut at = lab(3);
    at.source.take().expect("the source").stop();
    let config = configured(&at.member).unwrap();
    let mut sweeps = Sweeps::default();
    for (pass, turn) in [
        ("the first", Count::CatchUps),
        ("at rest", Count::GatedPasses),
    ] {
        let (counts, outcome) = gated(&at.member, &at.found, &mut None, &mut sweeps);
        assert_eq!(
            outcome.expect("a pass the servers answered"),
            SyncOutcome::Unchanged { seq: 2 },
            "{pass}"
        );
        let answered = [turn, Count::ProbeBodies, Count::ProbeNoNewer];
        assert_eq!(read(&counts, &answered), [1, 3, 0], "{pass}");
        let Observed::Read(receipts) = observe_receipts(&at.member, &config).unwrap() else {
            panic!("{pass}: the member's state lock is there and free");
        };
        let refresh = receipts.refresh.expect("a refresh retained");
        assert_eq!(
            refresh.outcome(),
            VaultNetworkRefreshOutcome::Succeeded,
            "{pass}"
        );
    }
    at.stop();
}

/// A source that answers but stays at an old version for good is overtaken at the sweep: five
/// peers hold the next version and the source holds the member's. Resting passes ask the source
/// alone and keep the member where it is; the sweep asks 4 of the 6 peers, the same 4 for
/// records and for the envelope, applies the next version, and leaves the next sweep a period
/// off.
///
/// Negative controls: `no_sweep` (`Turn::Sweep` in `gated_pass` runs the `Turn::SourceFirst`
/// arm: the sweep ends `Unchanged { seq: 2 }`); `sweep_asks_the_browse` (the sweep's
/// `sync_found` given `found`, not `&selected`: it reads `[1, 6, 6]`, wants `[1, 4, 4]`).
#[test]
fn a_frozen_source_is_overtaken_at_the_sweep() {
    let _answering = ServersAnswering::set();
    let at = lab(5);
    let seed = admin_open(&at.admin).unwrap().seed;
    let head = load_state(&at.member).unwrap().digest.unwrap();
    let next = signed_from(&at.held, &seed, 1, head);
    for peer in &at.found[..5] {
        transport::publish(&Source::Url(peer.url.clone()), &next).expect("publish the next");
    }
    let mut repaired = None;
    let mut sweeps = resting(Instant::now());
    for pass in 1..=2 {
        let (counts, outcome) = gated(&at.member, &at.found, &mut repaired, &mut sweeps);
        assert_eq!(outcome.expect("a pass"), SyncOutcome::Unchanged { seq: 2 });
        let asked = [Count::GatedPasses, Count::RecordsPeers, Count::ProbePeers];
        assert_eq!(read(&counts, &asked), [1, 0, 0], "pass {pass}");
    }
    sweeps.due = Some(Instant::now());
    let (counts, outcome) = gated(&at.member, &at.found, &mut repaired, &mut sweeps);
    assert!(
        matches!(outcome, Ok(SyncOutcome::Applied { seq: 3, .. })),
        "{outcome:?}"
    );
    let swept = [Count::Sweeps, Count::RecordsPeers, Count::ProbePeers];
    assert_eq!(read(&counts, &swept), [1, 4, 4]);
    assert_eq!(counts.get(Count::SweepUnanswered), 0);
    let later = Instant::now() + Duration::from_secs(60);
    assert_eq!(sweeps.turn(later, false), Turn::SourceFirst);
    at.stop();
}

/// A peer that does not answer keeps no sweep due: of the 4 peers a sweep asks, 3 listen on
/// nothing and one holds the vault, two more peers wait their turn in the period, and once the
/// one answered the next sweep is a period off, not [`SWEEP_RETRY`].
///
/// Negative control: `sweep_stays_due_on_any_failure` (`if answered == asked || !self.left` in
/// `Sweeps::swept`: the turn 6 s later is `Sweep`, wants `SourceFirst`).
#[test]
fn a_dead_name_does_not_keep_the_sweep_due() {
    let _answering = ServersAnswering::set();
    let at = lab(1);
    let peer = |url: String| DiscoveredVault { url, txt: None };
    let dead: Vec<_> = (0..3).map(|_| peer(dead_url())).collect();
    let waiting: Vec<_> = (0..2).map(|_| peer(dead_url())).collect();
    let found: Vec<_> = dead
        .iter()
        .chain([&at.found[0]])
        .chain(&waiting)
        .cloned()
        .collect();
    let mut sweeps = resting(Instant::now());
    let due = Instant::now();
    sweeps.due = Some(due);
    // Two peers this period has not tried, but tried before it: after those never tried.
    let before = due.checked_sub(Duration::from_secs(1)).expect("an instant");
    for waiting in &waiting {
        let key = transport::describe(&waiting.url).to_ascii_lowercase();
        sweeps.tried.insert(key, before);
    }
    let (counts, outcome) = gated(&at.member, &found, &mut None, &mut sweeps);
    assert_eq!(outcome.expect("a pass"), SyncOutcome::Unchanged { seq: 2 });
    let swept = [Count::Sweeps, Count::RecordsPeers, Count::SweepUnanswered];
    assert_eq!(read(&counts, &swept), [1, 4, 3]);
    let soon = Instant::now() + Duration::from_secs(6);
    assert_eq!(sweeps.turn(soon, false), Turn::SourceFirst);
    at.stop();
}

/// A sweep comes due [`crate::server::sweep_after`] after a catch-up, between 12 and 18 minutes
/// by the draw, and not before; routes that moved make any pass a catch-up; and before the
/// loop's first pass, nothing is due but a catch-up.
///
/// Negative control: `sweep_never` (`Some(due) if false` for `Some(due) if now >= due` in
/// `Sweeps::turn`: the turn at the due instant is `SourceFirst`, wants `Sweep`).
#[test]
fn the_sweep_comes_due_within_its_period() {
    let now = Instant::now();
    let milli = Duration::from_millis(1);
    let mut sweeps = Sweeps::default();
    assert_eq!(sweeps.turn(now, false), Turn::CatchUp);
    for (unit, minutes) in [(0, 12), (u64::MAX, 18)] {
        sweeps.caught_up(now, unit);
        let due = now + Duration::from_secs(minutes * 60);
        assert_eq!(
            sweeps.turn(due - milli, false),
            Turn::SourceFirst,
            "{minutes}"
        );
        assert_eq!(sweeps.turn(due - milli, true), Turn::CatchUp, "{minutes}");
        assert_eq!(sweeps.turn(due, false), Turn::Sweep, "{minutes}");
    }
}

/// A sweep no peer answers asks the next peers no sooner than [`SWEEP_RETRY`] (5 s) later, a
/// period that has tried every peer rests until the next whatever they answered, and the next
/// period starts with the peers tried longest ago: 10 peers, none answering, are asked 4, 4 and
/// 2 at a time, 5 s apart, then not until the next period.
///
/// Negative controls: `retry_without_spacing` (`SWEEP_RETRY` of `Duration::ZERO`: the turn 4 s
/// after a failed sweep is `Sweep`, wants `SourceFirst`); `sweep_never_rests` (`self.left =
/// true` in `Sweeps::choose`: the turn 5 s after the period's last peers is `Sweep`, wants
/// `SourceFirst`).
#[test]
fn a_sweep_no_peer_answers_asks_the_next_peers_soon_and_then_rests() {
    let keys: Vec<String> = (0..10).map(|n| format!("peer-{n}")).collect();
    let offer =
        || -> Vec<(String, String)> { keys.iter().map(|k| (k.clone(), k.clone())).collect() };
    let names = |range: std::ops::Range<usize>| keys[range].to_vec();
    let start = Instant::now();
    let mut sweeps = Sweeps::default();
    sweeps.caught_up(start, 0);
    let mut at = start + crate::server::sweep_after(0);
    for (chosen, last) in [(0..4, false), (4..8, false), (8..10, true)] {
        assert_eq!(sweeps.turn(at, false), Turn::Sweep);
        let asked = sweeps.choose(offer(), at, SWEEP_PEERS);
        assert_eq!(asked, names(chosen));
        sweeps.swept(at, asked.len(), 0, 0);
        let (sooner, retry) = (at + Duration::from_secs(4), at + Duration::from_secs(5));
        assert_eq!(sweeps.turn(sooner, false), Turn::SourceFirst);
        if last {
            assert_eq!(
                sweeps.turn(retry, false),
                Turn::SourceFirst,
                "the period rests"
            );
        } else {
            at = retry;
        }
    }
    let next = at + crate::server::sweep_after(0);
    assert_eq!(sweeps.turn(next, false), Turn::Sweep);
    assert_eq!(sweeps.choose(offer(), next, SWEEP_PEERS), names(0..4));
}

/// A period on a LAN of more names than a subscriber keeps tries for still ends: 300 names that
/// never answer are swept 4 at a time, [`SWEEP_RETRY`] apart, each asked once, until the period
/// has tried [`SWEEP_KEPT`] (256) of them, 64 sweeps; the period then rests until the next, which
/// starts with the names it did not reach.
///
/// Negative control: `evicts_the_period` (`Sweeps::choose` as step 12a had it: the oldest try goes
/// past 256 whenever it was made, and the period never limits itself, so a name tried in the
/// period is first in line again and the period never rests: the 100th sweep is still due).
#[test]
fn a_period_of_more_names_than_it_keeps_tries_for_ends() {
    let keys: Vec<String> = (0..300).map(|n| format!("peer-{n:03}")).collect();
    let offer =
        || -> Vec<(String, String)> { keys.iter().map(|k| (k.clone(), k.clone())).collect() };
    let start = Instant::now();
    let mut sweeps = Sweeps::default();
    sweeps.caught_up(start, 0);
    let mut at = start + crate::server::sweep_after(0);
    let mut asked = Vec::new();
    let mut swept = 0;
    while sweeps.turn(at, false) == Turn::Sweep {
        assert!(swept < 100, "the period never rests: {swept} sweeps");
        let chosen = sweeps.choose(offer(), at, SWEEP_PEERS);
        sweeps.swept(at, chosen.len(), 0, 0);
        asked.extend(chosen);
        swept += 1;
        at += SWEEP_RETRY;
    }
    assert_eq!(swept, SWEEP_KEPT / SWEEP_PEERS);
    assert_eq!(asked, keys[..SWEEP_KEPT], "each asked once, in turn");
    let next = at + crate::server::sweep_after(0);
    assert_eq!(sweeps.turn(next, false), Turn::Sweep);
    assert_eq!(
        sweeps.choose(offer(), next, SWEEP_PEERS),
        keys[SWEEP_KEPT..SWEEP_KEPT + SWEEP_PEERS],
        "the next period starts with the names this one did not reach"
    );
}

/// A stand-in server on loopback that hands each connection it accepts to `answer`, on a thread
/// of its own, with the head of the request read off it, until it is dropped.
struct StandIn {
    addr: std::net::SocketAddr,
    stop: Arc<std::sync::atomic::AtomicBool>,
}

impl StandIn {
    fn serving(answer: impl Fn(std::net::TcpStream, Vec<u8>) + Send + Sync + 'static) -> Self {
        use std::io::Read as _;
        use std::sync::atomic::Ordering;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().expect("addr");
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stopping = Arc::clone(&stop);
        let answer = Arc::new(answer);
        std::thread::spawn(move || {
            loop {
                let accepted = aterm_uds::accept(
                    &listener,
                    aterm_uds::AcceptWait::Forever,
                    aterm_uds::AcceptedMode::Blocking,
                );
                if stopping.load(Ordering::SeqCst) {
                    return;
                }
                let Ok((mut stream, _)) = accepted else {
                    continue;
                };
                let answer = Arc::clone(&answer);
                std::thread::spawn(move || {
                    let _ = stream.set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN));
                    let mut head = Vec::new();
                    let mut chunk = [0_u8; 1024];
                    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                        match stream.read(&mut chunk) {
                            Ok(0) | Err(_) => return,
                            Ok(n) => head.extend_from_slice(&chunk[..n]),
                        }
                    }
                    answer(stream, head);
                });
            }
        });
        StandIn { addr, stop }
    }

    /// Where it serves the lab's vault.
    fn url(&self) -> String {
        format!("http://{}/v1/team", self.addr)
    }

    /// A stand-in at an address of its own that reaches the server at `to` for the requests
    /// `reaches` says of (by the request's head) and answers every other with `otherwise`, a
    /// whole HTTP response.
    fn reaching(
        to: std::net::SocketAddr,
        reaches: fn(&[u8]) -> bool,
        otherwise: &'static [u8],
    ) -> Self {
        use std::io::Write as _;
        Self::serving(move |mut client, head| {
            if !reaches(&head) {
                let _ = client.write_all(otherwise);
                return;
            }
            let Ok(mut server) = std::net::TcpStream::connect(to) else {
                return;
            };
            if server.write_all(&head).is_err() {
                return;
            }
            let _ = std::io::copy(&mut server, &mut client);
        })
    }
}

impl Drop for StandIn {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        let _ = std::net::TcpStream::connect(self.addr);
    }
}

/// The address a lab's vault URL names.
fn addr_of(url: &str) -> std::net::SocketAddr {
    url.strip_prefix("http://")
        .and_then(|rest| rest.split('/').next())
        .and_then(|host| host.parse().ok())
        .expect("a lab URL")
}

/// Whether a request's head asks for the vault's records.
fn asks_records(head: &[u8]) -> bool {
    let line = head.split(|b| *b == b'\n').next().unwrap_or_default();
    line.windows(b"/records".len()).any(|w| w == b"/records")
}

/// A response that says this server holds no such vault.
const NO_VAULT: &[u8] = b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";

/// One sweep of the member at `paths` that asks `asked` (4 peers it never tried): its counts and
/// outcome, and whether the sweep finished. Two peers tried before its period wait behind them,
/// so a sweep none of the 4 answered asks the next peers [`SWEEP_RETRY`] later rather than rest.
fn one_sweep(
    paths: &Paths,
    asked: &[String],
) -> (Arc<PassCounts>, crate::Result<SyncOutcome>, bool) {
    let peer = |url: String| DiscoveredVault { url, txt: None };
    let waiting: Vec<_> = (0..2).map(|_| peer(dead_url())).collect();
    let found: Vec<_> = asked
        .iter()
        .cloned()
        .map(peer)
        .chain(waiting.iter().cloned())
        .collect();
    let due = Instant::now();
    let mut sweeps = resting(due);
    sweeps.due = Some(due);
    let before = due.checked_sub(Duration::from_secs(1)).expect("an instant");
    for waiting in &waiting {
        let key = transport::describe(&waiting.url).to_ascii_lowercase();
        sweeps.tried.insert(key, before);
    }
    let (counts, outcome) = gated(paths, &found, &mut None, &mut sweeps);
    assert_eq!(counts.get(Count::Sweeps), 1);
    let retry = Instant::now() + SWEEP_RETRY + Duration::from_secs(1);
    let finished = sweeps.turn(retry, false) == Turn::SourceFirst;
    (counts, outcome, finished)
}

/// A sweep is finished only by a peer that serves this vault and is not its source. Of 4 peers a
/// sweep asks, 3 listen on nothing, and the fourth is: a holder of the vault, which finishes it;
/// the source reached at another address (a holder's own label beside `alab-secrets` resolves to
/// the holder), whose records name the source's own boot id; a server that holds no such vault
/// (`404`) whose records requests reach a holder; and, once the source advanced, so the pass asks
/// no peer for the envelope, a server that holds no such vault (a stand-in that answers `404` to
/// every request, as such a server answers a caller on the LAN: the lab's own empty server serves
/// records to a caller on loopback as to its own Mac). None of the last three finishes the sweep,
/// so the next peers are asked [`SWEEP_RETRY`] later.
///
/// Negative controls: `source_answers` (`answered.is_ok()` for the records' note in
/// `records_pass`: the source at another address finishes the sweep); `missing_answers`
/// (`answer != Count::ProbeUnanswered` for the envelope's note in `probe_peers_for_sync`: the
/// `404` whose records reach a holder finishes it); `unserved_records` (`peer.boot.is_none() ||`
/// for `peer.boot.is_some() &&` in the records' note: the `404` after the source advanced
/// finishes it). Each fails in its scenario: `source_answers` at its count of unanswered peers
/// (3, wants 4), the others at `!finished`.
#[test]
fn only_a_server_of_the_vault_apart_from_its_source_finishes_a_sweep() {
    use std::io::Write as _;
    let _answering = ServersAnswering::set();
    let at = lab(1);
    let holder = at.found[0].url.clone();
    let (counts, outcome, finished) =
        one_sweep(&at.member, &[holder, dead_url(), dead_url(), dead_url()]);
    assert_eq!(outcome.expect("a sweep"), SyncOutcome::Unchanged { seq: 2 });
    assert_eq!(counts.get(Count::ProbeNoNewer), 1, "the holder's 204");
    assert!(finished, "a holder finishes the sweep");

    let source = at.source.as_ref().expect("the source").addr;
    let elsewhere = StandIn::reaching(source, |_| true, NO_VAULT);
    let (counts, outcome, finished) = one_sweep(
        &at.member,
        &[elsewhere.url(), dead_url(), dead_url(), dead_url()],
    );
    assert_eq!(outcome.expect("a sweep"), SyncOutcome::Unchanged { seq: 2 });
    assert_eq!(counts.get(Count::ProbeNoNewer), 1, "the source's 204");
    assert_eq!(counts.get(Count::SweepUnanswered), 4);
    assert!(!finished, "the source at another address");

    let records_only = StandIn::reaching(addr_of(&at.found[0].url), asks_records, NO_VAULT);
    let (counts, outcome, finished) = one_sweep(
        &at.member,
        &[records_only.url(), dead_url(), dead_url(), dead_url()],
    );
    assert_eq!(outcome.expect("a sweep"), SyncOutcome::Unchanged { seq: 2 });
    assert_eq!(counts.get(Count::ProbeMissing), 1, "its 404");
    assert!(
        !finished,
        "a server of no such vault, its records a holder's"
    );

    let seed = admin_open(&at.admin).unwrap().seed;
    let head = load_state(&at.member).unwrap().digest.unwrap();
    let next = signed_from(&at.held, &seed, 1, head);
    let source_url = format!("http://{source}/v1/team");
    transport::publish(&Source::Url(source_url), &next).expect("the source advances");
    let unheld = StandIn::serving(|mut stream, _| {
        let _ = stream.write_all(NO_VAULT);
    });
    let (counts, outcome, finished) = one_sweep(
        &at.member,
        &[unheld.url(), dead_url(), dead_url(), dead_url()],
    );
    assert!(
        matches!(outcome, Ok(SyncOutcome::Applied { seq: 3, .. })),
        "{outcome:?}"
    );
    assert_eq!(
        counts.get(Count::ProbePeers),
        0,
        "no envelope asked of a peer"
    );
    assert!(
        !finished,
        "a server of no such vault, after the source advanced"
    );
    drop((elsewhere, records_only, unheld));
    at.stop();
}

/// The lab's source again on the address its homes name, over a new store of its own (under
/// `store`) that holds `bytes` as the vault (none: no vault at all): a source that lost what it
/// held and was given something else.
fn source_holding(at: &mut Lab, store: &std::path::Path, bytes: Option<&[u8]>) {
    let source = at.source.take().expect("the source");
    let addr = source.addr;
    source.stop();
    at.source = Some(super::super::log_entries_tests::server_again(store, addr));
    if let Some(bytes) = bytes {
        let url = format!("http://{addr}/v1/team");
        transport::publish(&Source::Url(url), bytes).expect("seed the source");
    }
}

/// A source that falls short in what it says leaves a resting gated pass to the servers found,
/// one of which holds the version after this Mac's, and the pass applies it: where the source
/// names an older version than this Mac's (a `204` naming seq 1: it lost what it held and was
/// given an older copy, which this Mac would put back), holds no such vault (a `404`), or serves
/// a body another key signed (anyone on the LAN can answer at the source's name).
///
/// Negative controls, each an arm of `source_fell_short` made `false`: `put_back_unprobed`
/// (`Ok(Fetched::NoNewer(Some(_)))`), `missing_unprobed` (`Ok(Fetched::Missing)`) and
/// `forged_body_unprobed` (`Ok(Fetched::Body(_))`); each fails its case's `Applied { seq: 3 }`.
#[test]
fn a_source_that_falls_short_leaves_the_pass_to_the_servers_found() {
    let _answering = ServersAnswering::set();
    for case in ["older", "none", "forged"] {
        let mut at = lab(3);
        let seed = admin_open(&at.admin).unwrap().seed;
        let head = load_state(&at.member).unwrap().digest.unwrap();
        let next = signed_from(&at.held, &seed, 1, head);
        transport::publish(&Source::Url(at.found[0].url.clone()), &next).expect("the next");
        let held = envelope::parse(&at.held).unwrap();
        let signed = |seq, handoff, seed: &[u8; 32]| {
            let unsigned = envelope::Unsigned {
                name: held.name.clone(),
                seq,
                time: held.time,
                handoff,
                parents: Vec::new(),
                lockers: held.lockers.clone(),
            };
            envelope::sign(&unsigned, seed).unwrap()
        };
        let bytes = match case {
            "older" => Some(signed(1, held.handoff.clone(), &seed)),
            "none" => None,
            _ => Some(signed(9, Vec::new(), &[7; 32])),
        };
        let store = aterm_tempfile::tempdir().expect("tempdir");
        source_holding(&mut at, store.path(), bytes.as_deref());
        let mut sweeps = resting(Instant::now());
        let (counts, outcome) = gated(&at.member, &at.found, &mut None, &mut sweeps);
        assert!(
            matches!(outcome, Ok(SyncOutcome::Applied { seq: 3, .. })),
            "{case}: {outcome:?}"
        );
        let asked = [Count::GatedPasses, Count::ProbePeers];
        assert_eq!(read(&counts, &asked), [1, 4], "{case}");
        at.stop();
    }
}

/// A sweep's peer that answers the envelope but not the records did not answer the sweep: of the
/// 4 peers a sweep asks, one answers the envelope with a `204` at this Mac's version and every
/// records request with a `500`, and 3 listen on nothing; so the next peers are asked
/// [`SWEEP_RETRY`] later.
///
/// Negative control: `records_unheard` (no `heard.note` of the records' answers in
/// `records_pass`: the envelope's `204` alone answers the sweep, which counts 3 of its 4 peers
/// unanswered, wants 4).
#[test]
fn a_peer_that_fails_the_records_does_not_finish_a_sweep() {
    use std::io::Write as _;
    let _answering = ServersAnswering::set();
    let at = lab(0);
    let half = StandIn::serving(|mut stream, head| {
        let reply: &[u8] = if asks_records(&head) {
            b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        } else {
            b"HTTP/1.1 204 No Content\r\nETag: \"2\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        };
        let _ = stream.write_all(reply);
    });
    let (counts, outcome, finished) = one_sweep(
        &at.member,
        &[half.url(), dead_url(), dead_url(), dead_url()],
    );
    assert_eq!(outcome.expect("a sweep"), SyncOutcome::Unchanged { seq: 2 });
    assert_eq!(counts.get(Count::ProbeNoNewer), 1, "the envelope's 204");
    assert_eq!(counts.get(Count::SweepUnanswered), 4);
    assert!(!finished);
    drop(half);
    at.stop();
}

/// A pass catches up after the host's routes moved since the last began, after one that failed,
/// and after one whose source said nothing; not after one whose source answered, at once or held,
/// on routes that stayed.
///
/// Negative controls: `routes_ignored` (`let moved = self.owed;` in `Moves::before_pass`: the
/// pass on the moved routes reads `false`, wants `true`); `silence_ignored` (`self.owed =
/// failed;` in `Moves::after_pass`: the pass after a silent source reads `false`, wants `true`).
#[test]
fn a_pass_after_moved_routes_a_failure_or_a_silent_source_catches_up() {
    let here = [Some(std::net::IpAddr::from([10, 0, 0, 2])), None, None];
    let there = [Some(std::net::IpAddr::from([10, 0, 1, 2])), None, None];
    let mut moves = Moves::default();
    assert!(!moves.before_pass(here), "the first");
    for said in [SourceSaid::AtOnce, SourceSaid::Held] {
        moves.after_pass(false, said);
        assert!(!moves.before_pass(here), "{said:?}");
    }
    assert!(moves.before_pass(there), "the routes moved");
    moves.after_pass(false, SourceSaid::Held);
    assert!(!moves.before_pass(there), "they stayed");
    moves.after_pass(true, SourceSaid::Held);
    assert!(moves.before_pass(there), "a pass failed");
    moves.after_pass(false, SourceSaid::Nothing);
    assert!(moves.before_pass(there), "a source said nothing");
    moves.after_pass(false, SourceSaid::AtOnce);
    assert!(!moves.before_pass(there), "it answered again");
}

/// A subscriber keeps at most [`SWEEP_KEPT`] (256) peers' last tries, however many peers it is
/// offered: the one tried longest ago goes first, and is first in line again.
///
/// Negative control: `tries_unbounded` (no `while self.tried.len() > SWEEP_KEPT` loop in
/// `Sweeps::choose`: 300 kept, wants 256).
#[test]
fn the_tries_a_subscriber_keeps_are_bounded() {
    let offer =
        || -> Vec<(String, usize)> { (0..300).map(|n| (format!("peer-{n:03}"), n)).collect() };
    let start = Instant::now();
    let mut sweeps = Sweeps::default();
    let early = sweeps.choose(offer(), start, 44);
    assert_eq!(early, (0..44).collect::<Vec<_>>());
    let late = sweeps.choose(offer(), start + Duration::from_secs(1), 256);
    assert_eq!(late, (44..300).collect::<Vec<_>>(), "never tried first");
    assert_eq!(sweeps.tried.len(), SWEEP_KEPT);
    assert!(
        (0..44).all(|n| !sweeps.tried.contains_key(&format!("peer-{n:03}"))),
        "the tries made longest ago go first"
    );
}

/// A partition's heal reaches the admins at the sweep, with no probe of a peer on any resting
/// pass: two admins, each on its own server, edit apart (one once, one three times). Their
/// resting passes ask their own server alone and leave each server with its own side; two rounds
/// of sweeps, each admin asking the other's server, leave both serving both sides' edits.
///
/// Negative control: `no_sweep` (as in [`a_frozen_source_is_overtaken_at_the_sweep`]: `s1`
/// serves its own side's version, `other-key` still at `v0`, wants `v3-from-higher-seq`).
#[test]
fn a_partition_heal_reaches_an_admin_without_a_per_pass_probe() {
    let _answering = ServersAnswering::set();
    let lab = split_admins();
    let base_seq = load_state(&lab.st1).unwrap().seq;
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
    let peer = |url: &str| {
        vec![DiscoveredVault {
            url: url.to_owned(),
            txt: None,
        }]
    };
    let admins = [
        (&lab.st1, peer(&lab.url2), resting(Instant::now())),
        (&lab.st2, peer(&lab.url1), resting(Instant::now())),
    ];
    let mut admins = admins.map(|(paths, found, sweeps)| (paths, found, sweeps, None));
    for (paths, found, sweeps, repaired) in &mut admins {
        let (counts, _) = gated(paths, found, repaired, sweeps);
        let asked = [Count::GatedPasses, Count::RecordsPeers, Count::ProbePeers];
        assert_eq!(read(&counts, &asked), [1, 0, 0]);
    }
    let (_, view) = super::super::log_entries_tests::envelope_view(&lab.url1, &lab.m_0145_id);
    assert_eq!(
        view.get("other-key").map(Vec::as_slice),
        Some(b"v0".as_slice()),
        "no resting pass carries a side to the other's server"
    );
    for _ in 0..2 {
        for (paths, found, sweeps, repaired) in &mut admins {
            sweeps.due = Some(Instant::now());
            // What the sweep carried, the servers serve below.
            let _ = gated(paths, found, repaired, sweeps);
        }
    }
    for url in [&lab.url1, &lab.url2] {
        let (seq, view) = super::super::log_entries_tests::envelope_view(url, &lab.m_0145_id);
        assert_eq!(
            view.get("split-key").map(Vec::as_slice),
            Some(b"v1-from-lower-seq".as_slice()),
            "{url} serves seq {seq} without the lower-seq side's edit"
        );
        assert_eq!(
            view.get("other-key").map(Vec::as_slice),
            Some(b"v3-from-higher-seq".as_slice()),
            "{url} serves seq {seq} without the higher-seq side's edit"
        );
    }
    lab.s1.stop();
    lab.s2.stop();
}
