// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `MeshRelay` conformance (`docs/DESIGN-alab-one-mesh-2026-11-05.md` §9, step B8): the relay's
//! bursts, the egress, `Inventory` and the revocation pass, on in-process Macs (the B7a lab of
//! `tests` and the B7b stand-in stores of `tests::tables`), refine
//! `aterm_models_swarm::mesh_relay_model`. A, B and C, and D where it arrives, run their node
//! services over loopback `Bus` streams on the lab's injected link and wall clocks; the
//! harness plays the model's environment (a publish, a partition, a Mac that stops, a size cut,
//! 7 days passing, a membership change and when a Mac reads it, a forger, a rebuild) and lets
//! the relays and `Inventory` run until nothing more moves: rounds of a barrier record from each
//! up Mac, each passing every relay between up Macs (each relay commits its group before it
//! reports the burst, and reads a gap's notice before the commit), at least two and until a
//! round moves nothing but its own records; then every `Inventory` worker is idle, and every
//! ingest consumer has applied its log. That state, projected ([`mesh_relay_transition`]), is
//! the model's unique state with no relay or `Inventory` enabled after the same step
//! ([`aterm_models_swarm::SETTLES`]).
//!
//! NEGATIVE CONTROL: `NO_INVENTORY_ON_GAP` (`crate::inventory`). With it on, the cut-and-return
//! path diverges where B comes back: no `Inventory` follows the gap notices, and B's store never
//! holds C's record (the lab starts no presence, so the digest trigger, which would repair that
//! lasting difference too, asks nothing).

use super::super::{DAY_MS, HANG, Lab, Mac, actor, founded, payload_of, request, seating, sign};
use super::{Fake, TABLE, start_all, tables_mac};
use crate::logs::MESH_DIR;
use crate::relay::{BusDial, Event, group_of};
use crate::stores::{self, Kind, TableKey};
use crate::{MeshLog, Standings};
use aterm_models_swarm::{SETTLES, mesh_relay_model};
use aterm_secrets::membership_log::{Body, Entrant, Evidence, Log, Record};
use aterm_spec::derive::Model;
use aterm_spec::interp::{self, State};
use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Instant;

/// C's table record, the model's `r`.
const R: &str = "the-record";
/// C's presence rows: `p1`, and `p3` after the rebuild.
const P1: &str = "p1";
const P3: &str = "p3";
/// The forger's record under C's subject.
const FORGED: &str = "forged";

const A: usize = 0;
const B: usize = 1;
const C: usize = 2;
const D: usize = 3;

/// The membership log's seats in the members shape: a rogue, A, B, C and a fifth seat, by
/// device tag. A seat's device key derives from its tag as a lab Mac's node key does from its
/// own, so the tags stay clear of the Macs' (1 to 4): a seat whose device key is another Mac's
/// node key reads that Mac's removal as its own.
const ROGUE: u8 = 11;
const A_SEAT: u8 = 12;
const B_SEAT: u8 = 13;
const C_SEAT: u8 = 14;
const FIFTH: u8 = 15;

/// What the harness did: the model's environment variables, and what a cut or a rebuild left
/// that the Macs no longer show.
#[derive(Clone, Default)]
struct Env {
    sc: i64,
    cr: i64,
    bw: i64,
    ex: i64,
    cs: i64,
    cw: i64,
    dj: i64,
    ev: i64,
    lb: i64,
    fg: i64,
    cp: i64,
    /// `r`'s offsets in A's log, and in C's, when the cut dropped them.
    cut_a: Option<Vec<u64>>,
    cut_c: Option<Vec<u64>>,
    /// C's relays' progress over its presence rows when it was rebuilt (`qca`, `qcb`).
    rebuilt: Option<(i64, i64)>,
}

/// One path's lab.
struct Run {
    lab: Lab,
    macs: Vec<(Mac, Arc<Fake>)>,
    /// The membership log of the members shape, and its genesis.
    log: Option<(Log, Record)>,
    /// The barrier records go on `metrics.log` in the presence shape, on `tables.log` otherwise.
    presence: bool,
    env: Env,
    /// The last projection: what a Mac that is down still holds.
    last: State,
    marks: u32,
}

/// Where a path left the model.
#[derive(Debug)]
struct Divergence {
    at: &'static str,
    /// Each variable that differs: the model's value and the code's.
    diff: Vec<(&'static str, i64, Option<i64>)>,
}

/// The one state the relay and `Inventory` settle to from `state`, with no environment step.
fn settle(model: &Model, state: &State) -> State {
    let mut seen = BTreeSet::new();
    let mut ends = BTreeSet::new();
    let mut todo = vec![state.clone()];
    while let Some(at) = todo.pop() {
        if !seen.insert(at.clone()) {
            continue;
        }
        let next: Vec<State> = SETTLES
            .iter()
            .flat_map(|action| model.successors(action, &at))
            .collect();
        if next.is_empty() {
            ends.insert(at);
        }
        todo.extend(next);
    }
    assert_eq!(
        ends.len(),
        1,
        "from {state:?} the relay settles to {} states",
        ends.len()
    );
    ends.pop_first().expect("one end")
}

/// The environment step `action` from the settled `state`, settled.
fn after(model: &Model, state: &State, action: &str) -> State {
    let next = model.successors(action, state);
    assert_eq!(
        next.len(),
        1,
        "`{action}` from {state:?} has {} successors",
        next.len()
    );
    settle(model, &next[0])
}

/// Every record `mac`'s `log` holds under `filter`: its offset, subject and payload.
fn records(mac: &Mac, log: MeshLog, filter: &str) -> Vec<(u64, String, String)> {
    let mut client = mac
        .mesh()
        .read_client(log, filter, Some(HANG))
        .expect("a reader");
    let mut out = Vec::new();
    // A cut leaves the filter complete only from its oldest: a fetch below it is refused.
    let mut from = client.oldest(filter).expect("the oldest").0.unwrap_or(0);
    loop {
        let (page, (next, head)) = client.fetch(from, filter, 1024).expect("a page");
        for (offset, subject, body) in page {
            out.push((offset, subject, payload_of(&body)));
        }
        if next >= head {
            return out;
        }
        from = next;
    }
}

/// The offsets of the records under `subject` whose payload ends with `text`.
fn offsets(mac: &Mac, log: MeshLog, subject: &str, text: &str) -> Vec<u64> {
    records(mac, log, subject)
        .into_iter()
        .filter(|(_, _, payload)| payload.ends_with(text))
        .map(|(offset, _, _)| offset)
        .collect()
}

/// How many of `at` lie below where `from`'s relay to `to` resumes on `log`.
fn passed(from: &Mac, to: &str, log: MeshLog, at: &[u64]) -> i64 {
    let start = from.mesh().broker(log).group_start(&group_of(to));
    at.iter().filter(|offset| **offset < start).count() as i64
}

/// The row a payload of C's presence subject stands for: 1 for `p1`, 3 for `p3`.
fn row(payload: &str) -> i64 {
    if payload.ends_with(P3) {
        3
    } else if payload.ends_with(P1) {
        1
    } else {
        0
    }
}

/// C's presence subject.
fn now_subject(c: &Mac) -> String {
    format!("/m/{}/shared/now/presence", c.principal)
}

/// The newest row `mac` serves of `subject`, from its last-value face.
fn newest(mac: &Mac, subject: &str) -> i64 {
    let filter = aterm_metrics::rules::NOW_FILTER;
    let mut client = mac
        .mesh()
        .read_client(MeshLog::Metrics, filter, Some(HANG))
        .expect("a reader");
    let (rows, _) = client.last_all(filter).expect("the last-value face");
    rows.iter()
        .find(|(_, s, _)| s == subject)
        .map_or(0, |(_, _, body)| row(&payload_of(body)))
}

/// The last events `mac` reported, its links and the seats it takes as origins, for a wait that
/// failed.
fn tail(mac: &Mac) -> String {
    let links: Vec<(String, bool, Option<String>)> = mac
        .run
        .as_ref()
        .map(|run| {
            run.table
                .links()
                .iter()
                .map(|link| {
                    (
                        astream_broker::identity_principal(link.peer().key.bytes()),
                        link.dialed(),
                        link.closed(),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    let seats: Vec<String> = mac
        .run
        .as_ref()
        .and_then(|run| run.mesh.seats_by_principal())
        .map(|seats| seats.into_keys().collect())
        .unwrap_or_default();
    let events = super::super::lock(&mac.seen.events);
    let from = events.len().saturating_sub(40);
    format!(
        "{} links {links:?}, seats {seats:?}, reported {:#?}",
        mac.principal,
        &events[from..]
    )
}

/// Whether `mac`'s `log` holds a record under `subject` whose payload ends with `text`.
fn holds(mac: &Mac, log: MeshLog, subject: &str, text: &str) -> bool {
    records(mac, log, subject)
        .iter()
        .any(|(_, _, payload)| payload.ends_with(text))
}

/// Wait until `mac`'s `log` holds a record under `subject` whose payload ends with `text`, one
/// `from` relays to it: `from`'s relay reports each burst after `mac` took what it carried, so
/// each event `from` reports is a moment to look again.
fn until_record(from: &Mac, mac: &Mac, log: MeshLog, subject: &str, text: &str) {
    let deadline = Instant::now() + HANG;
    let mut events = super::super::lock(&from.seen.events);
    while !holds(mac, log, subject, text) {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            drop(events);
            let all = if log == MeshLog::Metrics {
                "/m/>"
            } else {
                "/t/>"
            };
            let held: Vec<(u64, String, String)> =
                records(mac, log, all).into_iter().rev().take(12).collect();
            panic!(
                "{} did not hold {text:?} under {subject} within the hang detector (holds now: \
                 {}; its last records {held:?}); {}; {}",
                mac.principal,
                holds(mac, log, subject, text),
                tail(from),
                tail(mac)
            );
        }
        events = from
            .seen
            .moved
            .wait_timeout(events, left)
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .0;
    }
}

/// Wait until `from`'s relay to `to` on `log` resumes past `offset`: its group commits before
/// the relay reports the burst, so each report is a moment to look again.
fn until_passed(from: &Mac, to: &str, log: MeshLog, offset: u64) {
    let broker = Arc::clone(from.mesh().broker(log));
    let group = group_of(to);
    from.seen.until(
        &format!(
            "{}'s relay to {to} on {} passes {offset}",
            from.principal,
            log.file()
        ),
        |_| broker.group_start(&group) > offset,
    );
}

impl Run {
    fn new(path: &[&str]) -> Self {
        let lab = Lab::new();
        let mut macs: Vec<(Mac, Arc<Fake>)> =
            (1..=3).map(|tag| tables_mac(&lab, tag, &["w"])).collect();
        start_all(&lab, &mut macs);
        let log = matches!(path.first(), Some(&("Remove" | "Readmit"))).then(|| {
            let rogue = astream_broker::origin::public_key(&[201; 32]);
            let fifth = astream_broker::origin::public_key(&[205; 32]);
            let founded = founded(&[
                (ROGUE, rogue),
                (A_SEAT, macs[A].0.key),
                (B_SEAT, macs[B].0.key),
                (C_SEAT, macs[C].0.key),
                (FIFTH, fifth),
            ]);
            for (mac, _) in &macs {
                mac.read(Standings::of_log(&founded.0));
                mac.follow();
            }
            lab.tick();
            founded
        });
        let mut run = Self {
            lab,
            macs,
            log,
            presence: path.contains(&"PublishRow"),
            env: Env::default(),
            last: State::new(),
            marks: 0,
        };
        run.last = mesh_relay_transition(&run);
        run
    }

    fn mac(&self, at: usize) -> &Mac {
        &self.macs[at].0
    }

    fn up(&self, at: usize) -> bool {
        match at {
            A => true,
            B => self.env.bw != 1,
            C => self.env.cw != 1,
            _ => self.env.dj == 1,
        }
    }

    /// Whether `from` seats `to`: only A's and B's reading of C ever changes.
    fn seats(&self, from: usize, to: usize) -> bool {
        match (from, to) {
            (A, C) => self.env.ev != 1,
            (B, C) => self.env.lb != 1,
            _ => true,
        }
    }

    /// Whether `from`'s relay to `to` runs: both up, not split apart, each seating the other.
    fn linked(&self, from: usize, to: usize) -> bool {
        from != to
            && self.up(from)
            && self.up(to)
            && ((from != C && to != C) || self.env.cs != 1)
            && self.seats(from, to)
            && self.seats(to, from)
    }

    /// Whether `from`'s relay to `to` holds C's records back: `to` refuses C while `from` seats
    /// it, so the relay's group stays below the first of them.
    fn holds_back(&self, from: usize, to: usize) -> bool {
        match (from, to) {
            (B, A) => self.env.ev == 1 && self.env.lb != 1,
            (A, B) => self.env.lb == 1 && self.env.ev != 1,
            _ => false,
        }
    }

    /// The mark of every Mac's events, taken before a step that (re)opens links.
    fn marks(&self) -> Vec<usize> {
        self.macs.iter().map(|(mac, _)| mac.seen.mark()).collect()
    }

    /// Wait until every relay a step (re)opened at `at` runs: `at` follows each Mac it is linked to
    /// and each of those follows `at`, from the marks `since` taken before the step (a Mac that
    /// joined since has none). A relay with no cursor starts at its log's head, so a record
    /// written before it follows reaches the store by `Inventory` alone, never the log.
    fn until_linked(&self, at: usize, since: &[usize]) {
        let mark = |p: usize| since.get(p).copied().unwrap_or(0);
        let mac = self.mac(at);
        let peers: Vec<&Mac> = (0..self.macs.len())
            .filter(|p| self.linked(at, *p))
            .map(|p| self.mac(p))
            .collect();
        mac.until_following(&peers, mark(at));
        for p in (0..self.macs.len()).filter(|p| self.linked(*p, at)) {
            self.mac(p).until_following(&[mac], mark(p));
        }
    }

    fn start(&mut self, at: usize) {
        let Self { lab, macs, .. } = self;
        let (mac, fake) = &mut macs[at];
        mac.start(lab);
        fake.serve(&mac.mesh().egress().expect("the mesh moves tables"));
    }

    /// Wait until `at`'s egress published every record its store holds.
    fn published(&self, at: usize) {
        let (mac, fake) = &self.macs[at];
        let key = TableKey {
            table: TABLE,
            recovery: false,
        };
        let length = fake.records(&TABLE).len() as u64;
        let egress = mac.mesh().egress().expect("an egress");
        assert!(egress.wait_published(key, length, Instant::now() + HANG));
    }

    /// Cut `at`'s `tables.log` below its ingest consumer's floor, as the size rule does once
    /// every other neighbour's cursor passed `r`: answers `r`'s offsets as they stood.
    fn cut(&self, at: usize) -> Vec<u64> {
        let mac = self.mac(at);
        let c = self.mac(C);
        let held = offsets(
            mac,
            MeshLog::Tables,
            &stores::table_subject(&TABLE, &c.principal),
            R,
        );
        let ingest = mac.mesh().ingest().expect("an ingest consumer");
        let floor = ingest.floor(mac.mesh());
        assert!(
            held.iter().all(|offset| *offset < floor),
            "{} holds {R} at {held:?}, below the floor {floor}",
            mac.principal
        );
        mac.mesh().cut_tables_for_test(floor).expect("the cut");
        assert!(
            offsets(
                mac,
                MeshLog::Tables,
                &stores::table_subject(&TABLE, &c.principal),
                R
            )
            .is_empty(),
            "the cut dropped {R}"
        );
        held
    }

    /// The members shape's log, after `by` signs `body`.
    fn signed(&mut self, by: u8, at: u64, body: Body) -> Standings {
        let (log, _) = self.log.as_mut().expect("the members shape");
        sign(log, by, at, body);
        Standings::of_log(log)
    }

    fn genesis(&self) -> [u8; 32] {
        *self.log.as_ref().expect("the members shape").1.id()
    }

    /// A reads the membership log as it stands, and applies it.
    fn a_reads(&mut self, standings: Standings) {
        self.mac(A).read(standings);
        self.mac(A).follow();
        self.lab.tick();
    }

    /// Run the environment step `action` on the lab.
    fn step(&mut self, action: &'static str) {
        match action {
            "Publish" => {
                self.macs[C].1.write("w", R);
                self.published(C);
                if self.env.sc == 0 {
                    self.env.sc = if self.env.bw == 1 { 2 } else { 1 };
                }
                self.env.cr = 1;
            }
            "Split" => {
                self.env.sc = 1;
                self.env.cs = 1;
                let c = self.mac(C);
                let peers: Vec<String> = [A, B, D]
                    .into_iter()
                    .filter(|at| *at < self.macs.len() && self.up(*at))
                    .map(|at| self.mac(at).principal.clone())
                    .collect();
                let since = c.seen.mark();
                c.cut(&self.lab);
                c.seen.until("C's relays end", |events| {
                    let ended: BTreeSet<(String, MeshLog)> = events
                        .iter()
                        .skip(since)
                        .filter_map(|event| match event {
                            Event::Ended { peer, log, .. } => Some((peer.clone(), *log)),
                            _ => None,
                        })
                        .collect();
                    peers.iter().all(|peer| {
                        MeshLog::ALL
                            .iter()
                            .all(|log| ended.contains(&(peer.clone(), *log)))
                    })
                });
            }
            "Join" => {
                self.env.cs = 2;
                let since = self.marks();
                self.mac(C).heal(&self.lab);
                self.until_linked(C, &since);
            }
            "AwayC" => {
                self.env.cw = 1;
                self.macs[C].0.stop();
            }
            "ReturnC" => {
                self.env.cw = 2;
                let since = self.marks();
                self.start(C);
                self.lab.tick();
                self.until_linked(C, &since);
            }
            "Arrive" => {
                self.env.dj = 1;
                let since = self.marks();
                let (mut d, fake) = tables_mac(&self.lab, 4, &["w"]);
                let all = {
                    let mut all: Vec<&Mac> = self.macs.iter().map(|(mac, _)| mac).collect();
                    all.push(&d);
                    seating(&all)
                };
                for (mac, _) in &self.macs {
                    mac.read(all.clone());
                    if mac.run.is_some() {
                        mac.follow();
                    }
                }
                d.read(all);
                d.start(&self.lab);
                fake.serve(&d.mesh().egress().expect("the mesh moves tables"));
                self.macs.push((d, fake));
                self.lab.tick();
                self.until_linked(D, &since);
            }
            "AwayB" => {
                self.env.bw = 1;
                self.macs[B].0.stop();
            }
            "ReturnB" => {
                self.env.bw = 2;
                let since = self.marks();
                self.start(B);
                self.lab.tick();
                self.until_linked(B, &since);
            }
            "Expire" => {
                self.env.ex = 1;
                self.lab.age(8 * DAY_MS);
            }
            "CutC" => {
                let held = self.cut(C);
                self.env.cut_c = Some(held);
            }
            "CutA" => {
                let held = self.cut(A);
                self.env.cut_a = Some(held);
            }
            "Remove" => {
                self.env.sc = 3;
                self.env.ev = 1;
                let cutoff = self.genesis();
                let read = self.signed(
                    ROGUE,
                    8,
                    Body::Remove {
                        member: actor(C_SEAT),
                        cutoff,
                    },
                );
                assert!(read.removed.contains(&self.mac(C).key));
                self.a_reads(read);
            }
            "Readmit" => {
                self.env.sc = 3;
                self.env.ev = 2;
                let cutoff = self.genesis();
                self.signed(
                    A_SEAT,
                    8,
                    Body::Remove {
                        member: actor(C_SEAT),
                        cutoff,
                    },
                );
                let key = self.mac(C).key;
                let read = self.signed(
                    A_SEAT,
                    9,
                    Body::Admit {
                        entrant: Entrant {
                            request: request(100 + C_SEAT),
                            node_key: key,
                            name: format!("mac-{C_SEAT}"),
                            label: format!("m{C_SEAT}"),
                        },
                        evidence: Evidence::Manual,
                    },
                );
                assert!(read.is_current(&key), "the new seat binds the kept key");
                self.a_reads(read);
            }
            "Void" => {
                self.env.ev = 3;
                let cutoff = self.genesis();
                let mut read = None;
                for who in [A_SEAT, B_SEAT, FIFTH] {
                    read = Some(self.signed(
                        who,
                        9,
                        Body::Remove {
                            member: actor(ROGUE),
                            cutoff,
                        },
                    ));
                }
                let read = read.expect("a quorum signed");
                assert!(
                    read.is_current(&self.mac(C).key),
                    "the rogue's removal is void"
                );
                self.a_reads(read);
            }
            "Learn" => {
                self.env.lb = self.env.ev;
                let (log, _) = self.log.as_ref().expect("the members shape");
                let read = Standings::of_log(log);
                self.mac(B).read(read);
                self.mac(B).follow();
                self.lab.tick();
            }
            "Forge" => {
                self.env.fg = 1;
                self.forge();
            }
            "PublishRow" => {
                self.env.sc = 4;
                self.env.cp += 1;
                let payload = if self.env.cp == 1 { P1 } else { P3 };
                self.mac(C).metric("now/presence", payload);
            }
            "Rebuild" => {
                self.env.cp = 2;
                self.env.rebuilt = Some((self.last["qca"], self.last["qcb"]));
                // The rebuilt C's relays start at their logs' heads, and metrics have no
                // `Inventory`: what a neighbour writes before its relay to C follows never
                // reaches C.
                let linked = self.marks();
                let Self { lab, macs, .. } = self;
                let (c, fake) = &mut macs[C];
                c.stop();
                std::fs::remove_dir_all(c.dir.path().join(MESH_DIR)).expect("the rebuild");
                let since = c.seen.mark();
                c.start(lab);
                fake.serve(&c.mesh().egress().expect("the mesh moves tables"));
                c.seen.until("the rebuilt lease settles", |events| {
                    events.iter().skip(since).any(|e| {
                        matches!(
                            e,
                            Event::Settled {
                                log: MeshLog::Metrics,
                                ..
                            }
                        )
                    })
                });
                lab.tick();
                self.until_linked(C, &linked);
            }
            other => panic!("`{other}` is no environment step the harness plays"),
        }
    }

    /// B, a neighbour of A, offers A a record under C's subject that C's key did not sign.
    fn forge(&self) {
        let (a, b, c) = (self.mac(A), self.mac(B), self.mac(C));
        let link = b
            .run
            .as_ref()
            .expect("B runs")
            .table
            .links()
            .into_iter()
            .find(|link| astream_broker::identity_principal(link.peer().key.bytes()) == a.principal)
            .expect("a link to A");
        let (stream, grants) = BusDial::new(link, MeshLog::Tables)
            .open()
            .expect("A serves B's Bus stream");
        let stream: Arc<dyn astream_broker::StreamLink> = Arc::new(stream);
        stream.set_read_timeout(Some(HANG)).expect("bounded");
        let (mut client, _closer) = astream_broker::connect_stream(stream);
        client
            .attach_all(grants.iter().map(|cap| (cap.filter.as_str(), &cap.tag[..])))
            .expect("B's grants attach");
        let subject = stores::table_subject(&TABLE, &c.principal);
        let seq = astream_broker::incarnation_base(9).expect("an incarnation") + 1;
        let payload = stores::table_payload(Kind::Operation, format!("w|{FORGED}").as_bytes());
        let body = astream_broker::origin::seal(&[77; 32], &subject, seq, &payload);
        let refused = client
            .relay(
                astream_broker::producer_id_of(&c.principal),
                seq,
                &subject,
                &body,
            )
            .expect_err("A refuses the forgery");
        let refusal = astream_broker::broker_refusal(&refused)
            .unwrap_or_else(|| panic!("a broker refusal: {refused}"));
        // While A seats C the signature is checked and the record refused for good; while it
        // does not, the record is not judged, and the edge that carries one holds it.
        let code = if self.seats(A, C) {
            astream_broker::proto::ERR_ORIGIN_REFUSED
        } else {
            astream_broker::proto::ERR_ORIGIN_UNKNOWN
        };
        assert_eq!(refusal.code, code, "A's answer to the forgery: {refusal:?}");
    }

    /// Every record but a barrier record that the up Macs' `log` holds, by Mac.
    fn moved(&self, log: MeshLog, up: &[usize]) -> Vec<Vec<(u64, String)>> {
        let filter = if log == MeshLog::Metrics {
            "/m/>"
        } else {
            "/t/>"
        };
        up.iter()
            .map(|at| {
                records(self.mac(*at), log, filter)
                    .into_iter()
                    .filter(|(_, _, payload)| !payload.contains("mark-"))
                    .map(|(offset, subject, _)| (offset, subject))
                    .collect()
            })
            .collect()
    }

    /// One barrier round: each up Mac publishes a barrier record, and each reaches every Mac its
    /// relays run to, past each relay's group unless that relay holds C's records back.
    fn round(&mut self, log: MeshLog, up: &[usize]) {
        self.marks += 1;
        let marks: Vec<(usize, String, u64)> = up
            .iter()
            .map(|at| {
                let text = format!("mark-{}-{at}", self.marks);
                let mac = self.mac(*at);
                let offset = if log == MeshLog::Metrics {
                    mac.metric("1m/mark", &text).offset
                } else {
                    self.macs[*at].1.write("w", &text);
                    self.published(*at);
                    mac.mesh()
                        .egress()
                        .expect("an egress")
                        .last_published(&TABLE)
                        .expect("the mark is published")
                };
                (*at, text, offset)
            })
            .collect();
        for (from, text, offset) in &marks {
            let source = self.mac(*from);
            let subject = if log == MeshLog::Metrics {
                format!("/m/{}/shared/1m/mark", source.principal)
            } else {
                stores::table_subject(&TABLE, &source.principal)
            };
            for to in up {
                if !self.linked(*from, *to) {
                    continue;
                }
                let dest = self.mac(*to);
                until_record(source, dest, log, &subject, text);
                if !self.holds_back(*from, *to) {
                    until_passed(source, &dest.principal, log, *offset);
                }
            }
        }
    }

    /// Let the relays and `Inventory` run until nothing more moves: rounds of barrier records,
    /// at least two and until a round moves nothing but its own records, then every `Inventory`
    /// idle and every ingest consumer caught up. A copy that landed on a Mac above its barrier
    /// record of one round lies below its record of the next, so the second round covers every
    /// hop the first left moving.
    fn settle(&mut self) {
        if self.env.sc == 0 {
            return;
        }
        let log = if self.presence {
            MeshLog::Metrics
        } else {
            MeshLog::Tables
        };
        let up: Vec<usize> = (0..self.macs.len()).filter(|at| self.up(*at)).collect();
        // A record goes at most one hop further each round, and no path is longer than the Macs.
        let most = up.len() + 1;
        let mut rounds = 0;
        loop {
            let before = self.moved(log, &up);
            self.round(log, &up);
            rounds += 1;
            if rounds >= 2 && self.moved(log, &up) == before {
                break;
            }
            assert!(
                rounds < most,
                "the relays still moved records after {rounds} barrier rounds"
            );
        }
        for at in &up {
            let mac = self.mac(*at);
            let inventory = mac.mesh().inventory().expect("an Inventory");
            assert!(
                inventory.wait_idle_for_test(Instant::now() + HANG),
                "{}'s Inventory is idle",
                mac.principal
            );
            if log == MeshLog::Tables
                && let Some((last, _, _)) = records(mac, MeshLog::Tables, "/t/>").last()
            {
                let ingest = mac.mesh().ingest().expect("an ingest consumer");
                assert!(
                    ingest.wait_committed_for_test(*last, Instant::now() + HANG),
                    "{}'s ingest applied its log",
                    mac.principal
                );
            }
        }
    }
}

/// `MeshRelay`'s variables, read off the lab: each Mac's log (the copies of `r` it holds, under
/// C's subject; the rows of C's presence subject, and the newest it serves), its store, its
/// relays' groups, its standings and its brokers' revocations, and the harness's record of what
/// it did. A Mac that is down shows what it held when it stopped; a cut log, the offsets `r`
/// had before the cut.
fn mesh_relay_transition(run: &Run) -> State {
    let env = &run.env;
    let last = |var: &str| run.last.get(var).copied().unwrap_or(0);
    let c = run.mac(C);
    let r_subject = stores::table_subject(&TABLE, &c.principal);
    let r_at = |at: usize| offsets(run.mac(at), MeshLog::Tables, &r_subject, R);
    let a = run.mac(A);
    let a_r = env.cut_a.clone().unwrap_or_else(|| r_at(A));
    let up_b = run.up(B);
    let up_c = run.up(C);
    let b = run.mac(B);
    let has_d = run.macs.len() > D;

    let (ha, ka) = match &env.cut_a {
        Some(held) => (held.len() as i64, i64::from(!r_at(A).is_empty())),
        None => (a_r.len() as i64, 1),
    };
    let b_r = if up_b { r_at(B) } else { Vec::new() };
    let hb = if up_b { b_r.len() as i64 } else { last("hb") };
    let (hc, kc, c_r) = match &env.cut_c {
        Some(held) => (
            held.len() as i64,
            if up_c {
                i64::from(!r_at(C).is_empty())
            } else {
                0
            },
            held.clone(),
        ),
        None if up_c => {
            let held = r_at(C);
            (held.len() as i64, 1, held)
        }
        None => (last("hc"), 1, Vec::new()),
    };
    let (hd, sd, ead) = if has_d {
        (
            r_at(D).len() as i64,
            i64::from(run.macs[D].1.holds(R)),
            passed(a, &run.mac(D).principal, MeshLog::Tables, &a_r),
        )
    } else {
        (0, 0, 0)
    };

    let current = |mac: &Mac| {
        i64::from(
            !mac.mesh()
                .standings()
                .is_some_and(|read| read.is_current(&c.key)),
        )
    };
    let revoked = |mac: &Mac| {
        i64::from(
            mac.mesh()
                .broker(MeshLog::Tables)
                .is_identity_revoked(&c.key),
        )
    };
    let forged = |mac: &Mac| {
        records(mac, MeshLog::Tables, &r_subject)
            .iter()
            .any(|(_, _, payload)| payload.ends_with(FORGED))
    };

    // C's presence rows: in C's log (before the rebuild, the old one), in A's and in B's.
    let now = now_subject(c);
    let rows = |mac: &Mac| -> Vec<(u64, i64)> {
        records(mac, MeshLog::Metrics, &now)
            .into_iter()
            .map(|(offset, _, payload)| (offset, row(&payload)))
            .filter(|(_, row)| *row > 0)
            .collect()
    };
    let highest = |rows: &[(u64, i64)]| rows.iter().map(|(_, row)| *row).max().unwrap_or(0);
    let c_progress = |to: &Mac, var: &str| -> i64 {
        if !up_c {
            return last(var);
        }
        let mine = rows(c);
        let past = |which: i64| {
            let at: Vec<u64> = mine
                .iter()
                .filter(|(_, row)| *row == which)
                .map(|(offset, _)| *offset)
                .collect();
            passed(c, &to.principal, MeshLog::Metrics, &at) > 0
        };
        match env.rebuilt {
            None => i64::from(env.cp >= 1 && past(1)),
            Some((qca, qcb)) => {
                let base = if var == "qca" { qca } else { qcb };
                if env.cp == 3 && past(3) { 2 } else { base }
            }
        }
    };
    let a_rows = rows(a);
    let qca = c_progress(a, "qca");
    let qcb = c_progress(b, "qcb");
    let ga = highest(&a_rows);
    let (gb, nb) = if up_b {
        (highest(&rows(b)), newest(b, &now))
    } else {
        (last("gb"), last("nb"))
    };
    let a_at: Vec<u64> = a_rows.iter().map(|(offset, _)| *offset).collect();

    State::from([
        ("sc", env.sc),
        ("cr", env.cr),
        ("ha", ha),
        ("hb", hb),
        ("hc", hc),
        ("hd", hd),
        ("sa", i64::from(run.macs[A].1.holds(R))),
        ("sb", i64::from(run.macs[B].1.holds(R))),
        ("sd", sd),
        ("ka", ka),
        ("kc", kc),
        (
            "eca",
            if up_c {
                passed(c, &a.principal, MeshLog::Tables, &c_r)
            } else {
                last("eca")
            },
        ),
        (
            "ecb",
            if up_c {
                passed(c, &b.principal, MeshLog::Tables, &c_r)
            } else {
                last("ecb")
            },
        ),
        ("eab", passed(a, &b.principal, MeshLog::Tables, &a_r)),
        (
            "eba",
            if up_b {
                passed(b, &a.principal, MeshLog::Tables, &b_r)
            } else {
                last("eba")
            },
        ),
        ("eac", passed(a, &c.principal, MeshLog::Tables, &a_r)),
        (
            "ebc",
            if up_b {
                passed(b, &c.principal, MeshLog::Tables, &b_r)
            } else {
                last("ebc")
            },
        ),
        ("ead", ead),
        // Every `Inventory` a notice asked for has run: the barrier waited for each idle.
        ("nab", 0),
        ("ncb", 0),
        ("nad", 0),
        ("bw", env.bw),
        ("ex", env.ex),
        ("cs", env.cs),
        ("cw", env.cw),
        ("dj", env.dj),
        ("ev", env.ev),
        ("lb", env.lb),
        ("va", current(a)),
        ("vb", if up_b { current(b) } else { last("vb") }),
        ("xa", revoked(a)),
        ("xb", if up_b { revoked(b) } else { last("xb") }),
        ("fg", env.fg),
        ("bad", i64::from(forged(a) || (up_b && forged(b)))),
        ("cp", env.cp),
        // The lab never reuses an incarnation: the control of that rule is B7a's
        // `with_reused_incarnation_a_rebuilt_macs_record_is_lost`.
        ("ru", 0),
        ("a1", i64::from(a_rows.iter().any(|(_, row)| *row == 1))),
        ("ga", ga),
        ("gb", gb),
        ("na", newest(a, &now)),
        ("nb", nb),
        ("qca", qca),
        ("qcb", qcb),
        ("qab", passed(a, &b.principal, MeshLog::Metrics, &a_at)),
        (
            "dp",
            i64::from((qca == 2 && ga != 3) || (up_b && qcb == 2 && gb != 3)),
        ),
    ])
}

/// Where `seen` differs from `want`.
fn compare(at: &'static str, want: &State, seen: &State) -> Result<(), Divergence> {
    let names: BTreeSet<&str> = want.keys().chain(seen.keys()).copied().collect();
    let diff: Vec<(&'static str, i64, Option<i64>)> = want
        .iter()
        .filter(|(var, value)| seen.get(*var) != Some(*value))
        .map(|(var, value)| (*var, *value, seen.get(var).copied()))
        .collect();
    assert_eq!(
        names.len(),
        want.len(),
        "the projection names a variable `MeshRelay` does not: {seen:?}"
    );
    if diff.is_empty() {
        Ok(())
    } else {
        Err(Divergence { at, diff })
    }
}

/// Run `path`'s environment steps on a lab and on the model, comparing after each.
fn conform(path: &[&'static str]) -> Result<(), Divergence> {
    let model = mesh_relay_model();
    let mut want = settle(&model, &model.init_state());
    let mut run = Run::new(path);
    compare("the start", &want, &run.last)?;
    for action in path {
        want = after(&model, &want, action);
        run.step(action);
        run.settle();
        let seen = mesh_relay_transition(&run);
        if let Err(divergence) = compare(action, &want, &seen) {
            let tails: Vec<String> = run.macs.iter().map(|(mac, _)| tail(mac)).collect();
            eprintln!("{path:?} at {action}: {}", tails.join("; "));
            return Err(divergence);
        }
        run.last = seen;
    }
    Ok(())
}

fn conforms(path: &[&'static str]) {
    if let Err(divergence) = conform(path) {
        panic!("{path:?} diverges from `MeshRelay`: {divergence:?}");
    }
}

/// B is away while C publishes; C's log and A's are cut below B's cursors; B returns.
const CUT_AND_RETURN: [&str; 5] = ["AwayB", "Publish", "CutC", "CutA", "ReturnB"];

/// Each dial of `MeshRelay` breaks its own laws and no other, the two actions dead with no
/// mutant on are dial 1's, and from every settled state each environment step settles to one
/// state, which is what lets a lab that waits for quiescence stand for the model's interleavings.
#[test]
fn mesh_relay_breaks_each_law_by_its_dial_and_settles_once_after_each_step() {
    let model = mesh_relay_model();
    let laws: [(i64, &[&str]); 6] = [
        (0, &[]),
        (1, &["NoSkip", "SeqFresh"]),
        (2, &["AppendedOnce"]),
        (3, &["NewestIsHighest"]),
        (4, &["OnlyCurrentOrigins"]),
        (5, &["NoCurrentSeatRevoked"]),
    ];
    for (dial, broken) in laws {
        assert_eq!(
            interp::violated_invariants(&interp::with_buggy(&model, dial)),
            broken.iter().copied().collect::<BTreeSet<_>>(),
            "dial {dial}"
        );
    }
    let fired = interp::fired_actions(&model);
    let dead: BTreeSet<&str> = model
        .actions
        .iter()
        .map(|action| action.name)
        .filter(|name| !fired.contains(name))
        .collect();
    assert_eq!(dead, BTreeSet::from(["GapSkip", "RebuildReuse"]));

    let environment: Vec<&str> = model
        .actions
        .iter()
        .map(|action| action.name)
        .filter(|name| !SETTLES.contains(name))
        .collect();
    let settled: Vec<State> = interp::reachable_states(&model)
        .into_iter()
        .filter(|state| SETTLES.iter().all(|a| !model.action_enabled(a, state)))
        .collect();
    assert!(settled.len() > 1);
    for state in &settled {
        for action in &environment {
            for next in model.successors(action, state) {
                settle(&model, &next);
            }
        }
    }
}

/// C publishes with every Mac up: the record floods the triangle and each Mac appends it once.
#[test]
fn a_record_floods_the_cycle_once() {
    conforms(&["Publish"]);
}

/// C publishes while its links are down, and the heal carries the record.
#[test]
fn a_record_published_across_a_split_arrives_at_the_join() {
    conforms(&["Split", "Publish", "Join"]);
}

/// C publishes while split and stops; D arrives; the split heals and C comes back: A's relay
/// carries the record to D.
#[test]
fn a_mac_that_arrives_while_the_origin_is_away_gets_the_record_by_the_relay() {
    conforms(&["Split", "Publish", "AwayC", "Arrive", "Join", "ReturnC"]);
}

/// D arrives after the record flooded: its relays start at the head, and `Inventory` fills it.
#[test]
fn a_mac_that_arrives_later_gets_the_record_by_inventory() {
    conforms(&["Publish", "Arrive"]);
}

/// B comes back to logs cut below its cursors: each relay reads a gap, and `Inventory` fills B.
#[test]
fn a_cut_and_a_return_fill_the_store_by_inventory() {
    conforms(&CUT_AND_RETURN);
}

/// B comes back after more than 7 days: its cursors are dropped, the relays start at the head,
/// and `Inventory` fills it.
#[test]
fn a_return_after_seven_days_fills_the_store_by_inventory() {
    conforms(&["AwayB", "Publish", "Expire", "ReturnB"]);
}

/// Only the origin's log is cut: A's relay delivers the record, and C's gap asks an `Inventory`.
#[test]
fn a_cut_on_the_origin_alone_leaves_the_record_to_its_neighbour() {
    conforms(&["AwayB", "Publish", "CutC", "ReturnB"]);
}

/// A rogue removes C on A: B holds C's record back from A, A leaves a forgery unjudged, B lets
/// the record go once it reads the removal; a quorum voids it, and the record reaches A and B
/// once.
#[test]
fn a_removed_origins_record_is_held_let_go_and_taken_after_the_void() {
    conforms(&["Remove", "Publish", "Forge", "Learn", "Void", "Learn"]);
}

/// A key-kept re-admission of C revokes nothing, on A or on B, and A refuses a record under C's
/// subject that C's key did not sign.
#[test]
fn a_readmitted_origin_is_never_revoked() {
    conforms(&["Readmit", "Publish", "Forge", "Learn"]);
}

/// The removal is voided before B reads it: B's held record reaches A.
#[test]
fn a_record_held_for_a_removal_reaches_its_mac_once_the_removal_is_void() {
    conforms(&["Remove", "Publish", "Void", "Learn"]);
}

/// C is rebuilt with its key kept while B is away: its new row is numbered above the old one,
/// and B, which gets the new row from C before the old one from A, serves the new one.
#[test]
fn a_rebuilt_macs_new_row_is_the_newest_everywhere() {
    conforms(&["AwayB", "PublishRow", "Rebuild", "PublishRow", "ReturnB"]);
}

/// C is rebuilt with every Mac up, and its new row reaches both.
#[test]
fn a_rebuilt_macs_new_row_reaches_every_mac() {
    conforms(&["PublishRow", "Rebuild", "PublishRow"]);
}

/// NEGATIVE CONTROL (`NO_INVENTORY_ON_GAP`): a gap notice asks no `Inventory`, so the
/// cut-and-return path diverges where B comes back: its store never holds C's record.
#[test]
fn with_no_inventory_on_gap_a_cut_and_a_return_diverge() {
    crate::inventory::NO_INVENTORY_ON_GAP.with(|on| on.set(true));
    let diverged = conform(&CUT_AND_RETURN);
    crate::inventory::NO_INVENTORY_ON_GAP.with(|on| on.set(false));
    let divergence = diverged.expect_err("the control diverges");
    assert_eq!(divergence.at, "ReturnB", "{divergence:?}");
    assert_eq!(
        divergence.diff,
        [("sb", 1, Some(0))],
        "only B's store differs"
    );
}
