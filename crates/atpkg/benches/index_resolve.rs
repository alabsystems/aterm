// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The index-resolve pricing instrument — what a no-op `atpkg update` pass spends on the
//! network before it decides there is nothing to do.
//!
//! # Why this bench exists (and why it is not criterion)
//!
//! atpkg had no benchmark at all, so the cost of resolving the signed index was argued
//! from the source rather than measured, and the source hid it well: `index_candidates`
//! is one listing request plus FOUR asset downloads per candidate, capped at
//! `INDEX_CANDIDATE_CAP = 4` — SIXTEEN sequential `curl` subprocesses, each with its own
//! DNS+TLS handshake — and the §14 `IndexCache` that already held those exact bytes was
//! consulted only when the fetch had already FAILED. A cache that is never a hit path is
//! invisible to every functional test, which is precisely why this counts round-trips as
//! well as timing them.
//!
//! What is saved is ROUND-TRIPS, not rate-limit budget — say it precisely or not at all.
//! Only the listing is an `api.github.com` request; the sixteen asset reads already go to
//! the release CDN (`release_download_base`, unmetered — `pkg_manifest`'s comment records
//! measuring HTTP 200 from it with the API at `remaining: 0`). So this removes sixteen
//! DNS+TLS+HTTP round-trips and leaves the one metered request exactly where it was.
//!
//! It deliberately brings NO dependency. atpkg's whole reason for existing is supply
//! chain: its manifest argues every edge (`ring` promoted to a direct dependency on
//! purpose, a matching `deny.toml` floor-ban, `default-features = false` so the verify
//! path pulls no alloc), and adding criterion to earn a prettier histogram would widen
//! that surface for a workload dominated by injected sleeps, where a sampling harness
//! measures nothing criterion is good at. `harness = false` + an explicit loop reports
//! exactly the two numbers that matter: round-trips, and wall time.
//!
//! # The model
//!
//! [`LatencyFetcher`] is `net::GithubFetcher`'s cost shape, not its logic: one memoized
//! listing request, then four asset reads per candidate, each charged `--rtt`. The
//! default 70 ms is the value measured on the author's machine against
//! objects.githubusercontent.com (warm; a cold handshake measured ~440 ms), so the
//! headline row prices reality rather than a convenient number. `ATPKG_BENCH_RTT_MS=0`
//! isolates the LOCAL cost — the identity probe, the cache read, the base64 decode —
//! which is reported on its own row because a saving in round-trips paid for with local
//! work should have to show its books.
//!
//! # Reach guards (both directions)
//!
//! A bench that silently stops exercising its subject reports a beautiful number forever.
//! So every scenario asserts the request counts it is supposed to produce: the COLD pass
//! must really spend 1 listing + 16 asset reads (if it does not, the fixture has drifted
//! and the "before" is fiction), and the WARM pass must really spend 0 asset reads (if it
//! does not, the hit path is not engaging and the "after" is fiction). A published
//! release must put the fetch back — a cache that keeps answering after the source moved
//! would be a downgrade oracle, not an optimization.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use atpkg::{Anchor, BuildFloor, Candidate, Fetcher, Layout};

/// Releases carrying a complete authorization quad that the fetcher offers, i.e. the
/// candidates `INDEX_CANDIDATE_CAP` admits. Four is the shipped cap.
const CANDIDATES: usize = 4;
/// Assets downloaded per candidate: `index.toml`, `index.toml.sig`, the roster, its sig.
const ASSETS_PER_CANDIDATE: usize = 4;
/// The real channel's size, from `aterm-update-core`'s own measurement of
/// `alabsystems/aterm` (2026-08-20): 42 releases / 200 assets on page 1. It fits in ONE
/// `per_page=100` page, which is why the listing here is one request and why this bench
/// does not pretend release count is the scaling variable — the candidate cap is.
const REAL_CHANNEL_RELEASES: usize = 42;

/// A fetcher with `net::GithubFetcher`'s cost shape: one memoized release listing, then
/// four asset reads per candidate, each charged a round-trip.
struct LatencyFetcher {
    rtt: Duration,
    /// Listing requests actually issued (the memo makes this 1 per process, as in `net`).
    requests: AtomicU64,
    /// Asset bodies actually downloaded — the number this whole finding is about.
    assets: AtomicU64,
    /// `net::GithubFetcher::releases` memoizes per process; so does this.
    listed: Mutex<bool>,
    /// The candidate identities the "listing" reports. Mutating this models a publish.
    identities: Mutex<Vec<String>>,
}

impl LatencyFetcher {
    fn new(rtt: Duration) -> Self {
        Self {
            rtt,
            requests: AtomicU64::new(0),
            assets: AtomicU64::new(0),
            listed: Mutex::new(false),
            identities: Mutex::new(
                (0..CANDIDATES)
                    .map(|i| format!("idx-{i}-asset-ids"))
                    .collect(),
            ),
        }
    }

    /// The release listing: one request per process, memoized exactly as `releases_at`
    /// memoizes, so the identity probe and the download loop share it and the probe can
    /// never ADD a round-trip.
    fn listing(&self) {
        let mut listed = self.listed.lock().expect("bench is single-threaded");
        if *listed {
            return;
        }
        *listed = true;
        self.requests.fetch_add(1, Ordering::Relaxed);
        std::thread::sleep(self.rtt);
    }

    fn counts(&self) -> (u64, u64) {
        (
            self.requests.load(Ordering::Relaxed),
            self.assets.load(Ordering::Relaxed),
        )
    }

    /// Publish: every candidate's assets move, which is what a new index release looks
    /// like from the listing.
    fn publish(&self) {
        let mut ids = self.identities.lock().expect("bench is single-threaded");
        for (i, id) in ids.iter_mut().enumerate() {
            *id = format!("idx-{i}-asset-ids-v2");
        }
    }
}

impl Fetcher for LatencyFetcher {
    fn index_candidates(&self) -> Result<Vec<Candidate>, String> {
        self.listing();
        let count = self
            .identities
            .lock()
            .expect("bench is single-threaded")
            .len();
        let mut out = Vec::with_capacity(count);
        for i in 0..count {
            for _ in 0..ASSETS_PER_CANDIDATE {
                self.assets.fetch_add(1, Ordering::Relaxed);
                std::thread::sleep(self.rtt);
            }
            // Unsigned filler of roughly the real document sizes: the live published
            // index on the author's machine is 1058 bytes and a detached Ed25519
            // signature is 64. Selection refuses all of it under the inert anchor below,
            // which is fine and deliberate — this bench prices the FETCH, which happens
            // before any of that, and pricing it with a real signing fixture would only
            // add key generation to every iteration.
            out.push(Candidate {
                label: format!("atpkg-index-{i}"),
                index_bytes: vec![b'#'; 1058],
                sig: vec![0u8; 64],
                roster_bytes: vec![b'#'; 512],
                roster_sig: vec![0u8; 64],
            });
        }
        Ok(out)
    }

    fn pkg_manifest(&self, _: &str, _: &str, _: u64) -> Result<(Vec<u8>, Vec<u8>), String> {
        Err("bench fetches no manifests".to_string())
    }

    fn download(&self, _: &str, _: &str, _: &Path) -> Result<(), String> {
        Err("bench downloads no artifacts".to_string())
    }

    fn source_id(&self) -> String {
        "github:bench/aterm".to_string()
    }

    fn index_identities(&self) -> Option<Vec<String>> {
        self.listing();
        Some(
            self.identities
                .lock()
                .expect("bench is single-threaded")
                .clone(),
        )
    }
}

/// One resolve, timed. The `Err` is the point of neither: an inert anchor refuses every
/// candidate, so both the cold and the warm pass end identically and the only difference
/// between them is how many round-trips they spent getting there.
fn resolve(fetcher: &LatencyFetcher, layout: &Layout) -> (Duration, bool) {
    let anchor = Anchor::of(Vec::new(), 0);
    let t = Instant::now();
    let outcome = atpkg::resolve_verified_index(fetcher, layout, &anchor, BuildFloor::none(), 0);
    (t.elapsed(), outcome.is_err())
}

fn scratch(label: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("atpkg-bench-index-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("bench scratch dir");
    d
}

/// One round's results: the three wall times, and the round-trip counts OBSERVED for
/// each pass.
///
/// The counts are carried out of the round rather than written into the report as
/// literals. That is not tidiness: a headline the reader believes ("17 -> 1") must be
/// derived from the same counters the reach guards assert on, or the table would keep
/// printing the old claim after the behaviour it describes stopped being true — a bench
/// asserting its conclusion instead of measuring it.
struct Round {
    cold: Duration,
    warm: Duration,
    after: Duration,
    /// Round-trips a COLD pass costs a fresh process: the listing + every asset.
    cold_trips: u64,
    /// Round-trips a WARM pass costs a fresh process. The listing memo is per-process,
    /// so a new process still pays `requests` for the listing that answers the cheap
    /// identity probe — plus whatever assets the pass actually downloaded (0 on a hit).
    warm_trips: u64,
    /// Round-trips the pass AFTER a publish costs: the listing + the re-downloaded set.
    after_trips: u64,
}

/// Cold resolve, then warm resolve, then a resolve after a publish — in ONE process, so
/// the fetcher's listing memo behaves exactly as `net::GithubFetcher`'s does.
fn one_round(rtt: Duration, label: &str) -> Round {
    let dir = scratch(label);
    let layout = Layout {
        prefix: dir.join("prefix"),
    };
    let f = LatencyFetcher::new(rtt);

    let (cold, cold_err) = resolve(&f, &layout);
    let (req, assets) = f.counts();
    assert!(cold_err, "an inert anchor must refuse every candidate");
    assert_eq!(req, 1, "the listing is ONE request (memoized), got {req}");
    assert_eq!(
        assets,
        (CANDIDATES * ASSETS_PER_CANDIDATE) as u64,
        "REACH GUARD: the cold pass must really download every candidate's quad — \
         if it does not, this bench is no longer pricing the thing it claims to"
    );

    let (warm, warm_err) = resolve(&f, &layout);
    let (req2, assets2) = f.counts();
    assert!(warm_err, "the warm pass must reach the same verdict");
    assert_eq!(req2, 1, "the listing memo still holds; no second request");
    assert_eq!(
        assets2, assets,
        "REACH GUARD: the warm pass must download NOTHING — a matching identity means \
         the bytes on disk are provably the bytes the source is publishing"
    );

    // A publish must put the fetch back. Without this the "win" above would be
    // indistinguishable from a cache that had simply stopped listening.
    f.publish();
    let (after, after_err) = resolve(&f, &layout);
    let (_, assets3) = f.counts();
    assert!(after_err, "same verdict again");
    assert_eq!(
        assets3 - assets2,
        (CANDIDATES * ASSETS_PER_CANDIDATE) as u64,
        "REACH GUARD: a moved identity must re-download the whole candidate set"
    );

    let _ = std::fs::remove_dir_all(&dir);
    Round {
        cold,
        warm,
        after,
        // Every figure below comes from the SAME atomics the reach guards above assert
        // on, so the printed headline cannot drift away from the asserted behaviour.
        cold_trips: req + assets,
        warm_trips: req + (assets2 - assets),
        after_trips: req + (assets3 - assets2),
    }
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn main() {
    // 70 ms is the MEASURED warm round-trip to objects.githubusercontent.com on the
    // author's machine (a cold handshake measured ~440 ms). Overridable so a run on a
    // different link can price its own reality.
    let measured_rtt: u64 = std::env::var("ATPKG_BENCH_RTT_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(70);

    println!("atpkg index-resolve — one no-op update pass");
    println!(
        "  model: {CANDIDATES} candidates x {ASSETS_PER_CANDIDATE} assets + 1 listing; \
         real channel = {REAL_CHANNEL_RELEASES} releases (one page)"
    );
    println!();
    println!(
        "  injected RTT   cold        warm        after publish   round-trips \
         (cold -> warm -> after)"
    );
    println!(
        "  ------------   ---------   ---------   -------------   \
         ------------------------------"
    );

    // The local row IS the `measured_rtt == 0` row, so asking for RTT=0 explicitly must
    // print one row, not the same row twice.
    let rtts: Vec<u64> = if measured_rtt == 0 {
        vec![0]
    } else {
        vec![measured_rtt, 0]
    };
    for rtt_ms in rtts {
        let rtt = Duration::from_millis(rtt_ms);
        let label = if rtt_ms == 0 { "local" } else { "rtt" };
        // Three rounds, report the median: process spawn jitter is not the subject.
        let mut rounds: Vec<Round> = (0..3)
            .map(|i| one_round(rtt, &format!("{label}-{i}")))
            .collect();
        rounds.sort_by_key(|r| r.cold);
        let r = &rounds[1];
        println!(
            "  {rtt_ms:>4} ms       {:>7.1}ms   {:>7.1}ms   {:>9.1}ms       {} -> {} -> {}",
            ms(r.cold),
            ms(r.warm),
            ms(r.after),
            r.cold_trips,
            r.warm_trips,
            r.after_trips
        );
        if rtt_ms == 0 {
            println!(
                "                 ^ LOCAL ONLY: the warm row here is the cost the hit path \
                 ADDS (cache read + base64), with no round-trip to save."
            );
        }
    }
    println!();
    println!(
        "  A no-op pass does no other network work (`decide` is pure and fetches no \
         manifests when every member is UpToDate), so the warm row is the whole pass."
    );
}
