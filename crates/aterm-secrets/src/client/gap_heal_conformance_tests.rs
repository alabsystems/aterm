// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `GapHeal` conformance (`docs/DESIGN-alab-one-mesh-2026-11-05.md` §9, step A3): the gap
//! closer ([`log_entries::close_wrap_gaps`]) and the tender ([`tend_epochs_at`]) on lab Macs, on
//! an injected clock, refine `aterm_models_swarm::gap_heal_model`. A and B are real vaults whose
//! passes run the code; C's batch, the records moving between the Macs, A's sleep and a person's
//! deny are the environment the model names, played within its four timing assumptions. A
//! person's acts on a Mac (a removal, a LAN admission) are the code's own, run once per input
//! they can meet: their epochs' keys are fresh on each run, so each is run at fixture time until
//! its gaps rank A first and B second, as the model fixes, and a replay takes the same records
//! wherever the Mac's records are the ones that act ran on.

use super::*;
use crate::age::Recipient;
use crate::client::log_entries::{GAP_STEP_MS, REMOVER_GRACE_MS, SETTLE_GRACE_MS};
use crate::client::log_entries_tests::newcomer;
use crate::client::readmit::{Book, Proven, Readmit};
use crate::membership_log::admission::OLD_GAP_RULE;
use crate::membership_log::rekey::UNSETTLED_OK;
use crate::membership_log::{Body, EpochAudience, MemberId, Record, RecordId, RekeyDue, Turn};
use aterm_spec::interp::State;

/// The clock a `Tick` passes: the settle grace, which is the remover's grace too and past every
/// rank of the four Macs, so every deadline a `Tick` stands for has run.
const TICK: u64 = SETTLE_GRACE_MS;
/// How far past its step's start A's passes run after a wake that slept across a `Tick`: a first
/// sight at a wake counts from the next step's start (timing assumption 2).
const LATE: u64 = 1;
/// How many fresh epochs a shape tries for the rank order the model fixes; each try finds it
/// with odds of one in six or better.
const SEARCH: usize = 96;

const _: () = assert!(REMOVER_GRACE_MS == TICK && 2 * GAP_STEP_MS < TICK && LATE < GAP_STEP_MS);

/// A seat of the lab: its member id and age recipient.
#[derive(Clone, Copy)]
struct Seat {
    actor: MemberId,
    recipient: Recipient,
}

fn seat(paths: &Paths) -> Seat {
    Seat {
        actor: member_key(paths).1,
        recipient: read_identity(paths).unwrap().unwrap().recipient(),
    }
}

/// One shape's lab: A's and B's homes (the vaults whose passes run), the seats, what every home
/// holds at the start, and each audience's newest epoch then.
struct Lab {
    macs: TwoMacs,
    a: &'static str,
    b: &'static str,
    seats: [Seat; 3],
    d: Option<Seat>,
    base: BTreeSet<RecordId>,
    e1: BTreeMap<EpochAudience, RecordId>,
}

impl Lab {
    /// `macs` with `others` let in by `m7`, every home holding every record; `a`, `b` and `c` the
    /// homes the roles go to, and `d` a seat's home or none.
    fn new(
        macs: TwoMacs,
        homes: &BTreeMap<&'static str, Paths>,
        roles: [&'static str; 3],
        d: Option<Seat>,
    ) -> Self {
        let a = &homes[roles[0]];
        let base = held(a);
        let e1 = [EpochAudience::Everyone, EpochAudience::Commons]
            .into_iter()
            .map(|audience| {
                let newest = log_of(a).newest_epochs(&audience);
                assert_eq!(
                    newest.len(),
                    1,
                    "one newest {audience:?} epoch at the start"
                );
                (audience, newest[0])
            })
            .collect();
        Self {
            seats: roles.map(|home| seat(&homes[home])),
            a: roles[0],
            b: roles[1],
            macs,
            d,
            base,
            e1,
        }
    }

    fn home(&self, name: &str) -> Paths {
        Paths::for_home(&self.macs.root.path().join(name))
    }
}

/// `m22` and `m7`, and `m7` lets in each of `names`; every home then holds every record.
fn lab_homes(names: &[&'static str]) -> (TwoMacs, BTreeMap<&'static str, Paths>) {
    let macs = two_macs(&[(Audience::Everyone, "shared", b"v1".as_slice())]);
    let mut homes = BTreeMap::from([("m22", macs.m22.clone()), ("m7", macs.m7.clone())]);
    for name in names {
        let (paths, _, _) = let_in_by(&macs, &macs.m7, name, name);
        homes.insert(*name, paths);
    }
    let wire = load_records(&macs.m7).unwrap().encode();
    for paths in homes.values() {
        save_records(paths, &wire).unwrap();
    }
    (macs, homes)
}

fn held(paths: &Paths) -> BTreeSet<RecordId> {
    log_of(paths).records().map(|record| *record.id()).collect()
}

/// The records `paths` holds that `before` does not, in its log's order.
fn since(paths: &Paths, before: &BTreeSet<RecordId>) -> Vec<Record> {
    log_of(paths)
        .records()
        .filter(|record| !before.contains(record.id()))
        .cloned()
        .collect()
}

fn ids_of(records: &[Record]) -> BTreeSet<RecordId> {
    records.iter().map(|record| *record.id()).collect()
}

fn give(paths: &Paths, records: &[Record]) {
    if !records.is_empty() {
        save_records(paths, &encode_records(records)).unwrap();
    }
}

fn wrap_of(record: &Record) -> Option<(RecordId, Recipient)> {
    match record.body() {
        Body::Wrap {
            epoch, recipient, ..
        } => Some((*epoch, *recipient)),
        _ => None,
    }
}

/// Whether `ancestor` lies in the causal past of `of` in `log`.
fn in_past(log: &Log, ancestor: &RecordId, of: &RecordId) -> bool {
    let mut stack = vec![*of];
    let mut seen = BTreeSet::new();
    while let Some(id) = stack.pop() {
        if !seen.insert(id) {
            continue;
        }
        if let Some(record) = log.get(&id) {
            for parent in record.parents() {
                if parent == ancestor {
                    return true;
                }
                stack.push(*parent);
            }
        }
    }
    false
}

/// `records` with every record of `from` in their causal past that `have` lacks, in `from`'s
/// order: what a pull of `records` brings a Mac, which keeps a record whose parent it lacks
/// waiting rather than applied.
fn with_past(from: &Log, records: &[Record], have: &BTreeSet<RecordId>) -> Vec<Record> {
    let mut want: BTreeSet<RecordId> = BTreeSet::new();
    let mut stack: Vec<RecordId> = records.iter().map(|r| *r.id()).collect();
    while let Some(id) = stack.pop() {
        if have.contains(&id) || !want.insert(id) {
            continue;
        }
        if let Some(record) = from.get(&id) {
            stack.extend(record.parents().iter().copied());
        }
    }
    from.records()
        .filter(|r| want.contains(r.id()))
        .cloned()
        .collect()
}

/// The part of `batch` signed before its first `Wrap` to `to`: what a Mac holds that has the
/// batch only as far as the minter's `Epoch` and the `Wrap`s ahead of its own.
fn before_wrap_to<'a>(batch: &'a [Record], to: &Recipient) -> &'a [Record] {
    let cut = batch
        .iter()
        .position(|record| wrap_of(record).is_some_and(|(_, recipient)| recipient == *to))
        .expect("the batch wraps it");
    &batch[..cut]
}

/// C's mint by the old intersection carry: an `[everyone]` epoch wrapped to `to` in ascending
/// recipient order, and nothing after its last `Wrap`, so it settles only by the grace.
fn old_batch(c: &Paths, to: &BTreeSet<Recipient>) -> Vec<Record> {
    let identity = read_identity(c).unwrap().unwrap();
    let (bootstrap, actor, incarnation) = member_key(c);
    let mut log = log_of(c);
    let opened = log.open_audience_entries(&identity).unwrap().unwrap();
    let generation = opened.max_generation[&EpochAudience::Everyone] + 1;
    bootstrap
        .device()
        .with_seed(|seed| -> crate::Result<Vec<Record>> {
            let (draft, key) =
                log.draft_epoch(&actor, 1000, EpochAudience::Everyone, generation)?;
            let epoch = draft.sign(seed, incarnation)?;
            let id = *epoch.id();
            log.insert(epoch.clone())?;
            let mut records = vec![epoch];
            for recipient in to {
                let wrap = log
                    .draft_wrap(&actor, 1000, id, &key, *recipient)?
                    .sign(seed, incarnation)?;
                log.insert(wrap.clone())?;
                records.push(wrap);
            }
            Ok(records)
        })
        .unwrap()
}

/// Whether, on `paths`' log with `added` in it, every gap of `seat` (one per audience
/// `audiences` counts) ranks `order` first, second and so on.
fn ranks_in_order(
    paths: &Paths,
    added: &[Record],
    seat: &MemberId,
    order: &[MemberId],
    audiences: usize,
) -> bool {
    let identity = read_identity(paths).unwrap().unwrap();
    let mut log = log_of(paths);
    for record in added {
        log.insert(record.clone()).unwrap();
    }
    let opened = log.open_audience_entries(&identity).unwrap().unwrap();
    let gaps: Vec<_> = log
        .wrap_gaps(
            &opened,
            &crate::membership_log::admission_audiences(&opened),
        )
        .into_iter()
        .filter(|gap| gap.member == *seat)
        .collect();
    gaps.len() == audiences
        && gaps.iter().all(|gap| {
            order
                .iter()
                .enumerate()
                .all(|(rank, actor)| log.gap_rank(&opened, gap, actor) == Some(rank))
        })
}

/// What a removal or an admission run once at fixture time signed, and the records the Mac
/// held when it ran.
struct Act {
    input: BTreeSet<RecordId>,
    signed: Vec<Record>,
}

/// `Churn`'s acts, each the code run on a copy of the home: B's admission of D on the start
/// (`adm0`) and on A's removal (`adm1`); A's removal of C on the start (`rem0`, its gaps of D
/// ranked A then B), on `adm0` (`rem1`), and on `adm0` and D's deny (`rem1d`); and D's deny on
/// `adm0` (`deny0`).
struct Churn {
    adm0: Act,
    adm1: Act,
    rem0: Act,
    rem1: Act,
    rem1d: Act,
    deny0: Act,
    /// The roster that denies D, as the `[everyone]` swarm-roster entry.
    roster: String,
}

/// The three shapes, each built once, and the injected clock's start.
struct GapShapes {
    /// `OldMint`: D let in, C mints wrapping A, B and itself; A holds the prefix through its own
    /// `Wrap` first.
    one: Lab,
    one_batch: Vec<Record>,
    one_prefix: usize,
    /// `OldMintOmitsB`: C mints wrapping A and itself; A ranks first among every member.
    two: Lab,
    two_batch: Vec<Record>,
    /// `Churn`: A removes C while B admits D.
    three: Lab,
    churn: Churn,
    t0: u64,
}

impl GapShapes {
    fn build() -> Self {
        let (one, one_batch, one_prefix) = Self::old_mint();
        let (two, two_batch) = Self::omits_b();
        let (three, churn) = Self::churn();
        let t0 = aterm_time::unix_millis().unwrap();
        Self {
            one,
            one_batch,
            one_prefix,
            two,
            two_batch,
            three,
            churn,
            t0,
        }
    }

    /// `OldMint`'s lab: of m22, m7 and m40, A the lowest recipient (so its `Wrap` leads C's
    /// batch), and D the admitted m41; C's batch ranks A, B, C for D's gap.
    fn old_mint() -> (Lab, Vec<Record>, usize) {
        let (macs, homes) = lab_homes(&["m40", "m41"]);
        let mut three = ["m22", "m7", "m40"];
        three.sort_by_key(|home| seat(&homes[*home]).recipient);
        let d = seat(&homes["m41"]);
        let lab = Lab::new(macs, &homes, three, Some(d));
        let to: BTreeSet<Recipient> = lab.seats.iter().map(|s| s.recipient).collect();
        let order = lab.seats.map(|s| s.actor);
        let c = lab.home(three[2]);
        let a = lab.home(lab.a);
        let batch = (0..SEARCH)
            .map(|_| old_batch(&c, &to))
            .find(|batch| ranks_in_order(&a, batch, &d.actor, &order, 1))
            .expect("an epoch that ranks A, B, C for D's gap");
        let prefix = before_wrap_to(&batch, &lab.seats[0].recipient).len() + 1;
        assert_eq!(prefix, 2, "CONTROL: A's `Wrap` leads C's batch");
        (lab, batch, prefix)
    }

    /// `OldMintOmitsB`'s lab: A is m7, ranked first among all three members for a re-key a
    /// removal made due ([`RekeyDue::turn`]); B is m40, whom m7 lets in after C (m22) last read
    /// the log; C's batch, minted over that view, ranks A before C for B's gap. C signs nothing
    /// after that view: a batch minted from it by a Mac that signed since would fork its chain.
    ///
    /// C leaves B out without knowing B's seat: a seat a newest epoch's minter knew and left out
    /// reads as no member (the `left_out` family, which one-mesh D6 retires) and could sign
    /// nothing, while the model's B signs the `Remove` alone before any heal reaches it.
    fn omits_b() -> (Lab, Vec<Record>) {
        let names = ["m22", "m7", "m40"];
        for _ in 0..SEARCH {
            let macs = two_macs(&[(Audience::Everyone, "shared", b"v1".as_slice())]);
            let root = macs.root.path().to_path_buf();
            copy_private_tree(&root.join("m22"), &root.join("m22-before"));
            let (m40, _, _) = let_in_by(&macs, &macs.m7, "m40", "m40");
            let homes = BTreeMap::from([
                ("m22", macs.m22.clone()),
                ("m7", macs.m7.clone()),
                ("m40", m40),
            ]);
            let wire = load_records(&macs.m7).unwrap().encode();
            for paths in homes.values() {
                save_records(paths, &wire).unwrap();
            }
            let membership = log_of(&homes["m22"]).membership();
            let first = |at: usize| {
                let due = RekeyDue {
                    audience: EpochAudience::Everyone,
                    newest: Vec::new(),
                    rotation: true,
                    coverage: false,
                    reseal: false,
                    removers: BTreeSet::from([seat(&homes["m40"]).actor]),
                    removals: BTreeSet::new(),
                    removed: BTreeSet::new(),
                    carriers: BTreeSet::new(),
                    resealers: BTreeSet::new(),
                    reseal_first: BTreeSet::new(),
                };
                due.turn(&membership, &seat(&homes[names[at]]).actor)
                    == Some(Turn {
                        rank: 0,
                        after_grace: true,
                    })
            };
            if !first(1) {
                macs.server.stop();
                continue;
            }
            let roles = ["m7", "m40", "m22"];
            let lab = Lab::new(macs, &homes, roles, None);
            let to = BTreeSet::from([lab.seats[0].recipient, lab.seats[2].recipient]);
            let order = [lab.seats[0].actor, lab.seats[2].actor];
            let c = lab.home(&format!("{}-before", roles[2]));
            let a = lab.home(lab.a);
            let batch = (0..SEARCH)
                .map(|_| old_batch(&c, &to))
                .find(|batch| ranks_in_order(&a, batch, &lab.seats[1].actor, &order, 1))
                .expect("an epoch that ranks A before C for B's gap");
            return (lab, batch);
        }
        panic!("no lab of three ranks m7 first");
    }

    /// `Churn`'s lab: A m7, B m22, C m40, and D a Mac asking B on the home LAN.
    fn churn() -> (Lab, Churn) {
        let (macs, homes) = lab_homes(&["m40"]);
        let (d_home, request, _) = newcomer(macs.root.path(), &homes["m22"], "d");
        let d = Seat {
            actor: request.binding().actor(),
            recipient: read_identity(&d_home).unwrap().unwrap().recipient(),
        };
        let lab = Lab::new(macs, &homes, ["m7", "m22", "m40"], Some(d));
        let root = lab.macs.root.path().to_path_buf();
        let copy = |from: &str, name: String, add: &[&Act]| -> Paths {
            copy_private_tree(&root.join(from), &root.join(&name));
            let paths = Paths::for_home(&root.join(&name));
            for act in add {
                give(&paths, &act.signed);
            }
            paths
        };
        let now = aterm_time::unix_millis().unwrap();
        let admit = |b: &Paths| -> Act {
            let input = held(b);
            let proven = Proven {
                request: &request,
                node: [0x5d; 32],
                name: "d",
                reader: crate::members::bundle::Reader::BoundedLog(
                    crate::crossing::RecordsBound::uncrossed(),
                ),
                fleet: false,
                lan: true,
                fresh: true,
                book: Book {
                    moved_seats_read: true,
                    ..Book::default()
                },
            };
            let let_in = admit_by_proof(b, &proven, now).expect("decided");
            assert!(matches!(let_in, Readmit::Let { .. }), "{let_in:?}");
            Act {
                signed: since(b, &input),
                input,
            }
        };
        let c = lab.seats[2].recipient;
        let remove = |a: &Paths| -> Act {
            let input = held(a);
            test_remove_unposted(a, &join_code(&c)).expect("A removes C");
            Act {
                signed: since(a, &input),
                input,
            }
        };
        let none: [&Act; 0] = [];
        let adm0 = admit(&copy(lab.b, "adm0".into(), &none));
        let a = lab.home(lab.a);
        let order = [lab.seats[0].actor, lab.seats[1].actor];
        let rem0 = (0..SEARCH)
            .map(|i| remove(&copy(lab.a, format!("rem0-{i}"), &none)))
            .find(|rem| {
                let added: Vec<Record> = adm0.signed.iter().chain(&rem.signed).cloned().collect();
                ranks_in_order(&a, &added, &d.actor, &order, 2)
            })
            .expect("a removal whose epochs rank A, then B, for D's gaps");
        let rem1 = remove(&copy(lab.a, "rem1".into(), &[&adm0]));
        let roster = format!(
            "deny_nodes = [\"{}\"]\n",
            aterm_codec::hex::encode_lower(request.binding().writer())
        );
        let denying = copy(lab.a, "rem1d".into(), &[&adm0]);
        let input = held(&denying);
        test_put_everyone(
            &denying,
            crate::swarm_roster::ROSTER_ENTRY,
            roster.as_bytes(),
        )
        .expect("A puts D's deny");
        let deny0 = Act {
            signed: since(&denying, &input),
            input,
        };
        let rem1d = remove(&denying);
        let adm1 = admit(&copy(lab.b, "adm1".into(), &[&rem0]));
        let churn = Churn {
            adm0,
            adm1,
            rem0,
            rem1,
            rem1d,
            deny0,
            roster,
        };
        (lab, churn)
    }

    fn stop(self) {
        self.one.macs.server.stop();
        self.two.macs.server.stop();
        self.three.macs.server.stop();
    }
}

/// One replay of `GapHeal` on copies of A's and B's homes, and what the harness keeps of the
/// environment: the steps passed (`k`), A's sleep, every record signed (C's batch first), each
/// mint's batch by the epochs it mints, B's admission of D, each Mac's first pass that held each
/// epoch, which passes ran since a Mac's last record arrived, B's rank clock (`bt`), the age of
/// the oldest open gap, D's deny, and the clocks the model keeps of A's grace and B's hold.
#[derive(Clone)]
struct GapRun<'a> {
    shapes: &'a GapShapes,
    dir: PathBuf,
    sc: i64,
    k: u64,
    late: bool,
    asleep: bool,
    slept: bool,
    signed: Vec<Record>,
    batches: BTreeMap<RecordId, Vec<Record>>,
    e2: BTreeMap<EpochAudience, RecordId>,
    admission: BTreeSet<RecordId>,
    a_first: BTreeMap<RecordId, u64>,
    b_first: BTreeMap<RecordId, u64>,
    pa: bool,
    pb: bool,
    bt: i64,
    age: i64,
    deny: Option<(usize, Record)>,
    gr_from: Option<u64>,
    rb_from: Option<u64>,
    lost: bool,
}

/// Which Mac signed a record: A, B or C (D signs nothing).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Who {
    A,
    B,
    C,
}

impl<'a> GapRun<'a> {
    /// The shape `selector` picks, A's and B's homes copied into `dir`.
    fn start(shapes: &'a GapShapes, selector: &str, dir: &Path) -> Self {
        let sc = match selector {
            "OldMint" => 1,
            "OldMintOmitsB" => 2,
            "Churn" => 3,
            other => panic!("not a GapHeal shape: {other}"),
        };
        let mut run = Self {
            shapes,
            dir: dir.to_path_buf(),
            sc,
            k: 0,
            late: false,
            asleep: false,
            slept: false,
            signed: Vec::new(),
            batches: BTreeMap::new(),
            e2: BTreeMap::new(),
            admission: BTreeSet::new(),
            a_first: BTreeMap::new(),
            b_first: BTreeMap::new(),
            pa: false,
            pb: false,
            bt: 0,
            age: 0,
            deny: None,
            gr_from: None,
            rb_from: None,
            lost: false,
        };
        let lab = run.lab();
        for home in [lab.a, lab.b] {
            copy_private_tree(&lab.macs.root.path().join(home), &dir.join(home));
        }
        if sc != 3 {
            let batch = if sc == 1 {
                &shapes.one_batch
            } else {
                &shapes.two_batch
            };
            run.minted(batch);
        }
        run
    }

    /// This run as it stands, its homes copied into `dir`.
    fn fork(&self, dir: &Path) -> Self {
        let lab = self.lab();
        for home in [lab.a, lab.b] {
            copy_private_tree(&self.dir.join(home), &dir.join(home));
        }
        Self {
            dir: dir.to_path_buf(),
            ..self.clone()
        }
    }

    fn lab(&self) -> &'a Lab {
        match self.sc {
            1 => &self.shapes.one,
            2 => &self.shapes.two,
            _ => &self.shapes.three,
        }
    }

    fn a(&self) -> Paths {
        Paths::for_home(&self.dir.join(self.lab().a))
    }

    fn b(&self) -> Paths {
        Paths::for_home(&self.dir.join(self.lab().b))
    }

    /// The start of this step.
    fn t(&self) -> u64 {
        self.shapes.t0 + self.k * TICK
    }

    /// The clock of A's passes this step.
    fn a_now(&self) -> u64 {
        self.t() + if self.late { LATE } else { 0 }
    }

    /// The audiences the shape mints in: `[everyone]`, and Commons too where A's removal
    /// re-keys both.
    fn audiences(&self) -> Vec<EpochAudience> {
        if self.sc == 3 {
            vec![EpochAudience::Everyone, EpochAudience::Commons]
        } else {
            vec![EpochAudience::Everyone]
        }
    }

    fn who(&self, actor: &MemberId) -> Who {
        let seats = &self.lab().seats;
        if *actor == seats[0].actor {
            Who::A
        } else if *actor == seats[1].actor {
            Who::B
        } else {
            assert_eq!(*actor, seats[2].actor, "only A, B and C sign");
            Who::C
        }
    }

    /// Note a mint: `signed` joins the run's records, and is the batch of each epoch it mints;
    /// an epoch over the start's newest is the shape's epoch 2 the first time.
    fn minted(&mut self, signed: &[Record]) {
        for record in signed {
            if let Body::Epoch { audience, .. } = record.body() {
                self.batches.insert(*record.id(), signed.to_vec());
                self.e2.entry(audience.clone()).or_insert(*record.id());
            }
        }
        self.signed.extend_from_slice(signed);
    }

    /// Every record any Mac holds or signed, as one log.
    fn union(&self) -> Log {
        let mut log = log_of(&self.a());
        for record in log_of(&self.b()).records().chain(&self.signed) {
            if log.get(record.id()).is_none() {
                log.insert(record.clone()).unwrap();
            }
        }
        log
    }

    fn newest(&self, log: &Log, audience: &EpochAudience) -> RecordId {
        let newest = log.newest_epochs(audience);
        assert_eq!(
            newest.len(),
            1,
            "two {audience:?} heads: concurrent mints are EpochConvergence's, outside GapHeal"
        );
        newest[0]
    }

    /// A first-sight clock of each epoch `paths` holds at a pass at `now`.
    fn see(first: &mut BTreeMap<RecordId, u64>, paths: &Paths, now: u64) {
        for record in log_of(paths).records() {
            if matches!(record.body(), Body::Epoch { .. }) {
                first.entry(*record.id()).or_insert(now);
            }
        }
    }

    /// The `Remove` of C, and the `Admit` of D, where `log` holds one.
    fn remove_of_c(&self, log: &Log) -> Option<RecordId> {
        let c = self.lab().seats[2].actor;
        log.records()
            .find(|r| matches!(r.body(), Body::Remove { member, .. } if *member == c))
            .map(|r| *r.id())
    }

    fn admit_of_d(&self, log: &Log) -> Option<RecordId> {
        let d = self.lab().d?.actor;
        log.records()
            .find(|r| matches!(r.body(), Body::Admit { entrant, .. } if entrant.member() == d))
            .map(|r| *r.id())
    }

    /// Hand D's deny to B once B holds every record the `Put` follows (assumption 4: at once,
    /// where B can take it; a B that cannot holds no batch it could heal D from).
    /// Gives D's deny to each Mac that holds its parents and not it yet: a deny reaches both
    /// Macs at once (the model's fourth timing assumption), and a `Put` whose parents a Mac
    /// lacks would only wait there.
    fn push_deny(&self) {
        let Some((_, put)) = &self.deny else {
            return;
        };
        for paths in [self.a(), self.b()] {
            let have = held(&paths);
            if !have.contains(put.id()) && put.parents().iter().all(|parent| have.contains(parent))
            {
                give(&paths, std::slice::from_ref(put));
            }
        }
    }

    /// The act of `variants` whose input is what `paths` holds.
    fn variant<'b>(paths: &Paths, variants: &[&'b Act], what: &str) -> &'b Act {
        let have = held(paths);
        variants
            .iter()
            .copied()
            .find(|act| act.input == have)
            .unwrap_or_else(|| panic!("{what} on records no fixture ran it on"))
    }

    fn apply(&mut self, action: &str) {
        let shapes = self.shapes;
        let churn = &shapes.churn;
        match action {
            "HealA" => {
                let a = self.a();
                let now = self.a_now();
                Self::see(&mut self.a_first, &a, now);
                let signed = log_entries::close_wrap_gaps(&a, now).unwrap();
                look_at_dues(&a, now).unwrap();
                self.signed.extend(signed);
                self.pa = true;
            }
            "HealB" => {
                let b = self.b();
                let now = self.t();
                Self::see(&mut self.b_first, &b, now);
                let signed = log_entries::close_wrap_gaps(&b, now).unwrap();
                look_at_dues(&b, now).unwrap();
                let union = self.union();
                let newest: BTreeSet<RecordId> = self
                    .audiences()
                    .iter()
                    .map(|audience| self.newest(&union, audience))
                    .collect();
                let sees = signed
                    .iter()
                    .filter_map(wrap_of)
                    .any(|(epoch, _)| newest.contains(&epoch))
                    || log_entries::gap_clocks(&b)
                        .iter()
                        .any(|((epoch, _), (_, start))| newest.contains(epoch) && *start != 0);
                self.bt = match (sees, self.bt) {
                    (false, _) => 0,
                    (true, 0) => 1,
                    (true, 2) => 3,
                    (true, bt) => bt,
                };
                self.signed.extend(signed);
                self.pb = true;
            }
            "Tick" => {
                let s = gap_heal_transition(self);
                let gap = (s["ad"] == 1
                    && s["dy"] == 0
                    && s["wd"] == 0
                    && s["had"] == 0
                    && s["hbd"] == 0
                    && ((s["sa"] == 0 && s["wa"] == 1 && s["va"] == 3 && s["dk"] == 1)
                        || (((s["vb"] > 1 && s["wb"] == 1) || (s["hab"] == 1 && s["qb"] == 0))
                            && s["vb"] == 3)))
                    || (s["wb"] == 0
                        && s["hab"] == 0
                        && s["sa"] == 0
                        && s["wa"] == 1
                        && s["va"] == 3);
                self.age = match (gap, self.age) {
                    (false, _) => 0,
                    (true, 0) => 1,
                    (true, _) => 2,
                };
                self.k += 1;
                self.late = false;
                self.slept |= self.asleep;
                if self.bt == 1 {
                    self.bt = 2;
                }
                self.pa = false;
                self.pb = false;
            }
            "Sleep" => {
                self.asleep = true;
                self.slept = false;
            }
            "Wake" => {
                let a = self.a();
                give(
                    &a,
                    &log_of(&self.b()).records().cloned().collect::<Vec<_>>(),
                );
                give(&a, &self.signed);
                self.asleep = false;
                self.late = self.slept;
                self.slept = false;
                self.pa = false;
            }
            "DeliverToA" => {
                let a = self.a();
                let batch = if self.sc == 1 {
                    &shapes.one_batch
                } else {
                    &shapes.two_batch
                };
                let part = if self.sc == 1 && !held(&a).contains(batch[0].id()) {
                    &batch[..shapes.one_prefix]
                } else {
                    &batch[..]
                };
                give(&a, part);
                self.pa = false;
            }
            "DeliverToB" => {
                let batch = if self.sc == 1 {
                    &shapes.one_batch
                } else {
                    &shapes.two_batch
                };
                give(&self.b(), batch);
                self.pb = false;
            }
            "ToA" => {
                let a = self.a();
                let removed_before = self.remove_of_c(&log_of(&a)).is_some();
                let ep = gap_heal_transition(self)["ep"];
                give(
                    &a,
                    &log_of(&self.b()).records().cloned().collect::<Vec<_>>(),
                );
                if !removed_before && self.remove_of_c(&log_of(&a)).is_some() && ep == 2 {
                    self.gr_from = Some(self.t());
                }
                self.pa = false;
            }
            "ToB" => {
                let s = gap_heal_transition(self);
                let b = self.b();
                let union = self.union();
                let e = self.newest(&union, &EpochAudience::Everyone);
                let batch = self.batches.get(&e).filter(|_| s["mm"] == 1);
                let have = held(&b);
                match batch {
                    Some(batch) if !batch.iter().all(|r| have.contains(r.id())) => {
                        if have.contains(&e) {
                            give(&b, batch);
                        } else {
                            let prefix = before_wrap_to(batch, &self.lab().seats[1].recipient);
                            give(&b, &with_past(&log_of(&self.a()), prefix, &have));
                        }
                    }
                    _ => give(
                        &b,
                        &log_of(&self.a()).records().cloned().collect::<Vec<_>>(),
                    ),
                }
                self.push_deny();
                if self.sc == 2 && s["cx"] == 1 && s["ep"] == 2 && s["qb"] == 1 {
                    self.rb_from = Some(self.t());
                }
                self.pb = false;
            }
            "Admit" => {
                let b = self.b();
                let act = Self::variant(&b, &[&churn.adm0, &churn.adm1], "B's admission");
                give(&b, &act.signed);
                self.admission = ids_of(&act.signed);
                self.signed.extend_from_slice(&act.signed);
                self.pb = false;
            }
            "Remove" => {
                let a = self.a();
                let act =
                    Self::variant(&a, &[&churn.rem0, &churn.rem1, &churn.rem1d], "A's removal");
                give(&a, &act.signed);
                self.minted(&act.signed);
                self.pa = false;
            }
            "Deny" => {
                // A person denies D on a Mac whose records the other holds, so the deny reaches
                // both at once without carrying a heal the model has not delivered yet (`qa`,
                // `qb`): A when B holds all of A's records, else B when A holds all of B's and B
                // can sign. Otherwise B holds only a prefix of A's batch (`vb` 1), A puts the
                // deny, and it reaches B with the rest of that batch (`ToB`): a B that holds a
                // prefix heals nothing, so it acts on no gap the deny would refuse before then.
                let (a, b) = (self.a(), self.b());
                let (on_a, on_b) = (held(&a), held(&b));
                let by_b = !on_a.is_subset(&on_b)
                    && on_b.is_subset(&on_a)
                    && log_entries::open_local_member_signer(&b, &log_of(&b))
                        .unwrap()
                        .is_some();
                let put = if !by_b && on_a == churn.deny0.input {
                    give(&a, &churn.deny0.signed);
                    churn.deny0.signed.clone()
                } else {
                    let (by, before) = if by_b { (&b, on_b) } else { (&a, on_a) };
                    test_put_everyone(
                        by,
                        crate::swarm_roster::ROSTER_ENTRY,
                        churn.roster.as_bytes(),
                    )
                    .unwrap_or_else(|e| {
                        panic!("D's deny by {} fails: {e:?}", if by_b { "B" } else { "A" })
                    });
                    since(by, &before)
                };
                assert_eq!(put.len(), 1, "a deny is one `Put`");
                self.deny = Some((self.signed.len(), put[0].clone()));
                self.signed.extend(put);
                self.push_deny();
                self.pa = false;
                self.pb = false;
            }
            "RemoveByB" => {
                let b = self.b();
                let union = self.union();
                let e = self.newest(&union, &EpochAudience::Everyone);
                let opens = log_of(&b)
                    .records()
                    .filter_map(wrap_of)
                    .any(|(epoch, to)| epoch == e && to == self.lab().seats[1].recipient);
                let before = held(&b);
                test_remove_unposted(&b, &join_code(&self.lab().seats[2].recipient))
                    .expect("B removes C");
                let signed = since(&b, &before);
                self.lost |= !opens
                    && signed.iter().any(|r| {
                        matches!(
                            r.body(),
                            Body::Epoch {
                                audience: EpochAudience::Everyone,
                                ..
                            }
                        )
                    });
                self.minted(&signed);
                self.pb = false;
            }
            "Rotate" => {
                let signed = tend_epochs_at(&self.a(), self.a_now()).unwrap();
                self.minted(&signed);
                self.pa = false;
            }
            "RotateByB" => {
                let signed = tend_epochs_at(&self.b(), self.t()).unwrap();
                self.minted(&signed);
                self.pb = false;
            }
            other => panic!("not a GapHeal action from a shape: {other}"),
        }
    }

    /// What `log`, a Mac's, holds of the batch that minted `e` (one batch for every audience it
    /// mints), as `va` and `vb` count it: 3 for the start's epoch and the Mac's own; else 0 none,
    /// 1 a prefix, and whole 3 where a record
    /// the minter signed after `e` settles it, else by the settle grace from the Mac's first
    /// sight (`first`, or this pass): 3 run, 2 runs out at the next `Tick`, 5 at the one after.
    /// A that slept across a `Tick` reads 6 for none and 4 for a prefix.
    fn view(
        &self,
        log: &Log,
        e: &RecordId,
        own: bool,
        first: Option<u64>,
        slept: bool,
        now: u64,
    ) -> i64 {
        let Some(batch) = self.batches.get(e) else {
            return 3;
        };
        if own {
            return 3;
        }
        let have = batch
            .iter()
            .filter(|record| log.get(record.id()).is_some())
            .count();
        if have == 0 {
            return if slept { 6 } else { 0 };
        }
        if have < batch.len() {
            return if slept { 4 } else { 1 };
        }
        let at = batch
            .iter()
            .position(|r| r.id() == e)
            .expect("the batch mints it");
        let minter = batch[at].actor();
        if batch[at + 1..]
            .iter()
            .any(|r| r.actor() == minter && wrap_of(r).is_none())
        {
            return 3;
        }
        let settles = first.unwrap_or(now) + TICK;
        if settles <= self.t() {
            3
        } else if settles <= self.t() + TICK {
            2
        } else {
            5
        }
    }

    /// The model's variables of the newest epoch of `audience`.
    fn epoch_vars(&self, union: &Log, a: &Log, b: &Log, audience: &EpochAudience) -> State {
        let lab = self.lab();
        let e = self.newest(union, audience);
        let ep = if e == lab.e1[audience] {
            1
        } else if self.e2.get(audience) == Some(&e) {
            2
        } else {
            3
        };
        let minter = self.who(union.get(&e).expect("held").actor());
        let mm = match (ep, minter) {
            (1, _) => 0,
            (_, Who::A) => 1,
            (_, Who::B) => 2,
            (_, Who::C) => 3,
        };
        let batch: BTreeSet<RecordId> = self
            .batches
            .get(&e)
            .map_or_else(|| lab.base.clone(), |batch| ids_of(batch));
        let seats = [
            Some(lab.seats[0].recipient),
            Some(lab.seats[1].recipient),
            Some(lab.seats[2].recipient),
            lab.d.map(|d| d.recipient),
        ];
        let mut w = [0i64; 4];
        let mut h: BTreeMap<(Who, usize), i64> = BTreeMap::new();
        let mut qa = 0;
        let mut qb = 0;
        for record in union.records() {
            let Some((epoch, to)) = wrap_of(record) else {
                continue;
            };
            if epoch != e {
                continue;
            }
            let Some(x) = seats.iter().position(|s| *s == Some(to)) else {
                continue;
            };
            if batch.contains(record.id()) || (ep == 1 && lab.base.contains(record.id())) {
                w[x] += 1;
                continue;
            }
            if lab.base.contains(record.id()) {
                continue;
            }
            let signer = self.who(record.actor());
            *h.entry((signer, x)).or_default() += 1;
            match signer {
                Who::A if b.get(record.id()).is_none() => qb = 1,
                Who::B if a.get(record.id()).is_none() && !self.admission.contains(record.id()) => {
                    qa = 1;
                }
                _ => {}
            }
        }
        let heal = |s: Who, x: usize| h.get(&(s, x)).copied().unwrap_or(0);
        let past = |id: Option<RecordId>| i64::from(id.is_some_and(|id| in_past(union, &id, &e)));
        let mk = if self.sc == 2 {
            1
        } else {
            past(self.admit_of_d(union))
        };
        let va = self.view(
            a,
            &e,
            minter == Who::A,
            self.a_first.get(&e).copied(),
            self.asleep && self.slept,
            self.a_now(),
        );
        let vb = self.view(
            b,
            &e,
            minter == Who::B,
            self.b_first.get(&e).copied(),
            false,
            self.t(),
        );
        State::from([
            ("ep", ep),
            ("mm", mm),
            ("er", past(self.remove_of_c(union))),
            ("mk", mk),
            ("wa", w[0]),
            ("wb", w[1]),
            ("wc", w[2]),
            ("wd", w[3]),
            ("hab", heal(Who::A, 1)),
            ("hac", heal(Who::A, 2)),
            ("had", heal(Who::A, 3)),
            ("hbc", heal(Who::B, 2)),
            ("hbd", heal(Who::B, 3)),
            ("va", va),
            ("vb", vb),
            ("qa", qa),
            ("qb", qb),
        ])
    }
}

/// `GapHeal`'s variables, read off a replay: the newest epoch's from the records every Mac holds
/// or signed (each audience the shape mints in reads the same), D's and C's standing from the
/// log and A's records, and the harness's clocks.
fn gap_heal_transition(run: &GapRun<'_>) -> State {
    let union = run.union();
    let a = log_of(&run.a());
    let b = log_of(&run.b());
    let per: Vec<State> = run
        .audiences()
        .iter()
        .map(|audience| run.epoch_vars(&union, &a, &b, audience))
        .collect();
    assert!(
        per.windows(2).all(|pair| pair[0] == pair[1]),
        "the audiences A's removal re-keys read apart: {per:?}"
    );
    let mut state = per[0].clone();
    let rule = |from: Option<u64>| match from {
        None => 0,
        Some(at) if at + TICK > run.t() => 1,
        Some(_) => 2,
    };
    let at_e2 = state["ep"] == 2;
    let dw = run.deny.as_ref().is_some_and(|(at, _)| {
        let d = run.lab().d.expect("a deny names D").recipient;
        run.signed[*at..]
            .iter()
            .filter_map(wrap_of)
            .any(|(_, to)| to == d)
    });
    for (name, value) in [
        ("sc", run.sc),
        ("ad", i64::from(run.admit_of_d(&union).is_some())),
        ("dk", i64::from(run.admit_of_d(&a).is_some())),
        ("dy", i64::from(run.deny.is_some())),
        ("cx", i64::from(run.remove_of_c(&union).is_some())),
        ("kr", i64::from(run.remove_of_c(&a).is_some())),
        ("sa", i64::from(run.asleep)),
        ("pa", i64::from(run.pa)),
        ("pb", i64::from(run.pb)),
        ("bt", run.bt),
        ("age", run.age),
        ("dw", i64::from(dw)),
        ("lost", i64::from(run.lost)),
        ("gr", rule(run.gr_from)),
        ("rb", if at_e2 { rule(run.rb_from) } else { 0 }),
    ] {
        state.insert(name, value);
    }
    state
}

/// `path` from `GapHeal`'s first state on the shapes' homes, copied into `dir`, each step's
/// projection kept.
fn gap_replay<'a>(shapes: &'a GapShapes, dir: &Path, path: &[&str]) -> (GapRun<'a>, Vec<State>) {
    let mut run = GapRun::start(shapes, path[0], dir);
    let mut seen = vec![gap_heal_transition(&run)];
    for action in &path[1..] {
        run.apply(action);
        seen.push(gap_heal_transition(&run));
    }
    (run, seen)
}

/// The states `GapHeal` passes through along `path` (each of its actions has one successor).
fn gap_states(model: &aterm_spec::derive::Model, path: &[&str]) -> Vec<State> {
    let mut state = model.init_state();
    path.iter()
        .map(|action| {
            let next = model.successors(action, &state);
            assert_eq!(next.len(), 1, "{action} from {state:?}");
            state = next[0].clone();
            state.clone()
        })
        .collect()
}

/// `OldMint` to the step where both Macs hold C's batch settled and A has healed D: the prefix
/// path every gap-closer control runs.
const SETTLED: [&str; 8] = [
    "OldMint",
    "DeliverToB",
    "DeliverToA",
    "DeliverToA",
    "HealA",
    "HealB",
    "Tick",
    "HealB",
];

/// A3: `GapHeal` holds its laws, each mutant breaks the law it stands for, and the gap closer
/// and the tender refine it: from every state the model reaches, every action it enables, run
/// on a copy of A's and B's homes that realize that state (each shape built once, every state's
/// homes forked from the state the walk first found it from), gives exactly the model's
/// successor. `HealA` and `HealB` are a Mac's vault pass up to its re-keys: the closer
/// ([`log_entries::close_wrap_gaps`]) and the tender's scheduling look ([`look_at_dues`]), at the
/// injected clock; `Rotate` and `RotateByB` are its tender ([`tend_epochs_at`]); `RemoveByB` is
/// [`test_remove_unposted`] on B, and `Remove` and `Admit` replay A's removal and B's LAN
/// admission ([`admit_by_proof`]) run at fixture time on the records the Mac holds.
///
/// NEGATIVE CONTROLS, each the real closer down a path the model takes:
/// - `OLD_GAP_RULE`: B, at its first look past the settle, reads D, whom C knew and left out, as
///   revoked and starts no clock, which is no successor; neither Mac heals D, and two steps on
///   D's gap is two steps old and `GapsClose` breaks.
/// - `UNSETTLED_OK`: A heals every seat its prefix of C's batch lacks, which is no successor;
///   the rest of the batch then wraps B again and `NoStorm` breaks.
/// - `remove::UNRANKED`: B heals D at its first settled look, beside A, which is no successor;
///   once A heals too `NoStorm` breaks.
/// - `log_entries::GAPS_FROM_FIRST_SIGHT`: B's rank counts from its first sight of the unsettled
///   batch, so it is due in the step A's is, which is no successor; `NoStorm` breaks the same.
#[test]
fn the_gap_closer_conforms_to_the_gap_heal_model() {
    use aterm_spec::interp::{violated_invariants, with_buggy};

    let model = aterm_models_swarm::gap_heal_model();
    aterm_spec::interp::prove_and_catch(&model);
    let fired = aterm_spec::interp::fired_actions(&model);
    assert_eq!(fired.len(), model.actions.len(), "fired actions: {fired:?}");
    let faults = aterm_spec::verify::sweep_faults(std::slice::from_ref(&model));
    assert!(faults.is_empty(), "{faults:?}");
    for (buggy, law) in [
        (1, "GapsClose"),
        (2, "RemovalIsFinal"),
        (3, "DeniedNeverWrapped"),
        (4, "OnlyHoldersMintOrHeal"),
        (5, "NoStorm"),
        (6, "NoStorm"),
        (7, "RemovalIsFinal"),
        (8, "NoStorm"),
    ] {
        assert!(
            violated_invariants(&with_buggy(&model, buggy)).contains(law),
            "mutant {buggy} breaks {law}"
        );
    }

    let shapes = GapShapes::build();
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let init = model.init_state();
    let mut order = vec![init.clone()];
    let mut realized: BTreeMap<State, GapRun<'_>> = BTreeMap::new();
    let mut runs = 0usize;
    let mut next = 0;
    while let Some(state) = order.get(next).cloned() {
        next += 1;
        for action in &model.actions {
            if !model.action_enabled(action.name, &state) {
                continue;
            }
            runs += 1;
            let dir = root.path().join(format!("r{runs}"));
            let run = match realized.get(&state) {
                Some(from) => {
                    let mut run = from.fork(&dir);
                    run.apply(action.name);
                    run
                }
                None => {
                    assert_eq!(state, init, "every later state is realized");
                    GapRun::start(&shapes, action.name, &dir)
                }
            };
            let observed = gap_heal_transition(&run);
            assert_eq!(
                model.successors(action.name, &state),
                vec![observed.clone()],
                "{} from {state:?}",
                action.name
            );
            if order.contains(&observed) {
                drop(run);
                let _ = std::fs::remove_dir_all(&dir);
            } else {
                order.push(observed.clone());
                realized.insert(observed, run);
            }
        }
    }
    assert_eq!(
        order.len(),
        aterm_spec::interp::reachable_states(&model).len()
    );
    eprintln!("GapHeal: {} states, {runs} transitions judged", order.len());
    drop(realized);

    let control = |switch: &'static std::thread::LocalKey<std::cell::Cell<bool>>,
                   name: &str,
                   path: &[&str],
                   diverges_at: &str|
     -> Vec<State> {
        switch.with(|cell| cell.set(true));
        let (_, seen) = gap_replay(&shapes, &root.path().join(name), path);
        switch.with(|cell| cell.set(false));
        let expected = gap_states(&model, path);
        let first = seen
            .iter()
            .zip(&expected)
            .position(|(seen, expected)| seen != expected)
            .unwrap_or_else(|| panic!("{name}: the code conforms down {path:?}"));
        assert_eq!(path[first], diverges_at, "{name}: {:?}", seen[first]);
        seen
    };

    let mut path = SETTLED.to_vec();
    path.extend(["HealA", "ToB", "HealB", "Tick", "HealA", "HealB", "Tick"]);
    let seen = control(&OLD_GAP_RULE, "old-gap-rule", &path, "HealB");
    let last = seen.last().unwrap();
    assert_eq!(last["had"] + last["hbd"], 0, "nothing heals D");
    assert!(!model.check_invariant("GapsClose", last), "{last:?}");

    let path = ["OldMint", "DeliverToA", "HealA", "DeliverToA"];
    let seen = control(&UNSETTLED_OK, "unsettled-ok", &path, "HealA");
    let last = seen.last().unwrap();
    assert_eq!((last["wb"], last["hab"]), (1, 1), "B wrapped twice");
    assert!(!model.check_invariant("NoStorm", last));

    let mut path = SETTLED.to_vec();
    path.push("HealA");
    for (switch, name) in [
        (&remove::UNRANKED, "unranked"),
        (&log_entries::GAPS_FROM_FIRST_SIGHT, "gaps-from-first-sight"),
    ] {
        let seen = control(switch, name, &path, "HealB");
        let last = seen.last().unwrap();
        assert_eq!(
            (last["had"], last["hbd"]),
            (1, 1),
            "{name}: D wrapped twice"
        );
        assert!(!model.check_invariant("NoStorm", last), "{name}");
    }

    shapes.stop();
}

/// The `Wrap`s of `epoch` to `to` that `paths` holds.
fn heals_of(paths: &Paths, epoch: &RecordId, to: &Recipient) -> BTreeSet<RecordId> {
    log_of(paths)
        .records()
        .filter(|record| wrap_of(record) == Some((*epoch, *to)))
        .map(|record| *record.id())
        .collect()
}

/// The recipients `paths` reads `epoch` as wrapped to.
fn wrapped_to(paths: &Paths, epoch: &RecordId) -> BTreeSet<Recipient> {
    let identity = read_identity(paths).unwrap().unwrap();
    let opened = log_of(paths)
        .open_audience_entries(&identity)
        .unwrap()
        .unwrap();
    opened.wrapped_recipients[epoch].clone()
}

/// A3: a duplicate heal, which `GapHeal`'s `NoStorm` rules out only under its timing
/// assumptions, is admitted wherever it lands, changes no recipient set, and costs one keyed
/// record (the design's `AtMostTwoHeals` bound). On `OldMint`'s lab both A and B hold C's batch
/// whole; once it settles A, ranked first, heals D. Breaking the first assumption, A's heal has
/// not reached B when B's rank falls due a step later, so B heals D too. Each Mac then takes the
/// other's `Wrap`: both are in its log, epoch 2's recipients are what its own heal made them,
/// its keyed count rises by exactly the one record, and no pass on either Mac signs a third.
///
/// NEGATIVE CONTROL: a log that refuses a second `Wrap` of one epoch to one recipient leaves
/// the other Mac's heal out, and the admitted assert fails; one that adds the recipient again
/// (a list, not a set) fails the recipient assert.
#[test]
fn a_duplicate_heal_is_admitted_changes_no_recipient_set_and_is_counted() {
    let (lab, batch, _) = GapShapes::old_mint();
    let (a, b) = (lab.home(lab.a), lab.home(lab.b));
    let d = lab.d.expect("OldMint seats D").recipient;
    let e2 = *batch[0].id();
    give(&a, &batch);
    give(&b, &batch);
    let t0 = aterm_time::unix_millis().unwrap();
    for paths in [&a, &b] {
        assert!(
            log_entries::close_wrap_gaps(paths, t0).unwrap().is_empty(),
            "nothing heals before the batch settles"
        );
    }
    let by_a = log_entries::close_wrap_gaps(&a, t0 + TICK).unwrap();
    assert!(
        log_entries::close_wrap_gaps(&b, t0 + TICK)
            .unwrap()
            .is_empty(),
        "B's rank is a step behind A's"
    );
    let by_b = log_entries::close_wrap_gaps(&b, t0 + TICK + GAP_STEP_MS).unwrap();
    for (who, signed) in [("A", &by_a), ("B", &by_b)] {
        assert_eq!(
            signed.iter().filter_map(wrap_of).collect::<Vec<_>>(),
            [(e2, d)],
            "{who} heals D once"
        );
    }
    for (paths, theirs) in [(&a, &by_b), (&b, &by_a)] {
        let wrapped = wrapped_to(paths, &e2);
        let counts = log_of(paths).counts();
        assert!(wrapped.contains(&d), "its own heal wraps D");
        give(paths, theirs);
        let log = log_of(paths);
        assert!(
            theirs.iter().all(|record| log.get(record.id()).is_some()),
            "the other Mac's heal is admitted"
        );
        assert_eq!(heals_of(paths, &e2, &d).len(), 2, "two heals of D, no more");
        assert_eq!(
            wrapped_to(paths, &e2),
            wrapped,
            "the duplicate changes no recipient"
        );
        let after = log.counts();
        assert_eq!(
            (after.weighed, after.keyed),
            (counts.weighed, counts.keyed + 1),
            "the duplicate costs one keyed record"
        );
    }
    for paths in [&a, &b] {
        assert!(
            log_entries::close_wrap_gaps(paths, t0 + TICK + 2 * GAP_STEP_MS)
                .unwrap()
                .is_empty(),
            "no third heal"
        );
    }
    lab.macs.server.stop();
}

/// A3: the 2026-11-03 drop on real homes (one-mesh §2.1; A1's log-level replay
/// `eighty_eight_seats_wrapped_to_three_heal_one_wrap_per_gap`, run through the client): m22
/// seats 84 Macs beside m22, m7, m40 and m41, and m41, holding every one of those admissions in
/// its causal past, mints `[everyone]` epoch 2 wrapped to m22, m7 and m40 alone, not even to
/// itself, a batch that ends in `Wrap`s. Every home's own closer
/// ([`log_entries::close_wrap_gaps`], with its own roster reads) runs at each step of an injected
/// clock, and every record reaches every home before the next step: nothing heals before the
/// batch settles, then each of the 85 gaps (the 84 and m41) gets one `Wrap`, signed by the holder
/// its rank puts first, and later steps sign nothing. m41 cannot open epoch 2 until healed, so it
/// heals nothing; the 84 seats have no home, so they never look.
///
/// NEGATIVE CONTROL: with `OLD_GAP_RULE` on (0.154.0 read an omission the minter knew as a
/// revocation), no closer signs and the 85 stay out of epoch 2. A closer that reads its roster
/// through `local_swarm_roster_for_log` instead, which folds the left-out seats it finds rows
/// for into the deny lists, refuses those gaps, and the one-`Wrap`-per-gap assert fails (40 of
/// the 85 healed in the run that checked it).
#[test]
fn the_eighty_eight_to_three_drop_heals_on_real_homes() {
    use crate::commons::binding::Request;
    use crate::membership_log::{Entrant, Evidence};

    let (macs, homes) = lab_homes(&["m40", "m41"]);
    let m22 = &homes["m22"];
    let trust = member_key(m22).0.trust();
    for tag in 100..184u8 {
        let identity = crate::age::Identity::from_secret_bytes([tag; 32]);
        let request = Request::create(
            trust.network,
            trust.authority,
            [tag; 16],
            [tag; 16],
            &identity,
            &[tag; 32],
        )
        .unwrap();
        let entrant = Entrant {
            request,
            node_key: [tag; 32],
            name: format!("m{tag}"),
            label: format!("m{tag}"),
        };
        log_entries::admit_member_in_log(m22, entrant, Evidence::Phrase).unwrap();
    }
    let wire = load_records(m22).unwrap().encode();
    for paths in homes.values() {
        save_records(paths, &wire).unwrap();
    }
    let holders = ["m22", "m7", "m40"];
    let looking = ["m22", "m7", "m40", "m41"];
    let current = log_of(m22).current_recipients(&EpochAudience::Everyone);
    assert_eq!(current.len(), 88, "four homes and the 84 m22 seated");
    // Each seat as its home reads it before epoch 2: a Mac the newest epoch leaves out reads
    // itself as no member until a heal wraps it (the `left_out` holds one-mesh D6 retires).
    let seats: BTreeMap<&str, Seat> = looking
        .iter()
        .map(|home| (*home, seat(&homes[home])))
        .collect();
    let to: BTreeSet<Recipient> = holders.iter().map(|home| seats[home].recipient).collect();
    let batch = old_batch(&homes["m41"], &to);
    let e2 = *batch[0].id();
    for paths in homes.values() {
        give(paths, &batch);
    }
    let first: BTreeMap<Recipient, MemberId> = {
        let log = log_of(m22);
        let opened = log
            .open_audience_entries(&read_identity(m22).unwrap().unwrap())
            .unwrap()
            .unwrap();
        log.wrap_gaps(&opened, &[crate::membership_log::SeatAudience::Everyone])
            .iter()
            .map(|gap| {
                assert_eq!(gap.epoch, e2);
                let ranked_first = holders
                    .iter()
                    .map(|home| seats[home].actor)
                    .find(|actor| log.gap_rank(&opened, gap, actor) == Some(0))
                    .expect("one holder ranks first");
                (gap.recipient, ranked_first)
            })
            .collect()
    };
    assert_eq!(
        first.len(),
        85,
        "every current seat epoch 2 omits is a gap, its minter's too"
    );
    assert!(first.contains_key(&seats["m41"].recipient));

    // Each step: every home's pass at `now`, then every record to every home.
    let heal = |root: &Path, old: bool| -> (Vec<Record>, BTreeMap<&str, Paths>) {
        let copies: BTreeMap<&str, Paths> = looking
            .iter()
            .map(|home| {
                let dir = root.join(home);
                copy_private_tree(&homes[home].home, &dir);
                (*home, Paths::for_home(&dir))
            })
            .collect();
        let t0 = aterm_time::unix_millis().unwrap();
        let mut signed = Vec::new();
        OLD_GAP_RULE.with(|on| on.set(old));
        for (step, now) in [
            t0,
            t0 + TICK,
            t0 + TICK + GAP_STEP_MS,
            t0 + TICK + 2 * GAP_STEP_MS,
        ]
        .into_iter()
        .enumerate()
        {
            let mut new = Vec::new();
            for paths in copies.values() {
                new.extend(log_entries::close_wrap_gaps(paths, now).unwrap());
            }
            if step != 1 {
                assert!(
                    new.is_empty(),
                    "step {step}: heals come only at the settle, all of them"
                );
            }
            for paths in copies.values() {
                give(paths, &new);
            }
            signed.extend(new);
        }
        OLD_GAP_RULE.with(|on| on.set(false));
        (signed, copies)
    };

    let scratch = aterm_tempfile::tempdir().expect("tempdir");
    let (signed, copies) = heal(&scratch.path().join("heal"), false);
    assert_eq!(signed.len(), 85, "one `Wrap` per gap");
    for record in &signed {
        let (epoch, recipient) = wrap_of(record).expect("a heal is a `Wrap`");
        assert_eq!(epoch, e2);
        assert_eq!(
            first.get(&recipient),
            Some(record.actor()),
            "the holder ranked first heals"
        );
    }
    for paths in copies.values() {
        assert_eq!(
            wrapped_to(paths, &e2),
            current,
            "every current seat holds epoch 2"
        );
    }

    let (signed, copies) = heal(&scratch.path().join("old"), true);
    assert!(signed.is_empty(), "CONTROL: the old rule heals nothing");
    for home in holders {
        assert_eq!(
            wrapped_to(&copies[home], &e2),
            to,
            "CONTROL: the 85 stay out"
        );
    }
    macs.server.stop();
}
