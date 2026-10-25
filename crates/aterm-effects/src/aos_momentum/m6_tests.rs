// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The M6 learner's tests (design §13.7, stage C): every m6 test, with in-memory `DurableLog` and
//! `DurableStore` implementations and the source scans.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::Instant;

use aterm_aos::Digest;

use super::corpus::SplitMix;
use super::m4::{Evaluator, LabelRefusal, LabelScope, Serial, Spread, TrainLabel};
use super::m4_tests::evaluator;
use super::m6::{
    Alter, Arm, ChainPin, Comparison, DurableLog, DurableStore, EvalRecord, GridRecord, Labeller,
    LearnRefusal, Ledger, LedgerRefusal, Line, LogFault, M6_CHAIN_V1, Manifest, ManifestRefusal,
    ModelRefusal, Moved, Pair, PairLines, Ranker, Reason, SessionEnv, SessionRefusal, SessionRun,
    Sets, State, StateRefusal, Step, Store, StoreFault, StoreRefusal, Version, VersionRefusal,
    VoidCause, VoidClass, chain, cross_pair, data, derive, forcing, label_budget, look_pair,
    next_line, run_look, run_replay, session_a, session_b, sign_test, train, void_class,
};
use super::m6_policy::{
    EPOCHS, FEATURES, JUDGED, LOOKS, MAX_LABELS, MAX_REVISIONS, MAX_STEPS, MAX_VERSIONS, POOLS,
    PoolId, SetId, SetTag, Side, control_id, features, set_pools, side,
};
use super::plan::SchedulePlan;
use super::test_fixtures::BUILD;

// ---- Fixtures ----

/// Every job on a scoped thread of its own.
struct Threads;

impl Spread for Threads {
    fn run(&self, n: usize, job: &(dyn Fn(usize) + Sync)) {
        std::thread::scope(|s| {
            for i in 0..n {
                s.spawn(move || job(i));
            }
        });
    }
}

/// The scale every test draws: 8 plans of each train pool, 12 pools of 8 plans of each other
/// set, and 6 grid plans.
fn scaled() -> Sets {
    Sets::scaled(8, 12, 8, 6)
}

/// The scaled sets and their chain, labelled once on the shared evaluator.
struct Fixture {
    sets: Sets,
    chain: Vec<Version>,
}

fn fixture() -> &'static Fixture {
    static F: OnceLock<Fixture> = OnceLock::new();
    F.get_or_init(|| {
        let sets = scaled();
        let chain = chain(&sets, &Labeller::new(evaluator(), &Threads)).expect("the scaled chain");
        Fixture { sets, chain }
    })
}

fn pin() -> ChainPin {
    ChainPin::of(&fixture().chain).expect("five members")
}

#[test]
fn the_pinned_chain_reproduces_on_the_v1_sets() {
    let pin = M6_CHAIN_V1.expect("stage E pins the chain");
    assert_eq!(
        pin.digest().hex(),
        "ed3cfd79d82728f86eb6def01c1935713907abfa22a7d283003eca65e6670322"
    );
    assert_eq!(pin.ids[0], Version::base().id());
    let members = chain(&Sets::v1(), &Labeller::new(evaluator(), &Threads)).expect("the v1 chain");
    assert_eq!(ChainPin::of(&members), Some(pin));
}

/// One version root's ledgers and chain file, shared by every ledger over it.
#[derive(Clone, Debug, Default)]
struct Root {
    pairs: BTreeMap<Pair, Vec<Vec<u8>>>,
    chain: Option<Digest>,
    /// Every append fails once this many lines exist under the root (a fault).
    fail_from: Option<usize>,
}

type Shared = Arc<Mutex<Root>>;

fn root() -> Shared {
    Arc::new(Mutex::new(Root::default()))
}

/// One pair's ledger over a shared root, in memory.
#[derive(Clone, Debug)]
struct MemLog {
    root: Shared,
    pair: Pair,
}

impl MemLog {
    fn of(root: &Shared, pair: Pair) -> MemLog {
        MemLog {
            root: Arc::clone(root),
            pair,
        }
    }

    fn with<R>(&self, f: impl FnOnce(&mut Root) -> R) -> R {
        f(&mut self.root.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

impl DurableLog for MemLog {
    fn lines(&self) -> Result<Vec<Vec<u8>>, LogFault> {
        Ok(self.with(|r| r.pairs.get(&self.pair).cloned().unwrap_or_default()))
    }

    fn append(&mut self, k: u64, bytes: &[u8]) -> Result<(), LogFault> {
        let pair = self.pair;
        self.with(|r| {
            let total: usize = r.pairs.values().map(Vec::len).sum();
            if r.fail_from.is_some_and(|n| total >= n) {
                return Err(LogFault::Io("injected".to_owned()));
            }
            let lines = r.pairs.entry(pair).or_default();
            match (lines.len() as u64).cmp(&k) {
                std::cmp::Ordering::Equal => {
                    lines.push(bytes.to_vec());
                    Ok(())
                }
                std::cmp::Ordering::Greater => Err(LogFault::Exists),
                std::cmp::Ordering::Less => Err(LogFault::Io(format!("line {k} past the end"))),
            }
        })
    }

    fn root(&self) -> Result<Vec<PairLines>, LogFault> {
        Ok(self.with(|r| r.pairs.iter().map(|(p, l)| (*p, l.clone())).collect()))
    }

    fn chain(&mut self, digest: &Digest) -> Result<Digest, LogFault> {
        Ok(self.with(|r| *r.chain.get_or_insert(*digest)))
    }
}

/// One store's files, in memory, with a seam that stops after the `n`th write of a step.
#[derive(Clone, Debug, Default)]
struct MemStore {
    files: BTreeMap<String, Vec<u8>>,
    stop: Option<(Step, usize)>,
    seen: BTreeMap<String, usize>,
}

impl MemStore {
    fn stopping(step: Step, nth: usize) -> MemStore {
        MemStore {
            stop: Some((step, nth)),
            ..MemStore::default()
        }
    }

    fn after(&mut self, step: Step) -> Result<(), StoreFault> {
        let n = self.seen.entry(format!("{step:?}")).or_default();
        *n += 1;
        match self.stop {
            Some((s, nth)) if s == step && *n == nth => Err(StoreFault::Stopped(step)),
            _ => Ok(()),
        }
    }

    /// The files under `prefix`.
    fn under(&self, prefix: &str) -> BTreeMap<String, Vec<u8>> {
        self.files
            .iter()
            .filter(|(n, _)| n.starts_with(prefix))
            .map(|(n, b)| (n.clone(), b.clone()))
            .collect()
    }
}

impl DurableStore for MemStore {
    fn read(&self, name: &str) -> Result<Option<Vec<u8>>, StoreFault> {
        Ok(self.files.get(name).cloned())
    }

    fn list(&self, dir: &str) -> Result<Vec<String>, StoreFault> {
        let prefix = format!("{dir}/");
        let mut names: Vec<String> = self
            .files
            .keys()
            .filter_map(|n| n.strip_prefix(&prefix))
            .filter_map(|rest| rest.split('/').next())
            .map(str::to_owned)
            .collect();
        names.dedup();
        Ok(names)
    }

    fn create(&mut self, step: Step, name: &str, bytes: &[u8]) -> Result<(), StoreFault> {
        if self.files.contains_key(name) {
            return Err(StoreFault::Exists(name.to_owned()));
        }
        self.files.insert(name.to_owned(), bytes.to_vec());
        self.after(step)
    }

    fn create_dir(
        &mut self,
        step: Step,
        dir: &str,
        files: &[(&str, &[u8])],
    ) -> Result<(), StoreFault> {
        let prefix = format!("{dir}/");
        if self.files.keys().any(|n| n.starts_with(&prefix)) {
            return Err(StoreFault::Exists(dir.to_owned()));
        }
        for (name, bytes) in files {
            self.files.insert(format!("{prefix}{name}"), bytes.to_vec());
        }
        self.after(step)
    }

    fn replace(&mut self, step: Step, name: &str, bytes: &[u8]) -> Result<(), StoreFault> {
        self.files.insert(name.to_owned(), bytes.to_vec());
        self.after(step)
    }
}

/// A fresh store for practice pair 0, with v0 active.
fn store(nonce: u64) -> Store<MemStore> {
    Store::init(
        MemStore::default(),
        BUILD,
        Pair::Practice(0),
        nonce,
        Digest([0; 32]),
    )
    .expect("a fresh store")
}

/// Synthetic chain ids: v0 to v4.
fn ids() -> [Digest; 5] {
    [0, 1, 2, 3, 4].map(|n| Digest([n + 1; 32]))
}

fn open() -> Line {
    Line::Open {
        store: Digest([9; 32]),
        build: BUILD.to_owned(),
        chain: Digest([8; 32]),
        tests_log: Digest([7; 32]),
        replay: false,
    }
}

fn begin(look: u8, bits: &[bool]) -> Line {
    let (candidate, comparator) = look_pair(look, bits, &ids()).expect("a look");
    Line::LookBegin {
        look,
        candidate,
        comparator,
        list: Digest([6; 32]),
    }
}

/// An open session with every look decided by `bits`.
fn decided(bits: &[bool; 5]) -> Vec<Line> {
    let mut lines = vec![open()];
    for (n, &bit) in (1..=LOOKS).zip(bits) {
        lines.push(begin(n, &bits[..usize::from(n) - 1]));
        lines.push(Line::LookVerdict { bit });
    }
    lines
}

/// `decided` read, replayed and retired.
fn retired(bits: &[bool; 5]) -> Vec<Line> {
    let mut lines = decided(bits);
    lines.push(Line::ReportBegin {
        list: Digest([5; 32]),
    });
    lines.push(Line::ReportDone {
        report: Digest([4; 32]),
    });
    lines.extend((1..=LOOKS).map(|look| Line::Replay { look }));
    lines.push(Line::Retire);
    lines
}

/// `lines` in order, each one checked by the rules.
fn follows(lines: &[Line]) -> Result<(), (usize, LedgerRefusal)> {
    for k in 0..lines.len() {
        next_line(&lines[..k], &lines[k], &ids()).map_err(|e| (k, e))?;
    }
    Ok(())
}

/// Both children of one scaled practice session, and their stores.
struct Session {
    a: SessionRun,
    b: SessionRun,
    store_a: Store<MemStore>,
    store_b: Store<MemStore>,
    root: Shared,
}

fn run_session(ev: &Evaluator, force: Option<fn(u8) -> bool>) -> Session {
    let f = fixture();
    let env = SessionEnv {
        build: BUILD,
        sets: &f.sets,
        labeller: Labeller::new(ev, &Threads),
        chain: pin(),
        tests_log: Digest([7; 32]),
    };
    let root = root();
    let run = || {
        let log = MemLog::of(&root, Pair::Practice(0));
        let mut ledger = Ledger::practice(log.clone(), 0, pin()).expect("a fresh ledger");
        let mut store_a = store(1);
        let a = session_a(&env, &mut ledger, &mut store_a).expect("child A");
        let mut ledger = Ledger::practice(log, 0, pin()).expect("A's ledger");
        let mut store_b = store(2);
        let b = session_b(&env, &mut ledger, &mut store_b).expect("child B");
        (a, b, store_a, store_b)
    };
    let (a, b, store_a, store_b) = match force {
        Some(force) => forcing(force, run),
        None => run(),
    };
    Session {
        a,
        b,
        store_a,
        store_b,
        root,
    }
}

/// The session forced to admit every look.
fn all_admitted() -> &'static Session {
    static S: OnceLock<Session> = OnceLock::new();
    S.get_or_init(|| run_session(evaluator(), Some(|_| true)))
}

/// The session forced to refuse every look.
fn all_refused() -> &'static Session {
    static S: OnceLock<Session> = OnceLock::new();
    S.get_or_init(|| run_session(evaluator(), Some(|_| false)))
}

/// The session with look 2, the control's, forced to admit and every other look refused.
fn control_admitted() -> &'static Session {
    static S: OnceLock<Session> = OnceLock::new();
    S.get_or_init(|| run_session(evaluator(), Some(|look| look == 2)))
}

/// The unforced session, on an evaluator of its own whose reads it counts.
fn natural() -> &'static (Evaluator, Session) {
    static S: OnceLock<(Evaluator, Session)> = OnceLock::new();
    S.get_or_init(|| {
        let ev = evaluator().anew();
        let session = run_session(&ev, None);
        (ev, session)
    })
}

/// G2's comparison of A and B (design §13.9): every version file, eval record and figures file
/// byte for byte, the revisions' (reason, active, head), and the bits.
fn replays_equal(s: &Session) {
    let (a, b) = (s.store_a.io(), s.store_b.io());
    assert_eq!(a.under("versions/"), b.under("versions/"), "version files");
    assert_eq!(
        a.under("eval/"),
        b.under("eval/"),
        "eval records and figures"
    );
    assert_eq!(a.under("eval/").len(), 2 * usize::from(LOOKS));
    let moves = |st: &Store<MemStore>| -> Vec<(Reason, Digest, Digest)> {
        st.revisions()
            .iter()
            .map(|r| (r.reason, r.active, r.head))
            .collect()
    };
    assert_eq!(moves(&s.store_a), moves(&s.store_b), "revisions");
    assert_eq!(
        s.a.bits, s.b.bits,
        "B's replayed bits are A's recorded ones"
    );
    assert_eq!(s.a.chain, s.b.chain);
    assert_eq!(s.a.mf, s.b.mf);
}

fn names(moves: &[(&'static str, Moved)]) -> Vec<&'static str> {
    moves.iter().map(|(n, _)| *n).collect()
}

// ---- The model ----

#[test]
fn the_chain_is_deterministic() {
    let f = fixture();
    let again = chain(&f.sets, &Labeller::new(&evaluator().anew(), &Serial)).expect("the chain");
    assert_eq!(
        again, f.chain,
        "serial on a fresh evaluator equals threaded on the shared one"
    );
    assert_eq!(again.len(), 5);
    for (r, v) in again.iter().enumerate() {
        assert_eq!(usize::from(v.round()), r);
        assert_eq!(
            v.parent(),
            r.checked_sub(1).map(|p| again[p].id()),
            "each member trains from the one before"
        );
    }
    assert!(again[1].state().steps() > 0, "v1 took steps");
    assert_eq!(again[4].state().epochs(), 4 * 16);
}

#[test]
fn the_chain_names_no_build() {
    let other = Evaluator::v1("another-build", Instant::now()).expect("the pinned policy v1");
    let theirs = chain(&fixture().sets, &Labeller::new(&other, &Threads)).expect("the chain");
    assert_eq!(
        ChainPin::of(&theirs),
        Some(pin()),
        "equal ids and label digests"
    );
}

#[test]
fn a_state_round_trips_and_its_bounds_are_refused() {
    for v in &fixture().chain {
        let bytes = v.state().encode();
        assert_eq!(bytes.len(), super::m6::STATE_LEN);
        assert_eq!(State::decode(&bytes).as_ref(), Ok(v.state()));
    }
    let good = fixture().chain[2].state().encode();
    let t_at = 12 + super::m6::ADAPTER_LEN + FEATURES * 8;
    let with = |at: usize, bytes: &[u8]| {
        let mut b = good.clone();
        b[at..at + bytes.len()].copy_from_slice(bytes);
        State::decode(&b)
    };
    assert_eq!(
        State::decode(&good[1..]),
        Err(StateRefusal::Length {
            len: good.len() - 1
        })
    );
    assert_eq!(with(0, b"XM6S"), Err(StateRefusal::Magic));
    assert_eq!(
        with(4, &2_u32.to_le_bytes()),
        Err(StateRefusal::Format { found: 2 })
    );
    assert_eq!(
        with(8, &1_u32.to_le_bytes()),
        Err(StateRefusal::Features { found: 1 })
    );
    assert_eq!(
        with(t_at, &(MAX_STEPS + 1).to_le_bytes()),
        Err(StateRefusal::Bounds),
        "t past MAX_STEPS"
    );
    let t = fixture().chain[2].state().steps();
    assert_eq!(
        with(12, &i32::try_from(t + 1).expect("small").to_le_bytes()),
        Err(StateRefusal::Bounds),
        "a weight past t"
    );
    assert_eq!(
        with(12 + super::m6::ADAPTER_LEN, &i64::MAX.to_le_bytes()),
        Err(StateRefusal::Bounds),
        "an average past t(t + 1)/2"
    );
}

#[test]
fn training_past_the_step_budget_is_refused() {
    let mut bytes = State::base().encode();
    let t_at = 12 + super::m6::ADAPTER_LEN + FEATURES * 8;
    bytes[t_at..t_at + 8].copy_from_slice(&MAX_STEPS.to_le_bytes());
    let full = State::decode(&bytes).expect("t at the budget is a state");
    let labels = evaluator()
        .label_spread(
            LabelScope::Train,
            &fixture()
                .sets
                .grid()
                .iter()
                .map(SchedulePlan::encode)
                .collect::<Vec<_>>(),
            &Threads,
        )
        .expect("the grid's labels");
    let refs: Vec<_> = labels.iter().collect();
    assert_eq!(
        train(&full, &refs),
        Err(super::m6::LearnRefusal::StepBudget)
    );
    let steps = train(&State::base(), &refs).expect("a train").steps();
    let from = |t: u64| {
        let mut bytes = State::base().encode();
        bytes[t_at..t_at + 8].copy_from_slice(&t.to_le_bytes());
        train(&State::decode(&bytes).expect("a state"), &refs)
    };
    assert_eq!(
        from(MAX_STEPS - steps).map(|s| s.steps()),
        Ok(MAX_STEPS),
        "a train may reach MAX_STEPS"
    );
    assert_eq!(
        from(MAX_STEPS - steps + 1),
        Err(super::m6::LearnRefusal::StepBudget),
        "and not pass it"
    );
    assert_eq!(label_budget(MAX_LABELS), Ok(()));
    assert_eq!(
        label_budget(MAX_LABELS + 1),
        Err(super::m6::LearnRefusal::LabelBudget {
            count: MAX_LABELS + 1
        })
    );
}

#[test]
fn a_version_round_trips_from_its_files() {
    for v in &fixture().chain {
        let parts = (
            v.descriptor(),
            v.state().encode(),
            v.manifest().encode(),
            v.labels_bytes(),
        );
        assert_eq!(
            Version::from_files(&parts.0, &parts.1, &parts.2, &parts.3).as_ref(),
            Ok(v)
        );
        let mut d = parts.0.clone();
        let last = d.len() - 1;
        d[last] ^= 1;
        assert_eq!(
            Version::from_files(&d, &parts.1, &parts.2, &parts.3),
            Err(VersionRefusal::Parts),
            "a descriptor naming another label digest"
        );
        if !parts.3.is_empty() {
            assert_eq!(
                Version::from_files(&parts.0, &parts.1, &parts.2, &parts.3[32..]),
                Err(VersionRefusal::Labels)
            );
        }
        let mut m = parts.2.clone();
        let ids_at = m.len() - 4 - 32;
        m[ids_at] ^= 1;
        assert_eq!(
            Version::from_files(&parts.0, &parts.1, &m, &parts.3),
            Err(VersionRefusal::Manifest(ManifestRefusal::Summary)),
            "a manifest whose id digest is not its plans'"
        );
        let mut m = parts.2.clone();
        let excluded_at = m.len() - 4;
        m[excluded_at..].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(
            Version::from_files(&parts.0, &parts.1, &m, &parts.3),
            Err(VersionRefusal::Manifest(ManifestRefusal::Bounds)),
            "a manifest dropping more plans than any chain offers"
        );
    }
    let f = fixture();
    let bytes = f.chain[1].manifest().encode();
    let dropping = |excluded: u32| {
        let mut m = bytes.clone();
        let at = m.len() - 4;
        m[at..].copy_from_slice(&excluded.to_le_bytes());
        Manifest::decode(&m).map(|m| m.excluded())
    };
    let cap = u32::try_from(MAX_LABELS).expect("a count");
    assert_eq!(dropping(cap), Ok(cap));
    assert_eq!(dropping(cap + 1), Err(ManifestRefusal::Bounds));
    let (v0, v4) = (&f.chain[0], &f.chain[4]);
    for (v, why) in [
        (v4.altered(Alter::Round(255)), "a round past ROUNDS"),
        (v4.altered(Alter::Round(0)), "round 0 with a parent"),
        (v0.altered(Alter::Round(1)), "a member without a parent"),
        (v0.altered(Alter::Random), "a round 0 that is not v0"),
    ] {
        assert_eq!(
            Version::from_files(
                &v.descriptor(),
                &v.state().encode(),
                &v.manifest().encode(),
                &v.labels_bytes()
            ),
            Err(VersionRefusal::Parts),
            "{why}"
        );
    }
    let past = v4.altered(Alter::Round(255));
    let labeller = Labeller::new(evaluator(), &Threads);
    assert_eq!(
        data(&past, &f.sets).err(),
        Some(LearnRefusal::Round { round: 255 })
    );
    assert_eq!(
        derive(&past, &f.sets, &labeller).err(),
        Some(LearnRefusal::Round { round: 255 }),
        "refused, not overflowed"
    );
    assert_eq!(
        derive(v4, &f.sets, &labeller).err(),
        Some(LearnRefusal::Round { round: 5 })
    );
}

#[test]
fn every_versioned_part_is_in_the_id() {
    let v = &fixture().chain[2];
    let mut seen = vec![v.id()];
    for how in [
        Alter::Weight(1),
        Alter::Average,
        Alter::Steps,
        Alter::Epochs,
        Alter::Random,
        Alter::DropPlan,
        Alter::Label,
    ] {
        let w = v.altered(how);
        assert!(!seen.contains(&w.id()), "{how:?} moves the id");
        seen.push(w.id());
    }
}

/// `m6-train v1` from its rule text (`TRAIN_RULE`), on a state's bytes, apart from the module's
/// trainer: the parent's state decoded by its layout, then the state's bytes after.
fn train_by_rule(parent: &[u8], data: &[&TrainLabel]) -> Vec<u8> {
    let at = |i: usize, n: usize| &parent[i..i + n];
    let le = |b: &[u8]| b.iter().rev().fold(0_u64, |v, &x| v << 8 | u64::from(x));
    let (w_at, acc_at) = (12, 12 + 4 * FEATURES);
    let t_at = acc_at + 8 * FEATURES;
    let mut w: Vec<i64> = (0..FEATURES)
        .map(|f| i64::from(le(at(w_at + 4 * f, 4)) as u32 as i32))
        .collect();
    let mut acc: Vec<i64> = (0..FEATURES)
        .map(|f| le(at(acc_at + 8 * f, 8)) as i64)
        .collect();
    let mut t = le(at(t_at, 8));
    let epochs = le(at(t_at + 8, 4)) as u32;
    let mut rng = SplitMix::new(le(at(t_at + 12, 8)));
    let order = |l: &TrainLabel| l.totals().map_or((1, 0, 0), |t| (0, t.ticks, t.wasted));
    let score = |w: &[i64], l: &TrainLabel| features(&l.plan()).iter().map(|&f| w[f]).sum::<i64>();
    for _ in 0..EPOCHS {
        let mut perm: Vec<usize> = (0..data.len()).collect();
        for i in (1..perm.len()).rev() {
            let j = rng.below(i as u64 + 1) as usize;
            perm.swap(i, j);
        }
        for k in 1..perm.len() {
            let (p, q) = (data[perm[k - 1]], data[perm[k]]);
            if order(p) == order(q) {
                continue;
            }
            let (better, worse) = if order(p) < order(q) { (p, q) } else { (q, p) };
            if score(&w, better) <= score(&w, worse) {
                for f in features(&better.plan()) {
                    w[f] += 1;
                }
                for f in features(&worse.plan()) {
                    w[f] -= 1;
                }
            }
            t += 1;
            for (a, &x) in acc.iter_mut().zip(&w) {
                *a += x;
            }
        }
    }
    let mut out = b"AM6S".to_vec();
    out.extend(1_u32.to_le_bytes());
    out.extend(u32::try_from(FEATURES).expect("367").to_le_bytes());
    for &x in &w {
        out.extend(i32::try_from(x).expect("an i32 weight").to_le_bytes());
    }
    for &x in &acc {
        out.extend(x.to_le_bytes());
    }
    out.extend(t.to_le_bytes());
    out.extend((epochs + EPOCHS).to_le_bytes());
    out.extend(rng.state().to_le_bytes());
    out
}

#[test]
fn the_trainer_follows_its_rule_and_its_random_state_matters() {
    let f = fixture();
    let (parent, member) = (&f.chain[1], &f.chain[2]);
    let labels = evaluator()
        .label_spread(
            LabelScope::Train,
            &member
                .manifest()
                .plans()
                .map(SchedulePlan::encode)
                .collect::<Vec<_>>(),
            &Threads,
        )
        .expect("v2's labels");
    let refs: Vec<&TrainLabel> = labels.iter().collect();
    let trained = train(parent.state(), &refs).expect("a train");
    assert_eq!(
        &trained,
        member.state(),
        "v2 is v1's state trained on v2's data"
    );
    assert_eq!(
        trained.encode(),
        train_by_rule(&parent.state().encode(), &refs),
        "m6-train v1, as its rule reads"
    );
    let reseeded = parent.altered(Alter::Random);
    let other = train(reseeded.state(), &refs).expect("a train");
    assert_ne!(
        (other.adapter_bytes(), other.optimizer_bytes()),
        (trained.adapter_bytes(), trained.optimizer_bytes()),
        "the random state orders the data, and the result depends on the order"
    );
    assert_eq!(
        other.encode(),
        train_by_rule(&reseeded.state().encode(), &refs)
    );
}

// ---- Admission ----

/// `C(n, k)` by Pascal's rule, independently of the module's.
fn pascal(n: u32) -> Vec<u128> {
    let mut row = vec![1_u128];
    for _ in 0..n {
        let mut next = vec![1_u128; row.len() + 1];
        for k in 1..row.len() {
            next[k] = row[k - 1] + row[k];
        }
        row = next;
    }
    row
}

#[test]
fn the_admission_rule_has_its_exact_thresholds() {
    assert!(sign_test(620, 10, 10), "n = 10, w = 10 admits");
    assert!(!sign_test(620, 9, 10));
    for w in 0..=9 {
        assert!(!sign_test(620, w, 9), "n = 9 never admits");
    }
    assert!(sign_test(60, 6, 6) && !sign_test(60, 5, 6) && !sign_test(60, 5, 5));
    assert!(!sign_test(620, 0, 0) && !sign_test(620, 11, 10));
    assert!(!sign_test(1, 65, 65), "n past POOLS");
    for level in [60, 620] {
        for n in 1..=u32::try_from(POOLS).expect("64") {
            let row = pascal(n);
            for w in 0..=n {
                let tail: u128 = row[w as usize..].iter().sum();
                assert_eq!(
                    sign_test(level, w, n),
                    level * tail <= 1 << n,
                    "{level} {w}/{n}"
                );
            }
        }
    }
    let c = |wins, losses, gain, against| Comparison {
        wins,
        losses,
        gain,
        against,
    };
    assert_eq!(
        c(0, 0, (0, 0), (0, 0)).beats(620),
        Err(ModelRefusal::NoUntied)
    );
    assert_eq!(
        c(9, 0, (9, 9), (0, 0)).beats(620),
        Err(ModelRefusal::Losses)
    );
    assert_eq!(c(10, 0, (1, 0), (0, 1)).beats(620), Err(ModelRefusal::Gain));
    assert_eq!(c(10, 0, (0, 1), (1, 0)).beats(620), Err(ModelRefusal::Gain));
    assert_eq!(c(10, 0, (1, 1), (1, 1)).beats(620), Ok(()));
    let six = c(6, 0, (1, 1), (0, 0));
    assert_eq!(
        six.admits(),
        Err(ModelRefusal::Losses),
        "a look is at 1/620"
    );
    assert_eq!(six.shows(), Ok(()), "a criterion is at 1/60");
    let ten = c(10, 0, (1, 1), (0, 0));
    assert_eq!((ten.admits(), ten.shows()), (Ok(()), Ok(())));
    assert_eq!(c(5, 0, (1, 1), (0, 0)).shows(), Err(ModelRefusal::Losses));
}

#[test]
fn the_comparators_follow_the_bits() {
    let [v0, v1, v2, v3, v4] = ids();
    assert_eq!(look_pair(1, &[], &ids()), Some((v1, v0)));
    assert_eq!(look_pair(2, &[true], &ids()), Some((control_id(), v0)));
    assert_eq!(look_pair(3, &[true, true], &ids()), Some((v2, v1)));
    assert_eq!(look_pair(3, &[false, true], &ids()), Some((v2, v0)));
    assert_eq!(look_pair(4, &[true, false, true], &ids()), Some((v3, v2)));
    assert_eq!(look_pair(4, &[true, false, false], &ids()), Some((v3, v1)));
    assert_eq!(
        look_pair(5, &[false, true, false, true], &ids()),
        Some((v4, v3))
    );
    assert_eq!(look_pair(5, &[false; 4], &ids()), Some((v4, v0)));
    assert_eq!(look_pair(6, &[true; 5], &ids()), None);
    let lines = vec![open(), begin(1, &[]), Line::LookVerdict { bit: true }];
    let naming_v1 = Line::LookBegin {
        look: 2,
        candidate: control_id(),
        comparator: v1,
        list: Digest([0; 32]),
    };
    assert_eq!(
        next_line(&lines, &naming_v1, &ids()),
        Err(LedgerRefusal::OutOfOrder),
        "after an admitting look 1, look 2 names v0"
    );
    assert_eq!(next_line(&lines, &begin(2, &[true]), &ids()), Ok(()));
}

// ---- The ledger ----

#[test]
fn a_whole_session_follows_the_rules() {
    let mut lines = retired(&[true, false, true, false, false]);
    lines.push(replay_open());
    lines.extend((1..=LOOKS).map(|look| Line::Replay { look }));
    lines.push(Line::Record);
    assert_eq!(follows(&lines), Ok(()));
}

fn replay_open() -> Line {
    replay_open_on(Digest([10; 32]))
}

/// A replaying open, of child B's store `store`.
fn replay_open_on(store: Digest) -> Line {
    match open() {
        Line::Open {
            build,
            chain,
            tests_log,
            ..
        } => Line::Open {
            store,
            build,
            chain,
            tests_log,
            replay: true,
        },
        _ => unreachable!(),
    }
}

#[test]
fn every_ledger_rule_refuses_by_name() {
    let refused = |lines: &[Line], line: Line| next_line(lines, &line, &ids()).err();
    let out = Some(LedgerRefusal::OutOfOrder);
    // Before an open.
    assert_eq!(refused(&[], begin(1, &[])), out);
    assert_eq!(refused(&[], Line::LookVerdict { bit: true }), out);
    assert_eq!(refused(&[], Line::Retire), out);
    // In a session.
    assert_eq!(refused(&[open()], open()), out, "a second open");
    assert_eq!(refused(&[open()], begin(2, &[])), out, "look 2 first");
    assert_eq!(refused(&[open()], Line::LookVerdict { bit: true }), out);
    let one = [open(), begin(1, &[])];
    assert_eq!(
        refused(&one, begin(2, &[false])),
        out,
        "a look before the verdict"
    );
    let partial = &decided(&[false; 5])[..5];
    assert_eq!(
        refused(
            partial,
            Line::ReportBegin {
                list: Digest([0; 32])
            }
        ),
        out,
        "the report before the last verdict"
    );
    let all = decided(&[false; 5]);
    let sixth = Line::LookBegin {
        look: 6,
        candidate: ids()[4],
        comparator: ids()[0],
        list: Digest([0; 32]),
    };
    assert_eq!(refused(&all, sixth), Some(LedgerRefusal::HeldOutSpent));
    assert_eq!(refused(&all, Line::Retire), out, "retire before the report");
    assert_eq!(
        refused(&all, Line::Replay { look: 1 }),
        out,
        "replay before the report"
    );
    let mut read = all.clone();
    read.push(Line::ReportBegin {
        list: Digest([0; 32]),
    });
    assert_eq!(
        refused(
            &read,
            Line::ReportBegin {
                list: Digest([0; 32])
            }
        ),
        Some(LedgerRefusal::ReportSpent)
    );
    read.push(Line::ReportDone {
        report: Digest([0; 32]),
    });
    assert_eq!(
        refused(&read, Line::Retire),
        out,
        "retire before every replay"
    );
    assert_eq!(refused(&read, Line::Replay { look: 6 }), out);
    assert_eq!(refused(&read, Line::Record), out, "record before retire");
    // After retire.
    let done = retired(&[false; 5]);
    assert_eq!(refused(&done, begin(1, &[])), out);
    assert_eq!(
        refused(
            &done,
            Line::ReportBegin {
                list: Digest([0; 32])
            }
        ),
        Some(LedgerRefusal::ReportSpent)
    );
    assert_eq!(
        refused(&done, Line::Replay { look: 1 }),
        out,
        "no replay before B opens"
    );
    assert_eq!(refused(&done, Line::Retire), out);
    assert_eq!(
        refused(&done, Line::Record),
        out,
        "no record before B opens"
    );
    assert_eq!(refused(&done, open()), out);
    assert_eq!(
        refused(&done, replay_open_on(Digest([9; 32]))),
        out,
        "a replaying open on a store already opened"
    );
    let mut b = done.clone();
    b.push(replay_open());
    assert_eq!(refused(&b, replay_open()), out, "one replaying child");
    assert_eq!(refused(&b, Line::Replay { look: 3 }), None);
    b.push(Line::Record);
    for line in [
        open(),
        replay_open(),
        Line::Replay { look: 1 },
        Line::Record,
    ] {
        assert_eq!(refused(&b, line), out, "nothing after a record");
    }
}

#[test]
fn each_void_is_classed_by_when_it_arose() {
    let causes = [
        VoidCause::Deadline,
        VoidCause::ChildFailed,
        VoidCause::Gate,
        VoidCause::Other,
    ];
    for cause in causes {
        assert_eq!(void_class(&[open()], cause), VoidClass::Stopped);
    }
    let begun = [open(), begin(1, &[])];
    assert_eq!(void_class(&begun, VoidCause::Deadline), VoidClass::Opening);
    assert_eq!(
        void_class(&begun, VoidCause::ChildFailed),
        VoidClass::Opening
    );
    assert_eq!(void_class(&begun, VoidCause::Gate), VoidClass::Closing);
    assert_eq!(void_class(&begun, VoidCause::Other), VoidClass::Closing);
    let decided = [open(), begin(1, &[]), Line::LookVerdict { bit: false }];
    for cause in causes {
        assert_eq!(void_class(&decided, cause), VoidClass::Closing);
    }
    let mut b = retired(&[false; 5]);
    b.push(replay_open());
    assert_eq!(
        void_class(&b, VoidCause::ChildFailed),
        VoidClass::Closing,
        "a replaying child's failure is in the session it replays"
    );
}

#[test]
fn a_stopped_session_spends_nothing() {
    let root = root();
    let mut ledger =
        Ledger::practice(MemLog::of(&root, Pair::Practice(0)), 0, pin()).expect("a ledger");
    ledger
        .open(Digest([1; 32]), BUILD, Digest([2; 32]), false)
        .expect("an open");
    assert_eq!(ledger.void(VoidCause::Deadline), Ok(VoidClass::Stopped));
    ledger
        .open(Digest([1; 32]), BUILD, Digest([2; 32]), false)
        .expect("the pair opens again");
    let again =
        Ledger::practice(MemLog::of(&root, Pair::Practice(0)), 0, pin()).expect("it reloads");
    assert_eq!(again.lines().len(), 3);
    let opened = [open(), begin(1, &[])];
    assert_eq!(
        next_line(
            &opened,
            &Line::Void {
                cause: VoidCause::Deadline,
                class: VoidClass::Stopped
            },
            &ids()
        ),
        Err(LedgerRefusal::OutOfOrder),
        "a void's class is its own, not the writer's"
    );
}

#[test]
fn the_pair_checks_refuse_by_name() {
    let rehearsed = (
        Pair::Practice(0),
        vec![
            open(),
            Line::ReportDone {
                report: Digest([0; 32]),
            },
        ],
    );
    let void = |class| Line::Void {
        cause: VoidCause::Deadline,
        class,
    };
    assert_eq!(cross_pair(1, &[]), Err(LedgerRefusal::NoRehearsal));
    assert_eq!(
        cross_pair(1, &[(Pair::Practice(0), vec![open()])]),
        Err(LedgerRefusal::NoRehearsal)
    );
    assert_eq!(cross_pair(1, std::slice::from_ref(&rehearsed)), Ok(()));
    assert_eq!(
        cross_pair(2, std::slice::from_ref(&rehearsed)),
        Err(LedgerRefusal::PairOrder),
        "pair 1 first"
    );
    let opening = (Pair::Real(1), vec![open(), void(VoidClass::Opening)]);
    assert_eq!(cross_pair(2, &[rehearsed.clone(), opening.clone()]), Ok(()));
    assert_eq!(
        cross_pair(3, &[rehearsed.clone(), opening.clone()]),
        Err(LedgerRefusal::PairOrder),
        "pair 2 between"
    );
    let later = (Pair::Real(2), vec![open()]);
    assert_eq!(
        cross_pair(
            1,
            &[rehearsed.clone(), (Pair::Real(1), vec![open()]), later]
        ),
        Err(LedgerRefusal::PairOrder),
        "a later pair exists"
    );
    for closing in [void(VoidClass::Closing), Line::Record] {
        assert_eq!(
            cross_pair(
                2,
                &[rehearsed.clone(), (Pair::Real(1), vec![open(), closing])]
            ),
            Err(LedgerRefusal::VersionClosed)
        );
    }
    assert_eq!(
        cross_pair(4, std::slice::from_ref(&rehearsed)),
        Err(LedgerRefusal::PairOrder)
    );
}

#[test]
fn the_ledger_refuses_a_race_a_moved_chain_and_a_broken_line() {
    let root = root();
    let log = MemLog::of(&root, Pair::Practice(0));
    let mut first = Ledger::practice(log.clone(), 0, pin()).expect("a ledger");
    let mut second = Ledger::practice(log.clone(), 0, pin()).expect("a ledger");
    first
        .open(Digest([1; 32]), BUILD, Digest([2; 32]), false)
        .expect("an open");
    assert_eq!(
        second.open(Digest([1; 32]), BUILD, Digest([2; 32]), false),
        Err(LedgerRefusal::LedgerRace)
    );
    let other = self::root();
    other.lock().expect("unpoisoned").chain = Some(Digest([3; 32]));
    let mut moved =
        Ledger::practice(MemLog::of(&other, Pair::Practice(1)), 1, pin()).expect("a ledger");
    assert_eq!(
        moved.open(Digest([1; 32]), BUILD, Digest([2; 32]), false),
        Err(LedgerRefusal::ChainMoved)
    );
    root.lock()
        .expect("unpoisoned")
        .pairs
        .get_mut(&Pair::Practice(0))
        .expect("a line")[0][0] ^= 1;
    assert_eq!(
        Ledger::practice(log, 0, pin()).err(),
        Some(LedgerRefusal::Broken { line: 0 })
    );
}

#[test]
fn a_token_is_minted_only_after_its_line() {
    let f = fixture();
    let root = root();
    let log = MemLog::of(&root, Pair::Practice(0));
    let mut ledger = Ledger::practice(log, 0, pin()).expect("a ledger");
    ledger
        .open(Digest([1; 32]), BUILD, Digest([2; 32]), false)
        .expect("an open");
    root.lock().expect("unpoisoned").fail_from = Some(1);
    let (x, a) = look_pair(1, &[], &pin().ids).expect("look 1");
    assert!(
        matches!(
            ledger.begin_look(1, x, a, &f.chain, &f.sets),
            Err(LedgerRefusal::Log(_))
        ),
        "no line, no token"
    );
    assert_eq!(ledger.lines().len(), 1);
    root.lock().expect("unpoisoned").fail_from = None;
    let look = ledger
        .begin_look(1, x, a, &f.chain, &f.sets)
        .expect("look 1 begins");
    let Some(Line::LookBegin { list, .. }) = ledger.lines().last() else {
        panic!("the look-begin line is durable");
    };
    assert_eq!(
        *list,
        look.token().digest(),
        "the line names the token's list"
    );
    let mut union: Vec<[u8; 12]> = look
        .tops()
        .iter()
        .flat_map(|(_, tx, ta)| tx.iter().chain(ta).map(SchedulePlan::encode))
        .collect();
    union.sort_unstable();
    union.dedup();
    assert_eq!(
        look.token().list(),
        union.as_slice(),
        "the list is the tops' union"
    );
    assert_eq!(look.tops().len(), 12, "every pool of the scaled set");
    for (pool, tx, ta) in look.tops() {
        let plans = &f.sets.pools(pool.set).expect("drawn")[pool.pool as usize];
        assert_eq!(
            *tx,
            Ranker::Model(f.chain[1].state())
                .top_k(pool, plans)
                .expect("4")
        );
        assert_eq!(
            *ta,
            Ranker::Model(f.chain[0].state())
                .top_k(pool, plans)
                .expect("4")
        );
    }
}

#[test]
fn a_practice_token_takes_only_practice_plans() {
    // The mints' own refusal of a side-1 or side-2 plan is m4's (`m4_tests.rs`), where the mint
    // scan lets a test call them; here the ledger mints, over its practice set.
    let held = set_pools(SetId {
        tag: SetTag::HeldOut,
        index: 1,
    })
    .expect("H_1")[0][0];
    let report = set_pools(SetId {
        tag: SetTag::Report,
        index: 1,
    })
    .expect("E_1")[0][0];
    assert_eq!(side(&held), Side::HeldOut);
    assert_eq!(side(&report), Side::Report);
    let f = fixture();
    for plan in [held, report] {
        let root = root();
        let mut ledger =
            Ledger::practice(MemLog::of(&root, Pair::Practice(0)), 0, pin()).expect("a ledger");
        ledger
            .open(Digest([1; 32]), BUILD, Digest([2; 32]), false)
            .expect("an open");
        let (x, a) = look_pair(1, &[], &pin().ids).expect("look 1");
        let look = ledger
            .begin_look(1, x, a, &f.chain, &f.sets)
            .expect("look 1 begins");
        let mut asked = look.token().list().to_vec();
        asked.push(plan.encode());
        let ev = evaluator().anew();
        assert_eq!(
            ev.label_spread(LabelScope::HeldOut(look.into_token()), &asked, &Serial)
                .err(),
            Some(LabelRefusal::ScopeMismatch {
                plan: Some(plan.encode())
            }),
            "a ledger's practice look token refuses a side-1 or side-2 plan"
        );
        assert_eq!(
            ev.reads().train.load(std::sync::atomic::Ordering::Relaxed),
            0,
            "refused before anything was judged"
        );
    }
}

#[test]
fn an_admission_is_bound_to_its_own_ledger() {
    let s = all_admitted();
    let f = fixture();
    let labeller = Labeller::new(evaluator(), &Threads);
    let root: Shared = Arc::new(Mutex::new(s.root.lock().expect("unpoisoned").clone()));
    let mut ledger =
        Ledger::practice(MemLog::of(&root, Pair::Practice(0)), 0, pin()).expect("the ledger");
    let mut replay = |look| {
        forcing(
            |_| true,
            || {
                let replay = ledger
                    .begin_replay(look, &f.chain, &f.sets)
                    .expect("a replay");
                run_replay(&labeller, replay).expect("its figures")
            },
        )
    };
    let (one, two, early, late) = (replay(1), replay(1), replay(1), replay(3));
    let other = Ledger::practice(MemLog::of(&root, Pair::Practice(1)), 1, pin()).expect("a ledger");
    assert_eq!(
        other.replayed(one).err(),
        Some(LedgerRefusal::OutOfOrder),
        "another pair's ledger takes no replay of practice 0's"
    );
    let ledger =
        Ledger::practice(MemLog::of(&root, Pair::Practice(0)), 0, pin()).expect("the ledger");
    let runner = Ledger::runner(
        MemLog::of(&root, Pair::Practice(0)),
        Pair::Practice(0),
        pin(),
    )
    .expect("the runner's");
    assert_eq!(
        runner.replayed(two).err(),
        Some(LedgerRefusal::NoCapability),
        "the runner admits nothing"
    );
    assert_eq!(
        ledger.replayed(early).err(),
        Some(LedgerRefusal::OutOfOrder),
        "a replay the last line does not name"
    );
    let (admitted, figures) = ledger.replayed(late).expect("the last line's replay");
    assert_eq!(
        admitted.map(|a| (a.pair(), a.look())),
        Some((Pair::Practice(0), 3))
    );
    assert_eq!(figures.look(), 3);
    // The same pair under another version root: there every look was refused, so its look 3
    // names v2 against v0, where this root's names v2 against v1.
    let refused: Shared = Arc::new(Mutex::new(
        all_refused().root.lock().expect("unpoisoned").clone(),
    ));
    let mut theirs =
        Ledger::practice(MemLog::of(&refused, Pair::Practice(0)), 0, pin()).expect("a ledger");
    let mut ours = ledger;
    let elsewhere = forcing(
        |_| true,
        || {
            let replay = theirs.begin_replay(3, &f.chain, &f.sets).expect("a replay");
            run_replay(&labeller, replay).expect("its figures")
        },
    );
    assert_eq!(elsewhere.figures().comparator(), f.chain[0].id());
    let flipped = forcing(
        |_| true,
        || {
            let replay = theirs.begin_replay(1, &f.chain, &f.sets).expect("a replay");
            run_replay(&labeller, replay).expect("its figures")
        },
    );
    assert_eq!(
        theirs.replayed(flipped).err(),
        Some(LedgerRefusal::ReplayMismatch { look: 1 }),
        "a replayed bit that is not the recorded one is a defect"
    );
    let _own = ours.begin_replay(3, &f.chain, &f.sets).expect("a replay");
    assert_eq!(
        ours.replayed(elsewhere).err(),
        Some(LedgerRefusal::OutOfOrder),
        "a replay names its own ledger's look-begin"
    );

    let fresh = self::root();
    let open = |j| {
        let mut l =
            Ledger::practice(MemLog::of(&fresh, Pair::Practice(j)), j, pin()).expect("a ledger");
        l.open(Digest([1; 32]), BUILD, Digest([2; 32]), false)
            .expect("an open");
        l
    };
    let (mut zero, mut one) = (open(0), open(1));
    let (x, a) = look_pair(1, &[], &pin().ids).expect("look 1");
    let theirs = run_look(
        &labeller,
        zero.begin_look(1, x, a, &f.chain, &f.sets).expect("look 1"),
    )
    .expect("practice 0's figures");
    let ours = run_look(
        &labeller,
        one.begin_look(1, x, a, &f.chain, &f.sets).expect("look 1"),
    )
    .expect("practice 1's figures");
    assert_eq!(
        (theirs.candidate(), theirs.comparator()),
        (ours.candidate(), ours.comparator()),
        "the same ids under the shared chain"
    );
    assert_ne!(theirs.list(), ours.list(), "another set's list");
    assert_eq!(
        one.verdict(&theirs).err(),
        Some(LedgerRefusal::OutOfOrder),
        "another pair's figures"
    );
    let apart = self::root();
    let mut narrow =
        Ledger::practice(MemLog::of(&apart, Pair::Practice(1)), 1, pin()).expect("a ledger");
    narrow
        .open(Digest([1; 32]), BUILD, Digest([2; 32]), false)
        .expect("an open");
    let fewer = Sets::scaled(8, 12, 7, 6);
    let short = run_look(
        &labeller,
        narrow
            .begin_look(1, x, a, &f.chain, &fewer)
            .expect("look 1"),
    )
    .expect("figures over shorter pools");
    assert_eq!(short.pair(), ours.pair());
    assert_ne!(short.list(), ours.list());
    assert_eq!(
        one.verdict(&short).err(),
        Some(LedgerRefusal::OutOfOrder),
        "this pair's figures over another list"
    );
    let mut runner = Ledger::runner(
        MemLog::of(&fresh, Pair::Practice(1)),
        Pair::Practice(1),
        pin(),
    )
    .expect("the runner's");
    assert_eq!(
        runner.verdict(&ours).err(),
        Some(LedgerRefusal::NoCapability)
    );
    assert!(one.verdict(&ours).is_ok());
    assert_eq!(
        one.verdict(&ours).err(),
        Some(LedgerRefusal::OutOfOrder),
        "one verdict per look-begin"
    );
}

#[test]
fn the_runner_writes_only_void_and_record() {
    let f = fixture();
    let root = root();
    let log = MemLog::of(&root, Pair::Practice(0));
    let runner = || Ledger::runner(log.clone(), Pair::Practice(0), pin()).expect("the runner's");
    assert_eq!(
        runner().open(Digest([1; 32]), BUILD, Digest([2; 32]), false),
        Err(LedgerRefusal::NoCapability)
    );
    assert_eq!(
        root.lock().expect("unpoisoned").chain,
        None,
        "refused before the chain file"
    );
    let mut child = Ledger::practice(log.clone(), 0, pin()).expect("a ledger");
    child
        .open(Digest([1; 32]), BUILD, Digest([2; 32]), false)
        .expect("the child's open");
    let (x, a) = look_pair(1, &[], &pin().ids).expect("look 1");
    assert_eq!(
        runner().begin_look(1, x, a, &f.chain, &f.sets).err(),
        Some(LedgerRefusal::NoCapability)
    );
    assert_eq!(runner().retire(), Err(LedgerRefusal::NoCapability));
    assert_eq!(runner().void(VoidCause::Deadline), Ok(VoidClass::Stopped));
}

// ---- Sessions ----

#[test]
fn a_scaled_practice_session_reads_no_held_out_item() {
    let (ev, s) = natural();
    let relaxed = std::sync::atomic::Ordering::Relaxed;
    assert_eq!(
        ev.reads().held_out.load(relaxed),
        0,
        "no held-out item judged"
    );
    assert_eq!(
        ev.reads().looks.load(relaxed),
        0,
        "no held-out gate entered"
    );
    assert!(ev.reads().train.load(relaxed) > 0);
    assert_eq!(s.a.bits.len(), usize::from(LOOKS), "every look spent");
    assert!(s.a.report.is_some() && s.b.report.is_none());
    replays_equal(s);
    let lines = Ledger::practice(MemLog::of(&s.root, Pair::Practice(0)), 0, pin())
        .expect("the ledger")
        .lines()
        .to_vec();
    assert!(lines.contains(&Line::Retire));
    assert_eq!(
        lines
            .iter()
            .filter(|l| matches!(l, Line::Replay { .. }))
            .count(),
        2 * usize::from(LOOKS),
        "A's replays and B's"
    );
}

#[test]
fn each_arm_folds_its_rankers_top_k() {
    let (_, s) = natural();
    let f = fixture();
    let report = s.a.report.as_ref().expect("A's report");
    let set = SetId {
        tag: SetTag::PracticeReport,
        index: 0,
    };
    let pools = f.sets.pools(set).expect("practice-report 0");
    let mf = f
        .chain
        .iter()
        .find(|v| v.id() == s.a.mf)
        .expect("Mf is a member");
    let rankers = [
        (Arm::U, Ranker::Model(f.chain[0].state())),
        (Arm::W1, Ranker::W1),
        (Arm::W2, Ranker::W2),
        (Arm::C, Ranker::Model(f.chain[1].state())),
        (Arm::Mf, Ranker::Model(mf.state())),
        (Arm::Ctl, Ranker::Control),
    ];
    assert_eq!(report.folded.len(), rankers.len());
    for ((arm, folded), (want, ranker)) in report.folded.iter().zip(&rankers) {
        assert_eq!(arm, want);
        assert_eq!(folded.len(), pools.len());
        for (p, (ids, plans)) in folded.iter().zip(&pools).enumerate() {
            let pool = PoolId {
                set,
                pool: u32::try_from(p).expect("small"),
            };
            let top = ranker.top_k(&pool, plans).expect("4");
            assert_eq!(*ids, top.map(|x| x.digest()), "{arm:?} pool {p}");
            if let Some(winner) = report.arm(*arm)[p].plan() {
                assert!(
                    top.contains(&winner),
                    "{arm:?} pool {p} folded its own top_k"
                );
            }
        }
        assert_eq!(report.arm(*arm).len(), pools.len());
    }
    assert_eq!(report.arm(Arm::O).len(), pools.len());
    for against in [Arm::U, Arm::W2, Arm::W1] {
        let c = Comparison::of(report.arm(Arm::Mf), report.arm(against));
        assert_eq!(report.criterion(against), (c, c.shows()), "{against:?}");
    }
}

#[test]
fn the_grid_reproduces_m4s_training_record() {
    let grid = Sets::v1().grid();
    assert_eq!(grid.len(), 54);
    let bytes: Vec<[u8; 12]> = grid.iter().map(SchedulePlan::encode).collect();
    let labels = evaluator()
        .label_spread(LabelScope::Train, &bytes, &Threads)
        .expect("the grid's labels");
    assert_eq!(GridRecord::of(&labels), GridRecord::m4(), "G1's grid check");
}

#[test]
fn the_chain_is_independent_of_admission() {
    let f = fixture();
    for s in [all_admitted(), all_refused(), control_admitted()] {
        assert_eq!(s.a.chain, f.chain);
        assert_eq!(s.b.chain, f.chain);
    }
}

#[test]
fn forced_admissions_publish_rollback_and_replay() {
    let s = all_admitted();
    let f = fixture();
    replays_equal(s);
    assert_eq!(s.a.bits, vec![true; 5]);
    assert_eq!(s.a.mf, f.chain[4].id(), "v1..v4 each admitted in turn");
    assert_eq!(
        s.a.r2,
        Some(3),
        "R2 rolls back to v3, the latest of v1..v3 admitted"
    );
    for run in [&s.a, &s.b] {
        let moves = names(&run.moves);
        assert_eq!(
            moves[moves.len() - 6..],
            [
                "rollback", "retrain", "retrain", "rollback", "retrain", "rollback"
            ],
            "R1, R2 and Mf restored"
        );
    }
    let rolled: Vec<Digest> = s
        .store_a
        .revisions()
        .iter()
        .filter(|r| r.reason == Reason::Rollback)
        .map(|r| r.active)
        .collect();
    assert_eq!(rolled, [f.chain[0].id(), f.chain[3].id(), f.chain[4].id()]);
    let admits = |st: &Store<MemStore>| -> Vec<Option<u8>> {
        st.revisions()
            .iter()
            .filter(|r| r.reason == Reason::Admit)
            .map(|r| r.look)
            .collect()
    };
    assert_eq!(admits(&s.store_a), [Some(1), Some(3), Some(4), Some(5)]);
    assert_eq!(
        admits(&s.store_b),
        admits(&s.store_a),
        "each published once"
    );
    assert_eq!(s.store_a.pointer().active, s.a.mf, "Mf restored");
    for st in [&s.store_a, &s.store_b] {
        let loaded = Store::load(st.io().clone(), BUILD).expect("it reloads");
        assert_eq!(loaded.pointer(), st.pointer());
        assert_eq!(loaded.revisions(), st.revisions());
    }
}

#[test]
fn a_replayed_admission_publishes_once() {
    let s = all_admitted();
    let f = fixture();
    let root: Shared = Arc::new(Mutex::new(s.root.lock().expect("unpoisoned").clone()));
    let mut ledger =
        Ledger::practice(MemLog::of(&root, Pair::Practice(0)), 0, pin()).expect("the ledger");
    let labeller = Labeller::new(evaluator(), &Threads);
    let admitted = forcing(
        |_| true,
        || {
            let replay = ledger
                .begin_replay(1, &f.chain, &f.sets)
                .expect("look 1 replays");
            let replayed = run_replay(&labeller, replay).expect("its figures");
            ledger.replayed(replayed).expect("the recorded bit").0
        },
    )
    .expect("look 1 admitted");
    let mut b = Store::load(s.store_b.io().clone(), BUILD).expect("B's store");
    assert_eq!(
        b.activate(admitted, ledger.head()),
        Err(StoreRefusal::Republished { look: 1 })
    );
}

#[test]
fn an_admitted_control_publishes_nothing() {
    let s = control_admitted();
    replays_equal(s);
    assert_eq!(s.a.bits, [false, true, false, false, false]);
    for run in [&s.a, &s.b] {
        assert!(
            !names(&run.moves).contains(&"activate"),
            "no revision at step 6"
        );
        assert_eq!(run.mf, fixture().chain[0].id());
        assert_eq!(run.r2, None, "R2 not exercised");
        let moves = names(&run.moves);
        assert_eq!(
            moves[moves.len() - 4..],
            ["rollback", "retrain", "retrain", "rollback"],
            "R1 and Mf restored, no R2"
        );
    }
    assert_eq!(names(&s.a.moves), names(&s.b.moves));
    let refused = all_refused();
    replays_equal(refused);
    assert_eq!(refused.a.bits, [false; 5]);
}

#[test]
fn a_look_runs_only_through_the_ledger_list() {
    let f = fixture();
    let root = root();
    let mut ledger =
        Ledger::practice(MemLog::of(&root, Pair::Practice(0)), 0, pin()).expect("a ledger");
    ledger
        .open(Digest([1; 32]), BUILD, Digest([2; 32]), false)
        .expect("an open");
    let (x, a) = look_pair(1, &[], &pin().ids).expect("look 1");
    let look = ledger
        .begin_look(1, x, a, &f.chain, &f.sets)
        .expect("look 1 begins");
    let figures = run_look(&Labeller::new(evaluator(), &Threads), look).expect("its figures");
    assert_eq!(figures.pools().len(), 12);
    assert_eq!((figures.candidate(), figures.comparator()), (x, a));
    assert_eq!(figures.bit(), figures.verdict().is_ok());
    assert_eq!(
        figures.verdict(),
        figures.comparison().admits(),
        "a look is decided at the look level"
    );
    let (xs, as_): (Vec<_>, Vec<_>) = figures.pools().iter().copied().unzip();
    assert_eq!(figures.comparison(), Comparison::of(&xs, &as_));
    let Some(Line::LookBegin { list, .. }) = ledger.lines().last() else {
        panic!("the look-begin line");
    };
    assert_eq!((figures.pair(), figures.list()), (Pair::Practice(0), *list));
    let bytes = figures.encode();
    for id in [x, a, *list] {
        assert!(
            bytes.windows(32).any(|w| w == id.0),
            "the figures file names {id:?}"
        );
    }
}

#[test]
fn a_session_runs_only_on_its_pairs_store_and_build() {
    let f = fixture();
    let env = SessionEnv {
        build: BUILD,
        sets: &f.sets,
        labeller: Labeller::new(evaluator(), &Threads),
        chain: pin(),
        tests_log: Digest([7; 32]),
    };
    let root = root();
    let mut ledger =
        Ledger::practice(MemLog::of(&root, Pair::Practice(0)), 0, pin()).expect("a ledger");
    let mut theirs = Store::init(
        MemStore::default(),
        BUILD,
        Pair::Practice(1),
        9,
        Digest([0; 32]),
    )
    .expect("practice 1's store");
    let foreign = Some(SessionRefusal::Store(StoreRefusal::ForeignPair));
    assert_eq!(session_a(&env, &mut ledger, &mut theirs).err(), foreign);
    assert_eq!(session_b(&env, &mut ledger, &mut theirs).err(), foreign);
    let elsewhere = SessionEnv {
        build: "another-build",
        ..env
    };
    let mut ours = store(10);
    assert_eq!(
        session_a(&elsewhere, &mut ledger, &mut ours).err(),
        Some(SessionRefusal::Store(StoreRefusal::ForeignBuild {
            found: BUILD.to_owned()
        }))
    );
    assert!(ledger.lines().is_empty(), "stopped before any line");
    assert_eq!(theirs.revisions().len(), 1, "nothing trained");
    assert_eq!(ours.revisions().len(), 1, "nothing trained");
}

// ---- The store ----

#[test]
fn a_reproducing_train_moves_the_head() {
    let f = fixture();
    let labeller = Labeller::new(evaluator(), &Threads);
    let mut st = store(3);
    for want in &f.chain[1..] {
        let (v, moved) = st
            .train(&f.sets, &labeller, Digest([0; 32]))
            .expect("a train");
        assert_eq!(&v, want);
        assert!(
            moved
                .written
                .iter()
                .any(|(n, _)| n.starts_with("versions/"))
        );
        assert_eq!(st.pointer().head, want.id());
        assert_eq!(
            st.pointer().active,
            f.chain[0].id(),
            "training never changes the active id"
        );
    }
    assert!(
        st.train(&f.sets, &labeller, Digest([0; 32])).is_err(),
        "no round 5"
    );
    let moved = st.rollback(f.chain[0].id(), Digest([0; 32])).expect("v0");
    assert_eq!(moved.read.len(), 4, "its four files re-read");
    let (v, moved) = st
        .train(&f.sets, &labeller, Digest([0; 32]))
        .expect("v1 again");
    assert_eq!(v, f.chain[1]);
    assert!(
        !moved
            .written
            .iter()
            .any(|(n, _)| n.starts_with("versions/")),
        "a reproduced version writes no version file"
    );
    assert_eq!(st.pointer().head, f.chain[1].id(), "but moves the head");
    assert_eq!(
        st.rollback(f.chain[2].id(), Digest([0; 32])),
        Err(StoreRefusal::NotAdmitted)
    );
    assert_eq!(
        st.rollback(Digest([0xAB; 32]), Digest([0; 32])),
        Err(StoreRefusal::NotRetained)
    );
    let loaded = Store::load(st.io().clone(), BUILD).expect("a round trip from its bytes");
    assert_eq!(loaded.pointer(), st.pointer());
    assert_eq!(loaded.revisions(), st.revisions());
    assert_eq!(loaded.versions(), st.versions());
}

#[test]
fn each_store_load_check_refuses() {
    let v0 = fixture().chain[0].id();
    let mut st = store(4);
    st.rollback(v0, Digest([0; 32])).expect("a rollback");
    let io = st.io().clone();
    assert!(Store::load(io.clone(), BUILD).is_ok());
    assert_eq!(
        Store::load(io.clone(), "another-build").err(),
        Some(StoreRefusal::ForeignBuild {
            found: BUILD.to_owned()
        })
    );
    assert_eq!(
        Store::load(io.clone(), "another-build")
            .and_then(|mut s| s.rollback(v0, Digest([0; 32])))
            .err(),
        Some(StoreRefusal::ForeignBuild {
            found: BUILD.to_owned()
        }),
        "a rollback in a store of another build"
    );
    let mut policy = io.clone();
    let text = String::from_utf8(policy.files["store.txt"].clone()).expect("text");
    let m6 = text
        .lines()
        .find(|l| l.starts_with("m6-policy "))
        .expect("its policy line")
        .to_owned();
    let flipped = format!("m6-policy {}", "0".repeat(64));
    policy.files.insert(
        "store.txt".to_owned(),
        text.replace(&m6, &flipped).into_bytes(),
    );
    assert_eq!(
        Store::load(policy, BUILD).err(),
        Some(StoreRefusal::PolicyMismatch)
    );
    let mut torn = MemStore::stopping(Step::Revision, 2);
    torn = {
        let mut s = Store::init(torn, BUILD, Pair::Practice(0), 5, Digest([0; 32]))
            .expect("revision 0 is not stopped");
        assert_eq!(
            s.rollback(v0, Digest([0; 32])),
            Err(StoreRefusal::Io(StoreFault::Stopped(Step::Revision)))
        );
        s.io().clone()
    };
    assert_eq!(
        Store::load(torn, BUILD).err(),
        Some(StoreRefusal::StoreTorn)
    );
    let dir = format!("versions/{}", v0.hex());
    for (file, what) in [
        (format!("{dir}/state"), dir.clone()),
        ("revisions/0001".to_owned(), "revisions/0001".to_owned()),
    ] {
        let mut bad = io.clone();
        bad.files.get_mut(&file).expect("the file")[0] ^= 1;
        assert_eq!(
            Store::load(bad, BUILD).err(),
            Some(StoreRefusal::StoreCorrupt { file: what })
        );
    }
    let mut gone = io;
    gone.files.remove("active");
    assert_eq!(
        Store::load(gone, BUILD).err(),
        Some(StoreRefusal::StoreCorrupt {
            file: "active".to_owned()
        })
    );
}

#[test]
fn the_store_budgets_hold() {
    let v0 = fixture().chain[0].id();
    let mut st = store(6);
    while st.revisions().len() < MAX_REVISIONS {
        st.rollback(v0, Digest([0; 32])).expect("under the budget");
    }
    assert_eq!(
        st.rollback(v0, Digest([0; 32])),
        Err(StoreRefusal::Budget {
            what: "RevisionBudget"
        })
    );
    let mut st = store(7);
    let mut k = 0;
    while st.versions().len() < MAX_VERSIONS {
        k += 1;
        st.keep_version(&fixture().chain[0].altered(Alter::Weight(k)))
            .expect("under the budget");
    }
    assert_eq!(
        st.keep_version(&fixture().chain[0].altered(Alter::Weight(k + 1))),
        Err(StoreRefusal::Budget { what: "versions" })
    );
    let s = all_refused();
    let figures = s.a.figures[0].clone();
    let record = EvalRecord::of(Pair::Practice(0).looks(), &figures);
    let mut a = Store::load(s.store_a.io().clone(), BUILD).expect("A's store");
    for look in [0, LOOKS + 1] {
        assert_eq!(
            a.write_eval(&EvalRecord { look, ..record }),
            Err(StoreRefusal::Budget {
                what: "an eval record"
            }),
            "look {look}"
        );
        assert_eq!(
            a.write_figures(&figures.clone().relooked(look)),
            Err(StoreRefusal::Budget {
                what: "a figures file"
            }),
            "look {look}"
        );
    }
    let exists = |name: &str| Err(StoreRefusal::Io(StoreFault::Exists(name.to_owned())));
    assert_eq!(
        a.write_eval(&record),
        exists("eval/1.rec"),
        "one record a look"
    );
    assert_eq!(
        a.write_figures(&figures),
        exists("eval/1.figures"),
        "one figures file a look"
    );
    let report = s.a.report.as_ref().expect("A's report");
    assert_eq!(a.write_report(report).map(|_| ()), exists("report.txt"));
    let mut other = Store::init(
        MemStore::default(),
        BUILD,
        Pair::Practice(1),
        8,
        Digest([0; 32]),
    )
    .expect("practice 1's store");
    assert_eq!(other.write_eval(&record), Err(StoreRefusal::ForeignPair));
    assert_eq!(
        other.write_figures(&figures),
        Err(StoreRefusal::ForeignPair)
    );
}

// ---- Exchangeability ----

#[test]
fn the_control_is_exchangeable_with_v0() {
    let set = SetId {
        tag: SetTag::PracticeHeldOut,
        index: 0,
    };
    let v0 = State::base();
    let (mut overlap, mut rank_sum, mut picks) = (0_usize, 0_usize, 0_usize);
    for (p, plans) in set_pools(set)
        .expect("practice-held-out 0")
        .iter()
        .enumerate()
    {
        let pool = PoolId {
            set,
            pool: u32::try_from(p).expect("small"),
        };
        let base = Ranker::Model(&v0).order(&pool, plans);
        let control = Ranker::Control.order(&pool, plans);
        assert_ne!(base, control, "pool {p}: the control is not v0's order");
        for plan in &control[..JUDGED] {
            overlap += usize::from(base[..JUDGED].contains(plan));
            rank_sum += base.iter().position(|b| b == plan).expect("a pool plan");
            picks += 1;
        }
    }
    // Under exchangeability the expected overlap is K²/N per pool (16 in all) and the control's
    // picks sit at v0's middle rank (N - 1)/2 = 31.5 on average.
    assert!((4..=40).contains(&overlap), "{overlap}");
    let mean = rank_sum as u64 * 10 / picks as u64;
    assert!((235..=395).contains(&mean), "mean rank {mean}/10");
}

// ---- Source scans ----

/// This crate's root.
fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// What each file the import allowlist reads may `use` from this crate: a path, or every item of
/// a module (a path ending `::`).
fn allowlist(file: &str) -> Vec<String> {
    let m6_policy: &[&str] = &[
        "super::corpus::SplitMix",
        "super::plan::SchedulePlan",
        "super::plan::Split",
        "super::plan::Wake",
        "super::plan::PlanRefusal",
        "super::plan::MAX_GAP_US",
        "super::plan::MIN_PERIOD_US",
        "super::plan::MAX_PERIOD_US",
        "super::policy::AcceptancePolicy",
        "super::policy::GRID_V1",
        "super::policy::M4_POLICY_V1_DIGEST",
        "super::policy::hex_digest",
    ];
    let m4: &[&str] = &[
        "Evaluator",
        "Spread",
        "TrainLabel",
        "LabelScope",
        concat!("Real", "Sets"),
        "PracticeSet",
        "LookToken",
        "ReadToken",
        "ReplayToken",
        "PoolOutcome",
        "LabelRefusal",
        "MintRefusal",
        "list_digest",
    ];
    match file {
        "m6_policy.rs" => m6_policy.iter().map(|s| (*s).to_owned()).collect(),
        "m6.rs" => {
            let mut out: Vec<String> = m4.iter().map(|i| format!("super::m4::{i}")).collect();
            out.extend(
                [
                    "super::m6_policy::",
                    "super::plan::SchedulePlan",
                    "super::policy::GRID_V1",
                    "super::corpus::SplitMix",
                ]
                .map(str::to_owned),
            );
            out
        }
        "aos_learn.rs" | "aos_learn_bench.rs" => {
            let effects = concat!("aterm_", "effects::aos_momentum::");
            let mut out: Vec<String> = [
                "m4::Evaluator",
                "m4::Spread",
                concat!("m4::Real", "Sets"),
                "m4::held_out_judged",
                "m6::",
                "m6_policy::",
            ]
            .iter()
            .map(|i| format!("{effects}{i}"))
            .collect();
            out.extend(
                [
                    "crate::Res",
                    "crate::Fail",
                    "crate::lab_build::LAB_BUILD",
                    "crate::lab_build::check_build",
                    "crate::spread::Threads",
                    "crate::sys::digest_file",
                    "crate::sys::await_exit",
                    "crate::stop::install",
                    "crate::stop::ring",
                    "crate::paths::paths",
                    "crate::paths::from_caller",
                    "crate::instance::take_value",
                    "crate::aos_gpu_bench::watch",
                    "crate::aos_gpu_bench::Watch",
                    "crate::aos_gpu_bench::clean_commit",
                    "crate::trail_gallery::checkouts",
                    "crate::trail_gallery::resolve_out",
                    // Reviewed in stage D: the child's footprint self-report (G4).
                    "crate::sys::max_footprint",
                ]
                .map(str::to_owned),
            );
            if file == "aos_learn_bench.rs" {
                // The ledger's file I/O, as designed, and (reviewed in stage D) the store's, for
                // G2's comparison, the children's summary, the exit's held_out_judged guard, the
                // durable directory maker, and the stop rule's check and sleep, so an interrupt
                // ends the runner's wait for the ledger and voids its session.
                out.extend(
                    [
                        "crate::aos_learn::LabLog",
                        "crate::aos_learn::LabStore",
                        "crate::aos_learn::Summary",
                        "crate::aos_learn::guarded",
                        "crate::aos_learn::make_dirs",
                        "crate::stop::check",
                        "crate::stop::sleep",
                    ]
                    .map(str::to_owned),
                );
            }
            out
        }
        _ => Vec::new(),
    }
}

/// Every `use` item of `src` outside comments, whitespace removed, braces expanded into paths.
fn use_paths(src: &str) -> Result<Vec<String>, String> {
    let code: String = src
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut out = Vec::new();
    let mut rest = code.as_str();
    while let Some(at) = find_use(rest) {
        let item = &rest[at..];
        let end = item.find(';').ok_or("an unterminated use")?;
        let body: String = item[4..end]
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        if body.contains(concat!("a", "s")) && item[4..end].split_whitespace().any(|w| w == "as") {
            return Err(format!("an `as` rename: use {body}"));
        }
        expand("", &body, &mut out)?;
        rest = &item[end..];
    }
    Ok(out)
}

/// The start of the next `use ` that begins an item: at a line's start after any indent, after
/// `;`, a brace or an attribute's `]`, or after `pub` or `pub(…)`.
fn find_use(s: &str) -> Option<usize> {
    let mut from = 0;
    while let Some(i) = s[from..].find("use ") {
        let at = from + i;
        let before = s[..at].trim_end_matches([' ', '\t']);
        if before.is_empty()
            || before.ends_with(['\n', ';', '{', '}', ']'])
            || before.ends_with("pub")
            || before.ends_with(')')
        {
            return Some(at);
        }
        from = at + 4;
    }
    None
}

/// `prefix` then `body`'s paths, each brace group expanded.
fn expand(prefix: &str, body: &str, out: &mut Vec<String>) -> Result<(), String> {
    let Some(open) = body.find('{') else {
        for part in body.split(',').filter(|p| !p.is_empty()) {
            out.push(format!("{prefix}{part}"));
        }
        return Ok(());
    };
    let head = &body[..open];
    let inner = body[open + 1..]
        .strip_suffix('}')
        .ok_or_else(|| format!("an unbalanced group: {body}"))?;
    let mut depth = 0;
    let mut start = 0;
    for (i, c) in inner.char_indices() {
        match c {
            '{' => depth += 1,
            '}' => depth -= 1,
            ',' if depth == 0 => {
                expand(&format!("{prefix}{head}"), &inner[start..i], out)?;
                start = i + 1;
            }
            _ => {}
        }
    }
    expand(&format!("{prefix}{head}"), &inner[start..], out)
}

/// The banned substrings (design §13.3), built so this file does not hold them.
const BANNED: &[&str] = &[
    concat!("held_", "out("),
    concat!("admit_", "one"),
    concat!(".sea", "rch("),
    concat!("Selec", "tion"),
    concat!("CorpusSplit::", "HeldOut"),
    concat!("HELD_OUT_", "SEEDS"),
    concat!("s", "0("),
    concat!("scena", "rio("),
    concat!("Corpus", "::"),
    concat!(".jud", "ge("),
    concat!("row_", "at("),
    concat!("training_", "rows"),
    concat!("meas", "ure("),
    concat!("indi", "ces("),
    concat!("Candidate", "Bundle"),
    concat!("bundle", "::"),
    concat!("lane", "::"),
    concat!("aos_", "publish"),
    concat!("aos_", "search"),
    concat!("aos_", "workers"),
];

/// `src`'s refusals under the import allowlist `allowed` and the banned substrings.
fn scan_file(src: &str, allowed: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    match use_paths(src) {
        Ok(paths) => {
            for p in paths {
                let ours = [
                    "super::",
                    "crate::",
                    "self::",
                    concat!("aterm_", "effects::"),
                ]
                .iter()
                .any(|r| p.starts_with(r));
                let listed = allowed
                    .iter()
                    .any(|a| *a == p || (a.ends_with("::") && p.starts_with(a.as_str())));
                if ours && !listed {
                    out.push(format!("use {p}"));
                }
            }
        }
        Err(e) => out.push(e),
    }
    let squeezed: String = src.chars().filter(|c| !c.is_whitespace()).collect();
    for b in BANNED {
        if squeezed.contains(b) {
            out.push(format!("banned {b}"));
        }
    }
    let code: String = outside_uses(&tight(src))
        .lines()
        .filter(|l| !(l.trim_start().starts_with("//!") || l.trim_start().starts_with("///")))
        .collect::<Vec<_>>()
        .join("\n");
    for root in [
        "super::",
        "crate::",
        "self::",
        concat!("aterm_", "effects::"),
    ] {
        if code
            .match_indices(root)
            .any(|(i, _)| !code[..i].ends_with(|c: char| c.is_alphanumeric() || c == '_'))
        {
            out.push(format!("a {root} path outside a use item"));
        }
    }
    let evaluator = concat!("Evaluator", "::");
    let ctor = concat!("Evaluator", "::v1(");
    if squeezed.matches(evaluator).count() != squeezed.matches(ctor).count() {
        out.push(format!("{evaluator} other than {ctor}"));
    }
    // The evaluator's own reads, by name, however the path to them is spelt.
    for word in [
        concat!("jud", "ge"),
        concat!("row_", "at"),
        concat!("training_", "rows"),
    ] {
        if has_word(src, word) {
            out.push(format!("names {word}"));
        }
    }
    for float in [concat!("f", "32"), concat!("f", "64")] {
        if words(src).any(|w| {
            w.split('_').any(|part| part == float)
                || (w.ends_with(float) && w.starts_with(|c: char| c.is_ascii_digit()))
        }) {
            out.push(format!("banned {float}"));
        }
    }
    if has_float_literal(src) {
        out.push("a float literal".to_owned());
    }
    out
}

/// Whether `src`'s code (outside comments and string literals) holds a float literal: digits, not
/// after an identifier or a `.` (so `x.0.1` is not), then a `.` not followed by a `.` or an
/// identifier (so `0..1` and `1.max(2)` are not), or a decimal exponent (`1e6`, not `0x1e6`).
fn has_float_literal(src: &str) -> bool {
    src.lines().any(|line| {
        let code = line.split("//").next().unwrap_or("");
        let mut bare = String::new();
        let mut quoted = false;
        let mut escaped = false;
        for c in code.chars() {
            match (quoted, escaped, c) {
                (true, true, _) => escaped = false,
                (true, false, '\\') => escaped = true,
                (_, false, '"') => quoted = !quoted,
                (false, _, c) => bare.push(c),
                _ => {}
            }
        }
        let b = bare.as_bytes();
        (0..b.len()).any(|i| {
            let led = i == 0
                || !(b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_' || b[i - 1] == b'.');
            let mut j = i;
            while j < b.len() && (b[j].is_ascii_digit() || (j > i && b[j] == b'_')) {
                j += 1;
            }
            let point = b.get(j) == Some(&b'.')
                && !b
                    .get(j + 1)
                    .is_some_and(|&c| c == b'.' || c == b'_' || c.is_ascii_alphabetic());
            let signed = usize::from(matches!(b.get(j + 1), Some(b'+' | b'-')));
            let exponent = matches!(b.get(j), Some(b'e' | b'E'))
                && b.get(j + 1 + signed).is_some_and(u8::is_ascii_digit);
            led && j > i && (point || exponent)
        })
    })
}

/// Whether `word` is among `text`'s words.
fn has_word(text: &str, word: &str) -> bool {
    words(text).any(|w| w == word)
}

/// `src`'s identifier-like words.
fn words(src: &str) -> impl Iterator<Item = &str> {
    src.split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .filter(|w| !w.is_empty())
}

/// `src` with the whitespace around each `::` removed (`super ::x` is `super::x`).
fn tight(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut rest = src;
    while let Some(at) = rest.find("::") {
        out.push_str(rest[..at].trim_end());
        out.push_str("::");
        rest = rest[at + 2..].trim_start();
    }
    out.push_str(rest);
    out
}

/// `code` with each `use` item cut out.
fn outside_uses(code: &str) -> String {
    let mut out = String::with_capacity(code.len());
    let mut rest = code;
    while let Some(at) = find_use(rest) {
        out.push_str(&rest[..at]);
        let end = rest[at..].find(';').map_or(rest.len(), |e| at + e + 1);
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

#[test]
fn m6_imports_only_its_allowlist() {
    for file in ["m6.rs", "m6_policy.rs"] {
        let src = std::fs::read_to_string(crate_root().join("src/aos_momentum").join(file))
            .expect("a readable source");
        assert_eq!(
            scan_file(&src, &allowlist(file)),
            Vec::<String>::new(),
            "{file}"
        );
    }
}

#[test]
fn the_lab_files_import_only_their_allowlist() {
    let lab = crate_root().join("../aterm-lab/src");
    for file in ["aos_learn.rs", "aos_learn_bench.rs"] {
        let src = std::fs::read_to_string(lab.join(file)).expect("a readable source");
        assert_eq!(
            scan_file(&src, &allowlist(file)),
            Vec::<String>::new(),
            "{file}"
        );
    }
}

#[test]
fn the_lab_allowlists_refuse_what_they_do_not_list() {
    let learn = allowlist("aos_learn.rs");
    let bench = allowlist("aos_learn_bench.rs");
    let refused = |src: &str, allowed: &[String]| !scan_file(src, allowed).is_empty();
    for ok in [
        concat!(
            "use aterm_",
            "effects::aos_momentum::m6::{Ledger, Store};\n"
        ),
        concat!(
            "use aterm_",
            "effects::aos_momentum::m4::{Evaluator, held_out_judged};\n"
        ),
        "use crate::{Fail, Res};\nuse crate::sys::{digest_file, max_footprint};\n",
        "use aterm_aos::Digest;\nuse std::fs;\n",
    ] {
        assert!(!refused(ok, &learn), "{ok}");
        assert!(!refused(ok, &bench), "{ok}");
    }
    assert!(refused("use crate::aos_learn::LabLog;\n", &learn));
    assert!(refused("use crate::stop::sleep;\n", &learn));
    assert!(!refused(
        "use crate::aos_learn::{LabLog, LabStore, Summary, guarded, make_dirs};\n\
         use crate::stop::{check, install, ring, sleep};\n",
        &bench
    ));
    for planted in [
        concat!("use aterm_", "effects::aos_momentum::m4::TrainLabel;\n"),
        concat!("use aterm_", "effects::aos_momentum::policy::GRID_V1;\n"),
        concat!("use aterm_", "effects::aos_momentum::", "m6 as k;\n"),
        "use crate::sys::kill;\n",
        "use crate::aos_learn::version_root;\n",
        concat!("use crate::aos_", "search::LAB_BUILD;\n"),
        concat!("fn f() { let _ = crate::", "sys::kill(1, 9); }\n"),
        concat!(
            "fn f() { let _ = aterm_",
            "effects::aos_momentum::policy::measure; }\n"
        ),
    ] {
        assert!(refused(planted, &bench), "{planted}");
        assert!(refused(planted, &learn), "{planted}");
    }
}

#[test]
fn the_import_scan_refuses_its_planted_fixtures() {
    let allowed = allowlist("m6.rs");
    let refused = |src: &str| !scan_file(src, &allowed).is_empty();
    assert!(!refused(
        "use super::m4::{Evaluator, TrainLabel};\nfn f() {}\n"
    ));
    assert!(
        !refused("use super::m6_policy::JUDGED;\n"),
        "m6_policy is not this crate's root path"
    );
    for planted in [
        concat!("use super::m4::", "Selection;\n"),
        concat!("use super::m4::{Evaluator ", "as E};\n"),
        concat!("use super::search::", "GridProposer;\n"),
        concat!("pub(super) use crate::aos_momentum::m4::", "Serial;\n"),
        concat!("fn f() { let _ = super::", "policy::x(); }\n"),
        concat!("fn f(ev: &E) { ev. ", "judge(1); }\n"),
        concat!("fn f() { ", "Evaluator::select(); }\n"),
        concat!("fn f(a: u64) -> ", "f64 { a as ", "f64 }\n"),
        concat!("fn f(a: u32) -> ", "f32 { a as ", "f32 }\n"),
        concat!("fn f(d: D) -> u64 { d.as_secs_", "f64() as u64 }\n"),
        concat!("const N: u64 = 1", "e6 as u64;\n"),
        concat!("const N: u64 = 2", "E-3 as u64;\n"),
        concat!("const H: u64 = (1. / 2", ".) as u64;\n"),
        concat!("fn f() { let _ = held_", "out (1); }\n"),
        concat!("fn f(p: &P) { p. ", "search(1); }\n"),
        concat!("fn f() { let _ = super ", "::search::X; }\n"),
        concat!("fn f() {\n\tuse super::", "search::X;\n}\n"),
        concat!("use super::m4::Evaluator; use super::", "search::X;\n"),
        concat!("const X: &str = \"", "held_out(\";\n"),
        concat!("use self::", "super::Sched;\n"),
        concat!("fn f(ev: &E) { <Evaluator>::", "judge(ev, 1); }\n"),
        concat!(
            "type E = Evaluator;\nfn f(ev: &E) { E::",
            "row_at(ev, 0); }\n"
        ),
        concat!("const X: u8 = 0.5", "f64 as u8;\n"),
        concat!("const X: u32 = 2", "f32 as u32;\n"),
        concat!("fn f() -> u64 { (0.7 * 3", ".0) as u64 }\n"),
    ] {
        assert!(refused(planted), "{planted}");
    }
    for allowed in [
        "fn f() -> u32 { 32 }\n// of 32\n",
        "fn f(x: ((u8, u8), u8)) -> u8 { (x.0).1 + x.0.1 }\n",
        "fn f() { for _ in 0..1 {} for _ in 1..=2 {} }\n",
        "const S: &str = \"v1.0\"; // 2.5\n",
        "fn f(t: (u32, u32)) -> u32 { 1.max(2) + 0x1e3 + 1_000 + t.0.max(t.1) }\n",
        "fn f() { use super::m4::Evaluator; }\n",
        "fn f() {\n\tuse super::m4::Evaluator;\n}\n",
    ] {
        assert!(!refused(allowed), "{allowed}");
    }
}

/// Every workspace `.rs` file outside targets, `vendor` and dot-directories (`.git`, and the
/// export a release stages in `publish/.out`), without following links.
fn workspace_sources() -> Vec<(PathBuf, String)> {
    fn walk(dir: &Path, out: &mut Vec<(PathBuf, String)>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let name = entry.file_name().to_string_lossy().into_owned();
            if kind.is_symlink() {
                continue;
            }
            if kind.is_dir() {
                if !(name.starts_with("target") || name.starts_with('.') || name == "vendor") {
                    walk(&path, out);
                }
            } else if name.ends_with(".rs")
                && let Ok(text) = std::fs::read_to_string(&path)
            {
                out.push((path, text));
            }
        }
    }
    let mut out = Vec::new();
    walk(&crate_root().join("../.."), &mut out);
    out
}

/// A workspace file's path from the workspace root, `/`-separated.
fn rel(path: &Path) -> String {
    path.strip_prefix(crate_root().join("../.."))
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

const M4_RS: &str = "crates/aterm-effects/src/aos_momentum/m4.rs";
const M4_TESTS_RS: &str = "crates/aterm-effects/src/aos_momentum/m4_tests.rs";
const M6_RS: &str = "crates/aterm-effects/src/aos_momentum/m6.rs";
const LEARN_RS: &str = "crates/aterm-lab/src/aos_learn.rs";
const LEARN_BENCH_RS: &str = "crates/aterm-lab/src/aos_learn_bench.rs";
const MOMENTUM_RS: &str = "crates/aterm-effects/src/aos_momentum.rs";

/// The files that may name m6 (design §13.3), by path.
const M6_NAMERS: &[&str] = &[
    LEARN_RS,
    LEARN_BENCH_RS,
    M6_RS,
    "crates/aterm-effects/src/aos_momentum/m6_tests.rs",
    "crates/aterm-effects/src/aos_momentum/locks_tests.rs",
];

/// `src`'s refusals under the workspace scan of m6's path, for the file at `path`.
fn scan_workspace_file(path: &str, src: &str) -> Vec<String> {
    let mut out = Vec::new();
    let squeezed: String = src.chars().filter(|c| !c.is_whitespace()).collect();
    // Paths with the whitespace around `::` removed, each matched as whole words.
    let paths = tight(src);
    let word = |c: char| c.is_alphanumeric() || c == '_';
    for token in [concat!("aos_momentum::", "m6"), concat!("super::", "m6")] {
        for (i, _) in paths.match_indices(token) {
            if !paths[..i].ends_with(word) && !paths[i + token.len()..].starts_with(word) {
                out.push(format!("the path {token}"));
            }
        }
    }
    let m6 = concat!("m", "6");
    let in_crate = path.starts_with("crates/aterm-effects/src/");
    let flat: String = src.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut rest = flat.as_str();
    while let Some(at) = rest.find("use ") {
        let item = &rest[at..];
        let led = !rest[..at].ends_with(|c: char| c.is_alphanumeric() || c == '_');
        let end = item.find(';').unwrap_or(item.len());
        let body = &item[..end];
        let rooted = ["use super::", "use self::", "use crate::"]
            .iter()
            .any(|r| body.starts_with(r));
        if led && body.contains("aos_momentum") {
            if has_word(body, m6) {
                out.push(format!("{body}: names m6"));
            }
            if body.contains('*') {
                out.push(format!("{body}: a glob"));
            }
            if has_word(body, concat!("a", "s")) {
                out.push(format!("{body}: an `as` rename"));
            }
        } else if led && in_crate && rooted && has_word(body, m6) {
            out.push(format!("{body}: names m6"));
        }
        rest = &item[end..];
    }
    // In this crate, m6's own path however it is reached (`use super::*;`, then `m6::`).
    let bare = concat!("m", "6::");
    if in_crate
        && paths
            .match_indices(bare)
            .any(|(i, _)| !paths[..i].ends_with(word))
    {
        out.push(format!("the path {bare}"));
    }
    // The parent declares m6 and names it nowhere else, so it re-exports none of it.
    if path == MOMENTUM_RS
        && words(src).filter(|w| *w == m6).count() > squeezed.matches(concat!("mod", "m6;")).count()
    {
        out.push("aos_momentum.rs names m6 past its declaration".to_owned());
    }
    out
}

/// The length of the item or statement `s` begins: to its first `;` outside brackets, the close of
/// its first block when `blocks` (an item's body ends it), or a closer of the block around it.
/// Strings, character literals and line comments are skipped.
fn item_end(s: &str, blocks: bool) -> usize {
    let b = s.as_bytes();
    let mut depth = 0_i32;
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'"' => {
                i += 1;
                while i < b.len() && b[i] != b'"' {
                    i += if b[i] == b'\\' { 2 } else { 1 };
                }
            }
            b'\'' if b.get(i + 1) == Some(&b'\\') => {
                i += 3;
                while i < b.len() && b[i] != b'\'' {
                    i += 1;
                }
            }
            b'\'' if b.get(i + 2) == Some(&b'\'') => i += 2,
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => {
                depth -= 1;
                if depth < 0 || (blocks && depth == 0 && b[i] == b'}') {
                    return i + 1;
                }
            }
            b';' if depth == 0 => return i + 1,
            _ => {}
        }
        i += 1;
    }
    b.len()
}

/// What a test gate covers in `s`, the text after it: an item (`fn`, `mod`, `impl`, `trait`,
/// `struct`, `enum`, `union`, `extern` or a macro) to its body's end; a statement, field or
/// expression past its own blocks (`if … {} else {}`, `S { … }.f()`) to its `;` or the block
/// around it.
fn gated_end(s: &str) -> usize {
    let mut rest = s;
    loop {
        rest = rest.trim_start();
        if let Some(r) = rest.strip_prefix("pub(") {
            rest = r.split_once(')').map_or("", |(_, r)| r);
        } else if rest.starts_with("#[") {
            rest = &rest[bracketed(&rest[1..]).map_or(rest.len(), |n| n + 1)..];
        } else if rest.starts_with("//") {
            rest = rest.split_once('\n').map_or("", |(_, r)| r);
        } else {
            break;
        }
    }
    let stop = rest
        .find(|c: char| !(c.is_alphanumeric() || c == '_' || c.is_whitespace()))
        .unwrap_or(rest.len());
    let item = rest[stop..].starts_with('!')
        || rest[..stop].split_whitespace().any(|w| {
            [
                "fn", "mod", "impl", "trait", "struct", "enum", "union", "extern",
            ]
            .contains(&w)
        });
    item_end(s, item)
}

/// The length of the bracketed text `s` begins (`[…]`), to its closer.
fn bracketed(s: &str) -> Option<usize> {
    let mut depth = 0_i32;
    for (i, c) in s.char_indices() {
        match c {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i + 1);
                }
            }
            _ => {}
        }
    }
    None
}

/// The length of the attribute `s` begins (`[…]`, after its `#`) when it is a test gate:
/// `#[test]`, or a `cfg` whose predicate requires `test`.
fn test_gate(s: &str) -> Option<usize> {
    let len = bracketed(s)?;
    let attr: String = s[1..len - 1].split_whitespace().collect();
    let gate = attr == "test"
        || attr
            .strip_prefix("cfg(")
            .and_then(|p| p.strip_suffix(')'))
            .is_some_and(requires_test);
    gate.then_some(len)
}

/// Whether the `cfg` predicate `p` (whitespace removed) requires `test`: `test` itself, or an
/// `all(…)` one of whose arguments does. (`any(test, …)` and `not(test)` do not: m4's mints
/// are `any(test, debug_assertions, …)` items.)
fn requires_test(p: &str) -> bool {
    p == "test"
        || p.strip_prefix("all(")
            .and_then(|r| r.strip_suffix(')'))
            .is_some_and(|args| {
                let mut depth = 0_i32;
                let mut start = 0;
                let mut parts = Vec::new();
                for (i, c) in args.char_indices() {
                    match c {
                        '(' => depth += 1,
                        ')' => depth -= 1,
                        ',' if depth == 0 => {
                            parts.push(&args[start..i]);
                            start = i + 1;
                        }
                        _ => {}
                    }
                }
                parts.push(&args[start..]);
                parts.into_iter().any(requires_test)
            })
}

/// The test code of the file at `path`: all of it in a test file (`*_tests.rs`, `tests.rs`,
/// under a `tests/` directory, or under `#![cfg(test)]`), and elsewhere what each test gate
/// covers and each doctest's lines.
fn test_parts<'s>(path: &str, src: &'s str) -> Vec<&'s str> {
    let name = path.rsplit('/').next().unwrap_or(path);
    if name.ends_with("_tests.rs")
        || name == "tests.rs"
        || path.contains("/tests/")
        || src
            .match_indices("#![")
            .any(|(at, _)| test_gate(&src[at + 2..]).is_some())
    {
        return vec![src];
    }
    let mut out = Vec::new();
    let mut fenced = false;
    for line in src.lines() {
        let doc = line.trim_start();
        let Some(text) = doc.strip_prefix("///").or_else(|| doc.strip_prefix("//!")) else {
            fenced = false;
            continue;
        };
        if text.trim_start().starts_with("```") {
            fenced = !fenced;
        } else if fenced {
            out.push(line);
        }
    }
    for (at, _) in src.match_indices("#[") {
        let attr = &src[at + 1..];
        if let Some(len) = test_gate(attr) {
            let item = &attr[len..];
            out.push(&item[..gated_end(item)]);
        }
    }
    out
}

/// Whether `text` holds the flag `flag` itself, not a longer one (`--pairs` is another).
fn has_flag(text: &str, flag: &str) -> bool {
    text.match_indices(flag).any(|(i, _)| {
        !text[i + flag.len()..].starts_with(|c: char| c.is_alphanumeric() || c == '_' || c == '-')
    })
}

/// `text`'s refusals under the confinement of the real sets' capability, the mints and the real
/// pairs' flag (design §13.3), for the workspace file at `path`. The flag is refused in every
/// file but the non-test code of the lab's learner and runner, test sources included however
/// they are named or gated.
fn scan_confinement(path: &str, text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let tests = test_parts(path, text);
    let in_tests = |word: &str| tests.iter().any(|t| has_word(t, word));
    let rules: [(&str, &[&str], &[&str]); 3] = [
        // Named in m4.rs's, m6.rs's and aos_learn.rs's non-test items alone.
        (concat!("Real", "Sets"), &[M4_RS, M6_RS, LEARN_RS], &[]),
        // Declared in m4.rs and called in m6.rs's non-test items alone.
        (concat!("mint_", "real"), &[M4_RS, M6_RS], &[]),
        // Those, and the tests of the mints in m4_tests.rs.
        (
            concat!("mint_", "practice"),
            &[M4_RS, M6_RS],
            &[M4_TESTS_RS],
        ),
    ];
    for (word, items, files) in rules {
        let named = if files.contains(&path) {
            false
        } else if items.contains(&path) {
            in_tests(word)
        } else {
            has_word(text, word)
        };
        if named {
            out.push(format!("names {word}"));
        }
    }
    let flag = concat!("--", "pair");
    let entry = [LEARN_RS, LEARN_BENCH_RS].contains(&path);
    if tests.iter().any(|t| has_flag(t, flag)) || (!entry && has_flag(text, flag)) {
        out.push(format!("{flag} outside the lab's learner and runner"));
    }
    out
}

#[test]
fn only_m6s_consumers_name_it() {
    let sources = workspace_sources();
    assert!(
        sources.len() > 1_000,
        "the workspace was read: {}",
        sources.len()
    );
    let paths: Vec<String> = sources.iter().map(|(p, _)| rel(p)).collect();
    for want in [M4_RS, M4_TESTS_RS, M6_RS] {
        assert!(
            paths.iter().any(|p| p == want),
            "{want} is read by its path"
        );
    }
    let mut refused = Vec::new();
    for (path, (_, text)) in paths.iter().zip(&sources) {
        for r in scan_confinement(path, text) {
            refused.push(format!("{path}: {r}"));
        }
        if !M6_NAMERS.contains(&path.as_str()) {
            for r in scan_workspace_file(path, text) {
                refused.push(format!("{path}: {r}"));
            }
        }
    }
    assert_eq!(refused, Vec::<String>::new());
    // m4.rs: the declaration, its impl, `new`'s return and body, and the three real-set mints;
    // m6.rs: the import, the ledger's field, the real-set open and the private load it calls.
    let real = concat!("Real", "Sets");
    for (want, n) in [(M4_RS, 7), (M6_RS, 4)] {
        let (_, text) = &sources[paths.iter().position(|p| p == want).expect("read")];
        assert_eq!(
            words(text).filter(|w| *w == real).count(),
            n,
            "{want} names {real} only where the design lists"
        );
    }
}

#[test]
fn the_workspace_scan_refuses_its_planted_fixtures() {
    let lab = "crates/aterm-lab/src/aos_search.rs";
    let sibling = "crates/aterm-effects/src/aos_momentum/search.rs";
    let refused = |path: &str, src: &str| !scan_workspace_file(path, src).is_empty();
    assert!(!refused(
        lab,
        concat!("use aterm_effects::aos_momentum::", "m6_policy::JUDGED;")
    ));
    assert!(!refused(
        lab,
        "use aterm_effects::aos_momentum::{m4, policy};"
    ));
    assert!(!refused(
        sibling,
        concat!("use super::", "m6_policy::JUDGED;")
    ));
    assert!(!refused(
        lab,
        concat!("// because aos_momentum ", "as a whole;")
    ));
    for (path, planted) in [
        (
            lab,
            concat!("use aterm_effects::aos_momentum::", "m6::Store;"),
        ),
        (
            lab,
            concat!("use aterm_effects::aos_momentum::{m4, ", "m6};"),
        ),
        (
            lab,
            concat!("let s = aterm_effects::aos_momentum::", "m6::Store::load;"),
        ),
        (
            lab,
            concat!(
                "let s = aterm_effects::aos_momentum:: ",
                "m6 ::Store::load;"
            ),
        ),
        (lab, concat!("use aterm_effects::aos_momentum::", "m4::*;")),
        (lab, concat!("use aterm_effects::aos_momentum ", "as am;")),
        (
            lab,
            concat!("use aterm_effects::aos_momentum::{self ", "as am};"),
        ),
        (
            lab,
            concat!("use aterm_effects::{aos_momentum::", "m6, x};"),
        ),
        (sibling, concat!("use super::", "m6::Store;")),
        (sibling, concat!("use super::{m4, ", "m6};")),
        (sibling, concat!("let s = super::", "m6::Store::load;")),
        (sibling, concat!("use self::super::", "m6::Store;")),
        (
            MOMENTUM_RS,
            concat!("fn f() { let s = self::", "m6::Store::load; }"),
        ),
        (MOMENTUM_RS, concat!("pub use ", "m6::{Ranker, Store};")),
        (MOMENTUM_RS, concat!("pub use ", "m6 as learner;")),
        (sibling, concat!("use super::*;\nuse ", "m6::Store;")),
        (sibling, concat!("fn f() { return super::", "m6::f(); }")),
        (sibling, concat!("reach!(super::", "m6, Store);")),
        (sibling, concat!("pub(in super::", "m6) fn f() {}")),
        (sibling, concat!("use super::", "m6 as k;")),
        (lab, concat!("fn f() { return aos_momentum::", "m6::f(); }")),
        (
            sibling,
            concat!("use super::*;\nfn f() { let _ = ", "m6::Store::load; }"),
        ),
    ] {
        assert!(refused(path, planted), "{path}: {planted}");
    }
    assert!(!refused(
        MOMENTUM_RS,
        concat!(
            "#[cfg(any(test, feature = \"aos-learn\"))]\npub mod ",
            "m6;\npub mod m6_policy;"
        )
    ));

    let scan = |path: &str, src: &str| scan_confinement(path, src);
    let test_src = "crates/aterm-effects/src/aos_momentum/m6_tests.rs";
    let real = concat!("Real", "Sets");
    for (path, planted) in [
        (test_src, format!("fn f() {{ let _ = {real}::new(); }}")),
        (M4_TESTS_RS, format!("fn f(_: {real}) {{}}")),
        (
            M4_TESTS_RS,
            concat!("let t = LookToken::mint_", "real(&s, 1, &l);").to_owned(),
        ),
        (
            M4_TESTS_RS,
            concat!("let m = LookToken::mint_", "real;").to_owned(),
        ),
        (
            test_src,
            concat!("let t = LookToken::mint_", "practice(s, &l);").to_owned(),
        ),
        (
            test_src,
            concat!("let args = [\"session\", \"--", "pair\", \"1\"];").to_owned(),
        ),
        (lab, format!("fn f(_: {real}) {{}}")),
        (
            M6_RS,
            format!("#[cfg(test)]\nfn f() {{ let _ = {real}::new(); }}"),
        ),
        (
            M6_RS,
            format!("/// ```\n/// let s = {real}::new();\n/// ```\nfn f() {{}}"),
        ),
        (
            LEARN_RS,
            concat!(
                "#[cfg(test)]\nmod tests {\n    const A: &str = \"--",
                "pair\";\n}\n"
            )
            .to_owned(),
        ),
        (
            "crates/aterm-lab/tests/learn.rs",
            concat!("const A: &str = \"--", "pair\";").to_owned(),
        ),
        (
            M6_RS,
            format!(
                "#[cfg(all(test, target_os = \"macos\"))]\nmod t {{\n    fn f() {{ let _ = {real}::new(); }}\n}}"
            ),
        ),
        (
            M6_RS,
            format!("#[test]\nfn f() {{ let _ = {real}::new(); }}"),
        ),
        (
            M6_RS,
            format!("#![cfg(test)]\nfn f() {{ let _ = {real}::new(); }}"),
        ),
        (
            M6_RS,
            format!(
                "fn f() {{\n    #[cfg(test)]\n    let c = if x {{ None }} else {{ Some({real}::new()) }};\n}}"
            ),
        ),
        (
            M6_RS,
            format!(
                "fn f() {{\n    #[cfg(test)]\n    let s = Probe {{ n: 0 }}.with({real}::new());\n}}"
            ),
        ),
        (
            M4_RS,
            concat!(
                "#[cfg(all(unix, test))]\nfn f() { let _ = LookToken::mint_",
                "real(&s, 1, &l); }"
            )
            .to_owned(),
        ),
        (
            LEARN_RS,
            concat!(
                "#[cfg(all(test, target_os = \"macos\"))]\nmod t {\n    const A: &str = \"--",
                "pair\";\n}\n"
            )
            .to_owned(),
        ),
        (
            "crates/aterm-effects/src/aos_momentum/test_fixtures.rs",
            concat!(
                "pub(super) const A: [&str; 2] = [\"session\", \"--",
                "pair\"];"
            )
            .to_owned(),
        ),
    ] {
        assert!(!scan(path, &planted).is_empty(), "{path}: {planted}");
    }
    for (path, allowed) in [
        (M4_RS, format!("pub struct {real}(NonZeroU64);")),
        (
            M6_RS,
            format!("pub fn real(log: L, i: u32, sets: {real}) {{}}"),
        ),
        (
            M6_RS,
            concat!("let t = LookToken::mint_", "real(real, i, &list)?;").to_owned(),
        ),
        (
            M4_TESTS_RS,
            concat!("let t = LookToken::mint_", "practice(s, &l);").to_owned(),
        ),
        (
            LEARN_RS,
            concat!("match a { \"--", "pair\" => {} _ => {} }").to_owned(),
        ),
        (
            LEARN_BENCH_RS,
            concat!("let args = [\"session\", \"--", "pair\", &i];").to_owned(),
        ),
        (
            M4_RS,
            format!(
                "#[cfg(any(test, debug_assertions, feature = \"aos-learn\"))]\npub fn mint(_: &{real}) {{}}"
            ),
        ),
        (
            M6_RS,
            format!(
                "fn f() {{\n    #[cfg(test)]\n    let v = match x {{ _ => 1 }};\n}}\nfn g(_: {real}) {{}}"
            ),
        ),
        (
            M6_RS,
            format!("#[cfg(test)]\npub(super) fn f() {{}}\nfn g(_: {real}) {{}}"),
        ),
        (
            test_src,
            concat!("let a = [\"--", "pairs\", \"2\"];").to_owned(),
        ),
    ] {
        assert_eq!(
            scan(path, &allowed),
            Vec::<String>::new(),
            "{path}: {allowed}"
        );
    }
}

#[test]
fn the_pools_judged_are_k_per_arm() {
    assert_eq!(JUDGED, 4);
    let s = all_admitted();
    let figures = s.a.figures.first().expect("look 1's figures");
    assert_eq!(figures.pools().len(), 12);
    let _: fn(&PoolId) -> u32 = |p| p.pool;
}
