// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Count a membership-log records file by kind and time what a member reads of it, or sign a
//! synthetic log in the live ALab log's shape and write its records file, to time it the same way.
//!
//! `log_census <records file> [vault]` reads the file (a copy of a Mac's `records`, or one `build`
//! wrote) into the store `vault` keeps: `alab` by default, whose store takes only the compiled
//! ALab genesis; a synthetic log is read as any other vault's, whose store takes any genesis.
//! It times each step once (a repeated `membership()` as the mean of 1,000 calls) and prints a
//! `time <step> <ms>` line for it and `rss <step> <bytes>` lines for the process's peak resident
//! memory so far (`getrusage`'s `ru_maxrss`), then counts the records. Run it once per
//! measurement, so every first read is a fresh process's. No network; a records file holds no
//! key or secret value, only ciphertext nothing here opens.
//!
//! `log_census build <shape> <out file>` signs a log of `shape` (`live`, `keyed` or `full`:
//! [`LIVE`], [`KEYED`], [`FULL`]) and writes its records file. Every key it signs with is derived
//! from a fixed label: none is a live identity.

use aterm_digest::Sha256;
use aterm_secrets::age::Identity;
use aterm_secrets::commons::binding::Request;
use aterm_secrets::membership_log::{
    Body, Draft, Entrant, EpochAudience, Evidence, Inserted, Kind, Log, MemberId, Record, RecordId,
    Store, decode_records, encode_records, node_proof_message,
};
use ring::signature::{Ed25519KeyPair, KeyPair};
use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [build, shape, out] if build == "build" => build_log(shape, out),
        [path] => read(path, "alab"),
        [path, vault] => read(path, vault),
        _ => Err(
            "usage: log_census <records file> [vault] | log_census build <shape> <out file>".into(),
        ),
    }
}

/// The process's peak resident memory so far, in bytes: `getrusage`'s `ru_maxrss`, which macOS
/// counts in bytes and Linux in KiB.
#[cfg(any(
    target_os = "macos",
    all(target_os = "linux", target_pointer_width = "64")
))]
fn peak_rss() -> Option<u64> {
    /// `struct rusage` on a 64-bit target: two 16-byte `timeval`s, then fourteen `long`s, the
    /// first of them `ru_maxrss`.
    #[repr(C)]
    struct Rusage {
        times: [i64; 4],
        maxrss: i64,
        rest: [i64; 13],
    }
    unsafe extern "C" {
        fn getrusage(who: std::ffi::c_int, usage: *mut Rusage) -> std::ffi::c_int;
    }
    const RUSAGE_SELF: std::ffi::c_int = 0;
    let mut usage = Rusage {
        times: [0; 4],
        maxrss: 0,
        rest: [0; 13],
    };
    // SAFETY: `usage` is writable for one `struct rusage` in this target's layout, and the call
    // does not retain the pointer.
    if unsafe { getrusage(RUSAGE_SELF, &raw mut usage) } != 0 {
        return None;
    }
    let maxrss = u64::try_from(usage.maxrss).ok()?;
    Some(if cfg!(target_os = "macos") {
        maxrss
    } else {
        maxrss * 1024
    })
}

#[cfg(not(any(
    target_os = "macos",
    all(target_os = "linux", target_pointer_width = "64")
)))]
fn peak_rss() -> Option<u64> {
    None
}

fn rss(step: &str) {
    match peak_rss() {
        Some(bytes) => println!("rss {step} {bytes}"),
        None => println!("rss {step} unavailable"),
    }
}

/// Run `step` once and print its wall time.
fn timed<T>(step: &str, run: impl FnOnce() -> T) -> T {
    let start = Instant::now();
    let out = run();
    println!("time {step} {:.3}", start.elapsed().as_secs_f64() * 1e3);
    out
}

fn audience(a: &EpochAudience) -> String {
    match a {
        EpochAudience::Everyone => "everyone".into(),
        EpochAudience::Commons => "commons".into(),
        EpochAudience::Hosts(_) => "hosts".into(),
    }
}

/// Time what a member reads of the records file at `path`, then count its records.
fn read(path: &str, vault: &str) -> Result<()> {
    rss("start");
    let bytes = timed("read", || std::fs::read(path))?;
    let store = timed("recover", || Store::recover(vault, &bytes))?;
    rss("recover");
    let log = store.log().ok_or("the store holds no log")?;
    println!(
        "held {} waiting {} footprint {}",
        log.held_count(),
        log.waiting(),
        log.footprint()
    );
    let membership = timed("membership_first", || log.membership());
    timed("membership_again", || log.membership());
    let repeats = 1_000;
    let start = Instant::now();
    for _ in 0..repeats {
        std::hint::black_box(log.membership());
    }
    println!(
        "time membership_repeated_each {:.6}",
        start.elapsed().as_secs_f64() * 1e3 / f64::from(repeats)
    );
    println!("members {}", membership.members().len());
    rss("membership");
    let members: Vec<MemberId> = membership.members().keys().copied().collect();
    for (name, audience) in [
        ("everyone", EpochAudience::Everyone),
        ("commons", EpochAudience::Commons),
    ] {
        for pass in ["first", "again"] {
            let out = timed(&format!("left_out_{name}_{pass}"), || {
                members
                    .iter()
                    .filter(|member| log.audience_left_out(&audience, member))
                    .count()
            });
            println!("left_out {name} {pass} {out} of {}", members.len());
        }
    }
    let keyed = timed("keyed_epochs_first", || log.keyed_epochs());
    timed("keyed_epochs_again", || log.keyed_epochs());
    let dues = timed("rekey_dues_first", || log.rekey_dues());
    timed("rekey_dues_again", || log.rekey_dues());
    let newest = timed("newest_everyone", || {
        log.newest_epochs(&EpochAudience::Everyone)
    });
    let removed = timed("removed_recipients", || log.removed_recipients());
    println!(
        "keyed_epochs {} rekey_dues {} newest_everyone {} removed_recipients {}",
        keyed.len(),
        dues.len(),
        newest.len(),
        removed.len()
    );
    rss("end");
    census(&bytes, log)
}

/// Count the records `bytes` holds by kind, audience, signer and parent edge, and `log`'s heads.
fn census(bytes: &[u8], log: &Log) -> Result<()> {
    let records = decode_records(bytes)?;
    let mut kinds: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    let mut signers: BTreeMap<Kind, BTreeSet<MemberId>> = BTreeMap::new();
    let mut epochs: BTreeMap<RecordId, String> = BTreeMap::new();
    let mut wraps: BTreeMap<RecordId, usize> = BTreeMap::new();
    let mut puts: BTreeMap<String, usize> = BTreeMap::new();
    let (mut edges, mut widest) = (0, 0);
    for r in &records {
        let mut count = |key: String| {
            let entry = kinds.entry(key).or_default();
            entry.0 += 1;
            entry.1 += r.bytes().len();
        };
        count(format!("{:?}", r.kind()));
        edges += r.parents().len();
        widest = widest.max(r.parents().len());
        if r.kind() != Kind::Genesis {
            signers.entry(r.kind()).or_default().insert(*r.actor());
        }
        match r.body() {
            Body::Genesis { founders, .. } => {
                let bound = founders.iter().filter(|f| f.binding.is_some()).count();
                println!("genesis founders {} bound {bound}", founders.len());
            }
            Body::Admit { evidence, .. } => count(format!("Admit {evidence:?}")),
            Body::Epoch { audience: a, .. } => {
                epochs.insert(*r.id(), audience(a));
            }
            Body::Wrap { epoch, .. } => *wraps.entry(*epoch).or_default() += 1,
            Body::Put { audience: a, .. } => *puts.entry(audience(a)).or_default() += 1,
            _ => {}
        }
    }
    let weighed = records.iter().filter(|r| r.kind().weighed()).count();
    println!(
        "file bytes {} records {} weighed {weighed} keyed {}",
        bytes.len(),
        records.len(),
        records.len() - weighed
    );
    for (k, (n, b)) in &kinds {
        println!(
            "  {k:13} {n:6} records {b:10} bytes (avg {})",
            b / n.max(&1)
        );
    }
    let all: BTreeSet<MemberId> = signers.values().flatten().copied().collect();
    let by_kind: Vec<String> = signers
        .iter()
        .map(|(k, s)| format!("{k:?} {}", s.len()))
        .collect();
    println!("signers {} ({})", all.len(), by_kind.join(", "));
    println!(
        "parent edges {edges} widest {widest} heads {}",
        log.heads().len()
    );
    println!("puts by audience {puts:?}");
    let mut by_audience: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (epoch, a) in &epochs {
        by_audience
            .entry(a)
            .or_default()
            .push(wraps.get(epoch).copied().unwrap_or(0));
    }
    for (a, w) in &mut by_audience {
        w.sort_unstable();
        println!(
            "  epochs of {a}: {} wraps {} per epoch min {} median {} max {}",
            w.len(),
            w.iter().sum::<usize>(),
            w[0],
            w[w.len() / 2],
            w[w.len() - 1]
        );
    }
    Ok(())
}

/// What a synthetic log holds: its counts by kind, and how the live log spreads them.
#[derive(Clone, Copy, Debug)]
struct Mix {
    /// Macs the genesis binds.
    founders: usize,
    admits: usize,
    removes: usize,
    /// `Node` records: one per Mac until every Mac has one, then rotations.
    nodes: usize,
    /// `Epoch`s of `[everyone]`, of Commons, and of one host each.
    everyone: usize,
    commons: usize,
    hosts: usize,
    /// Commons epochs that wrap every seated Mac; the other Commons epochs wrap three and a
    /// host's wraps one.
    commons_full: usize,
    wraps: usize,
    /// `Put`s of `[everyone]`, Commons and a host.
    puts: [usize; 3],
    /// The seated Macs (the first founders) that mint and wrap, and that write.
    minters: usize,
    writers: usize,
}

impl Mix {
    const fn epochs(&self) -> usize {
        self.everyone + self.commons + self.hosts
    }
    const fn total(&self) -> usize {
        1 + self.admits
            + self.removes
            + self.nodes
            + self.epochs()
            + self.wraps
            + self.puts[0]
            + self.puts[1]
            + self.puts[2]
    }
}

/// The live ALab log's counts on 2026-11-04: 4,093 records (Genesis 1, Admit 27, Remove 8 by one
/// remover, Node 53, Epoch 83, Wrap 3,369, Put 552), 57 members, 24 `[everyone]` epochs of 83 to
/// 90 wraps, 55 Commons epochs (13 of them wrapping every Mac, the rest three), 4 host epochs of
/// one wrap, 24 minters and 20 writers. Its genesis lists 40 founders, two unbound, and one
/// `Admit` lets one of those in (`Listed`); here 38 bound founders and 27 `Manual` admissions
/// leave the same 57 members.
const LIVE: Mix = Mix {
    founders: 38,
    admits: 27,
    removes: 8,
    nodes: 53,
    everyone: 24,
    commons: 55,
    hosts: 4,
    commons_full: 13,
    wraps: 3_369,
    puts: [526, 22, 4],
    minters: 24,
    writers: 20,
};

/// [`LIVE`]'s keyed records scaled by 12,288 / 4,057 to fill the keyed budget, 12,288 records,
/// and its weighed ones by the same factor: the log the live mix reaches when its keyed budget
/// is full, 12,395 records.
const KEYED: Mix = Mix {
    admits: 82,
    removes: 24,
    nodes: 161,
    everyone: 73,
    commons: 166,
    hosts: 12,
    commons_full: 39,
    wraps: 10_204,
    puts: [1_593, 67, 12],
    ..LIVE
};

/// [`KEYED`]'s keyed records and a full weighed budget, 4,096 records in the live log's ratio of
/// admissions to removals (27 to 8): 16,384 records, the most the production budgets hold.
const FULL: Mix = Mix {
    admits: 3_159,
    removes: 936,
    ..KEYED
};

fn shape(name: &str) -> Result<Mix> {
    Ok(match name {
        "live" => LIVE,
        "keyed" => KEYED,
        "full" => FULL,
        _ => return Err(format!("unknown shape {name}: live, keyed or full").into()),
    })
}

/// Wrap capsule and put ciphertext lengths that give a synthetic log the live log's average
/// `Wrap` (641 bytes) and `Put` (2,470 bytes) record lengths.
const CAPSULE: usize = 340;
const CIPHERTEXT: usize = 2_158;

/// SplitMix64: the same records on every run.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: usize) -> usize {
        let n = u64::try_from(n.max(1)).unwrap_or(u64::MAX);
        usize::try_from(self.next() % n).unwrap_or(0)
    }
}

/// 32 bytes derived from `label` and `n`.
fn derived(label: &str, n: usize) -> [u8; 32] {
    let mut input = b"log_census synthetic ".to_vec();
    input.extend_from_slice(label.as_bytes());
    input.extend_from_slice(&n.to_be_bytes());
    Sha256::digest(&input)
}

/// An Ed25519 node key pair derived from `label` and `n`.
fn node_pair(label: &str, n: usize) -> Result<Ed25519KeyPair> {
    Ok(Ed25519KeyPair::from_seed_unchecked(&derived(label, n))
        .map_err(|_| "a derived seed is an Ed25519 seed")?)
}

/// One synthetic Mac's keys, all derived from its number.
struct Mac {
    seed: [u8; 32],
    incarnation: [u8; 16],
    request: Request,
    node: Ed25519KeyPair,
    label: String,
}

impl Mac {
    fn new(n: usize) -> Result<Self> {
        let seed = derived("writer", n);
        let mut incarnation = [0; 16];
        incarnation.copy_from_slice(&derived("incarnation", n)[..16]);
        let mut machine = [0; 16];
        machine.copy_from_slice(&derived("machine", n)[..16]);
        let identity = Identity::from_secret_bytes(derived("age", n));
        let request = Request::create([2; 16], [3; 32], machine, incarnation, &identity, &seed)?;
        Ok(Self {
            seed,
            incarnation,
            request,
            node: node_pair("node", n)?,
            label: format!("m{}", n + 1),
        })
    }
    fn id(&self) -> MemberId {
        self.request.binding().actor()
    }
    fn entrant(&self) -> Entrant {
        Entrant {
            request: self.request.clone(),
            node_key: public(&self.node),
            name: format!("mac-{}", self.label),
            label: self.label.clone(),
        }
    }
}

fn public(pair: &Ed25519KeyPair) -> [u8; 32] {
    let mut key = [0; 32];
    key.copy_from_slice(pair.public_key().as_ref());
    key
}

/// Signs a synthetic log batch by batch, as Macs do: a batch is one Mac's chain on its previous
/// record, whose first record also follows the heads (one batch in five an older view of them,
/// so a head forks off that a later batch merges back). A batch on an older view also follows
/// the signer's seat and each record its records name, once each.
struct Writer {
    log: Log,
    rng: Rng,
    macs: Vec<Mac>,
    /// Each Mac's last record and its sequence.
    previous: BTreeMap<usize, (RecordId, u64)>,
    /// Each admitted Mac's `Admit`.
    seat: BTreeMap<usize, RecordId>,
    heads: BTreeSet<RecordId>,
    /// The heads as the previous batch started.
    older: Vec<RecordId>,
    /// Within a batch: whether it follows an older view, and what it already follows.
    batch: Option<(bool, BTreeSet<RecordId>)>,
    authored_ms: u64,
}

impl Writer {
    fn start(&mut self) {
        self.batch = None;
    }

    /// Sign and hold `body` by Mac `t`.
    fn sign(&mut self, t: usize, body: Body) -> Result<RecordId> {
        let previous = self.previous.get(&t).copied();
        let mut parents: Vec<RecordId> = previous.map(|(id, _)| id).into_iter().collect();
        if self.batch.is_none() {
            let heads: Vec<RecordId> = self.heads.iter().copied().collect();
            let older = self.rng.below(5) == 0 && !self.older.is_empty();
            parents.extend(if older { &self.older } else { &heads });
            self.older = heads;
            self.batch = Some((older, BTreeSet::new()));
        }
        let named = match &body {
            Body::Wrap { epoch, .. } | Body::Put { epoch, .. } => Some(*epoch),
            Body::Remove { cutoff, .. } => Some(*cutoff),
            _ => None,
        };
        let seat = self.seat.get(&t).copied();
        if let Some((true, followed)) = &mut self.batch {
            for needed in seat.into_iter().chain(named) {
                if followed.insert(needed) {
                    parents.push(needed);
                }
            }
        }
        parents.sort_unstable();
        parents.dedup();
        if parents.len() > aterm_secrets::membership_log::MAX_PARENTS {
            return Err(format!("{} parents", parents.len()).into());
        }
        let sequence = previous.map_or(1, |(_, s)| s + 1);
        self.authored_ms += 1_000;
        let mac = &self.macs[t];
        let record = Draft {
            network: *self.log.network(),
            sequence,
            previous: previous.map(|(id, _)| id),
            parents,
            authored_ms: self.authored_ms,
            body,
        }
        .sign(&mac.seed, mac.incarnation)?;
        let id = *record.id();
        for parent in record.parents() {
            self.heads.remove(parent);
        }
        match self.log.insert(record)? {
            Inserted::Held => {}
            other => return Err(format!("a synthetic record was {other:?}, not held").into()),
        }
        self.heads.insert(id);
        self.previous.insert(t, (id, sequence));
        Ok(id)
    }

    /// A `Node` by Mac `t` binding `pair`'s key, in a batch of its own.
    fn node(&mut self, t: usize, pair: &Ed25519KeyPair) -> Result<()> {
        self.start();
        let proof = pair.sign(&node_proof_message(self.log.network(), &self.macs[t].id()));
        let mut signature = [0; 64];
        signature.copy_from_slice(proof.as_ref());
        let body = Body::Node {
            node_key: public(pair),
            proof: signature,
        };
        self.sign(t, body).map(|_| ())
    }

    fn pick(&mut self, from: &[usize]) -> usize {
        from[self.rng.below(from.len())]
    }
}

/// `total` spread over `n` rounds: round `r`'s share.
const fn share(total: usize, n: usize, r: usize) -> usize {
    (r + 1) * total / n - r * total / n
}

/// Sign a log of `mix`. It runs one round per `Epoch`, in a shuffled order of audiences that
/// starts with `[everyone]`. A round holds its share of admissions (each by one of two founders,
/// the entrant's `Node` after it while Macs lack one), of `Node` rotations, of removal batches
/// (up to eight `Remove`s by one founder, of admitted Macs), the epoch and its `Wrap`s (a
/// minter's to the seated Macs, then another minter's past them, as a second holder wraps), and
/// its share of `Put`s, one batch per audience by one writer, under that audience's newest
/// epoch. Wrap capsules and put ciphertexts are filler ([`CAPSULE`], [`CIPHERTEXT`]).
fn sign_log(mix: Mix) -> Result<Log> {
    let macs = (0..mix.founders + mix.admits)
        .map(Mac::new)
        .collect::<Result<Vec<Mac>>>()?;
    let founders = macs[..mix.founders].iter().map(Mac::entrant).collect();
    let genesis = Record::found(&derived("genesis", 0), founders, 1)?;
    let mut w = Writer {
        log: Log::new(genesis)?,
        rng: Rng(u64::try_from(mix.total())?),
        macs,
        previous: BTreeMap::new(),
        seat: BTreeMap::new(),
        heads: BTreeSet::new(),
        older: Vec::new(),
        batch: None,
        authored_ms: 1_000,
    };
    w.heads.insert(*w.log.network());
    let minters: Vec<usize> = (0..mix.minters.min(mix.founders)).collect();
    let writers: Vec<usize> = (0..mix.writers.min(mix.founders)).collect();
    let first_nodes = mix.nodes.min(mix.founders + mix.admits);
    let rotations = mix.nodes - first_nodes;
    let mut nodes = 0;
    let mut seated: Vec<usize> = (0..mix.founders).collect();
    for t in 0..mix.founders.min(first_nodes) {
        let pair = node_pair("node", t)?;
        w.node(t, &pair)?;
        nodes += 1;
    }
    let mut audiences: Vec<u8> = [(0u8, mix.everyone), (1, mix.commons), (2, mix.hosts)]
        .iter()
        .flat_map(|&(a, n)| std::iter::repeat_n(a, n))
        .collect();
    for i in (1..audiences.len()).rev() {
        let j = w.rng.below(i + 1);
        audiences.swap(i, j);
    }
    let first_everyone = audiences.iter().position(|&a| a == 0).unwrap_or(0);
    audiences.swap(0, first_everyone);
    let epochs = mix.epochs();
    let few = 3 * (mix.commons - mix.commons_full) + mix.hosts;
    let (full_epochs, full_wraps) = (mix.everyone + mix.commons_full, mix.wraps - few);
    let (mut full_seen, mut commons_seen, mut rotated) = (0, 0, 0);
    let removal_batches = mix.removes.div_ceil(8);
    let mut removes_left = mix.removes;
    let mut puts_left = mix.puts;
    let mut latest: BTreeMap<u8, (RecordId, EpochAudience)> = BTreeMap::new();
    let mut admitted = mix.founders;
    for (round, &kind) in audiences.iter().enumerate() {
        for _ in 0..share(mix.admits, epochs, round) {
            let (by, t) = (w.rng.below(2), admitted);
            admitted += 1;
            w.start();
            let entrant = w.macs[t].entrant();
            let evidence = Evidence::Manual;
            let admit = w.sign(by, Body::Admit { entrant, evidence })?;
            w.seat.insert(t, admit);
            seated.push(t);
            if nodes < first_nodes {
                let pair = node_pair("node", t)?;
                w.node(t, &pair)?;
                nodes += 1;
            }
        }
        for _ in 0..share(rotations, epochs, round) {
            let t = w.pick(&seated);
            let pair = node_pair("rotation", rotated)?;
            w.node(t, &pair)?;
            rotated += 1;
        }
        if removes_left > 0 && share(removal_batches, epochs, round) > 0 {
            w.start();
            for _ in 0..removes_left.min(8) {
                let targets: Vec<usize> = seated
                    .iter()
                    .copied()
                    .filter(|&t| t >= mix.founders)
                    .collect();
                if targets.is_empty() {
                    break;
                }
                let t = w.pick(&targets);
                let cutoff = w
                    .previous
                    .get(&t)
                    .map(|(id, _)| *id)
                    .or(w.seat.get(&t).copied());
                let cutoff = cutoff.ok_or("an admitted Mac has a seat")?;
                let member = w.macs[t].id();
                w.sign(0, Body::Remove { member, cutoff })?;
                seated.retain(|&s| s != t);
                removes_left -= 1;
            }
        }
        let minter = w.pick(&minters);
        let (audience, full) = match kind {
            0 => (EpochAudience::Everyone, true),
            1 => {
                commons_seen += 1;
                (EpochAudience::Commons, commons_seen <= mix.commons_full)
            }
            _ => {
                let host = w.pick(&seated);
                (
                    EpochAudience::Hosts(vec![w.macs[host].label.clone()]),
                    false,
                )
            }
        };
        let count = match (full, kind) {
            (true, _) => {
                full_seen += 1;
                share(full_wraps, full_epochs, full_seen - 1)
            }
            (false, 1) => 3,
            (false, _) => 1,
        };
        w.start();
        let epoch = w.sign(
            minter,
            Body::Epoch {
                audience: audience.clone(),
                generation: u64::try_from(round + 1)?,
                commitment: derived("commitment", round),
            },
        )?;
        latest.insert(kind, (epoch, audience));
        let offset = w.rng.below(seated.len());
        let mut wrapper = minter;
        for i in 0..count {
            if i > 0 && i % seated.len() == 0 {
                wrapper = w.pick(&minters);
                w.start();
            }
            let recipient = w.macs[seated[(offset + i) % seated.len()]]
                .request
                .binding()
                .recipient();
            let capsule = vec![0x5a; CAPSULE].into();
            w.sign(
                wrapper,
                Body::Wrap {
                    epoch,
                    recipient,
                    capsule,
                },
            )?;
        }
        // The round's puts, each to an audience that has an epoch, weighted by the puts it still
        // has; the last round takes every put left.
        let mut by_audience = [0usize; 3];
        let puts = if round + 1 == epochs {
            puts_left.iter().sum()
        } else {
            share(mix.puts.iter().sum(), epochs, round)
        };
        for _ in 0..puts {
            let open: Vec<usize> = (0..3)
                .filter(|&a| puts_left[a] > by_audience[a] && latest.contains_key(&(a as u8)))
                .collect();
            let weight: usize = open.iter().map(|&a| puts_left[a] - by_audience[a]).sum();
            if weight == 0 {
                break;
            }
            let mut pick = w.rng.below(weight);
            for &a in &open {
                let n = puts_left[a] - by_audience[a];
                if pick < n {
                    by_audience[a] += 1;
                    break;
                }
                pick -= n;
            }
        }
        for (a, &n) in by_audience.iter().enumerate() {
            if n == 0 {
                continue;
            }
            let (epoch, audience) = latest[&(a as u8)].clone();
            let writer = w.pick(&writers);
            w.start();
            for _ in 0..n {
                let salt = derived("salt", w.log.held_count());
                let ciphertext = vec![0xc3; CIPHERTEXT].into();
                let audience = audience.clone();
                w.sign(
                    writer,
                    Body::Put {
                        audience,
                        epoch,
                        salt,
                        ciphertext,
                    },
                )?;
            }
            puts_left[a] -= n;
        }
    }
    let held = w.log.held_count();
    if held != mix.total() || removes_left != 0 || puts_left != [0; 3] {
        return Err(format!(
            "signed {held} records of {}, {removes_left} removals and {puts_left:?} puts left",
            mix.total()
        )
        .into());
    }
    Ok(w.log)
}

/// Sign a log of the shape `name` and write its records file to `out`.
fn build_log(name: &str, out: &str) -> Result<()> {
    let log = timed("sign", || sign_log(shape(name)?))?;
    let stream = encode_records(log.records());
    std::fs::write(out, &stream)?;
    println!(
        "wrote {} records, {} bytes, {} members to {out}",
        log.held_count(),
        stream.len(),
        log.membership().members().len()
    );
    Ok(())
}
