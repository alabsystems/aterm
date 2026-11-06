// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `FabricRouting` and `FabricHalts` conformance (`docs/DESIGN-alab-one-mesh-2026-11-05.md` §9,
//! §11 C7): the code refines `aterm_models_swarm::fabric_routing_model` and
//! `fabric_halts_model` in every state each model reaches.
//!
//! - **Routing.** Three in-process `bus.log` brokers under aterm's rules, each guarded by its own
//!   mint secret and serving the others the ring of §4.11 ([`peer_ring`]) over adopted streams, as
//!   the node service serves them over a link. A push step runs every lane its Mac's [`plan`]
//!   names once, each opened by the code's own [`Shared::open`] and driven one burst
//!   synchronously, so a step's appends are settled when it ends. B's revocation pass is played
//!   as the node service's (`aterm_mesh::logs::Mesh::apply`) moves B's broker: C's key and
//!   principal revoked, and lifted. Each model state is reached by replaying its first path on a
//!   fresh world, and each broker's records are projected onto the model after the step.
//! - **Halts.** Each Mac's [`crate::halt::Halts`], fed as its bridge feeds it: a person's
//!   request said on its Mac and applied there, every record a Mac hears applied in its log's
//!   order and kept on its log (a relay appends what it carries), an answer said back at most once
//!   a [`crate::halt::RESAY_MS`], what [`crate::halt::Halts::due`] names said again, and the
//!   removal applied by [`crate::halt::Halts::lift_gone`]. The logs are the environment: A's and
//!   B's hear each other within a step, C's only across a joined island, and the span gives up
//!   every record older than `bus.log`'s seven days.

use std::os::unix::net::UnixStream;

use astream_broker::{AnyClient, Broker, StreamLink};
use aterm_spec::derive::Model;
use aterm_spec::interp::State as ModelState;

use super::tests::{lane, pid, Mac, Peer};
use super::*;
use crate::halt::{Halts, Lift, Outcome, Said, Word};

const MUST: Duration = aterm_spec::wait::MUST_HAPPEN;
const A: usize = 0;
const B: usize = 1;
const C: usize = 2;

/// The model's checks: every law holds with no mutant on, each mutant breaks the law it is
/// written for, the non-vacuity sweep finds nothing, and each liveness obligation holds, rests
/// on every fairness it names, and is broken by its mutant.
pub(crate) fn checked(model: &Model, live: &aterm_spec::derive::Liveness, pairs: &[(i64, &str)]) {
    aterm_spec::interp::prove_and_catch(model);
    let faults = aterm_spec::verify::sweep_faults(std::slice::from_ref(model));
    assert!(faults.is_empty(), "{}: {faults:#?}", model.name);
    aterm_spec::verify::audit_liveness(model, live)
        .unwrap_or_else(|why| panic!("{}: {why}", model.name));
    for (buggy, law) in pairs {
        let broken =
            aterm_spec::interp::violated_invariants(&aterm_spec::interp::with_buggy(model, *buggy));
        assert!(
            broken.contains(law),
            "{}: mutant {buggy} breaks {broken:?}, not {law}",
            model.name
        );
    }
}

/// Every state `model` reaches, each with the first path that reaches it, in breadth-first order.
pub(crate) fn paths(model: &Model) -> Vec<(ModelState, Vec<&'static str>)> {
    let mut order = vec![(model.init_state(), Vec::new())];
    let mut seen: BTreeSet<ModelState> = [model.init_state()].into();
    let mut next = 0;
    while let Some((state, path)) = order.get(next).cloned() {
        next += 1;
        for action in &model.actions {
            for after in model.successors(action.name, &state) {
                if seen.insert(after.clone()) {
                    let mut longer = path.clone();
                    longer.push(action.name);
                    order.push((after, longer));
                }
            }
        }
    }
    order
}

/// The one state `action` leads `state` to in `model`.
pub(crate) fn successor(model: &Model, action: &str, state: &ModelState) -> ModelState {
    let mut after = model.successors(action, state);
    assert_eq!(
        after.len(),
        1,
        "{}: {action} is one step from {state:?}",
        model.name
    );
    after.pop().unwrap()
}

/// The model's states along `path` from its start.
fn walked(model: &Model, path: &[&str]) -> Vec<ModelState> {
    let mut state = model.init_state();
    path.iter()
        .map(|action| {
            assert!(
                model.action_enabled(action, &state),
                "{action} at {state:?}"
            );
            state = successor(model, action, &state);
            state.clone()
        })
        .collect()
}

/// Where a control's run first leaves the model along `path`, given the projected states.
pub(crate) fn diverges(model: &Model, path: &[&str], seen: &[ModelState]) -> &'static str {
    let expected = walked(model, path);
    let first = expected
        .iter()
        .zip(seen)
        .position(|(model_state, real)| model_state != real)
        .unwrap_or_else(|| panic!("{}: the code conforms down {path:?}", model.name));
    model
        .actions
        .iter()
        .find(|action| action.name == path[first])
        .map(|action| action.name)
        .unwrap()
}

fn state(pairs: &[(&'static str, i64)]) -> ModelState {
    pairs.iter().copied().collect()
}

// ---- Routing ------------------------------------------------------------------------------------

/// Streams a broker adopts under `key`.
struct Dial {
    broker: Arc<Broker>,
    key: [u8; 32],
}

impl StreamDial for Dial {
    fn dial(&self) -> io::Result<Arc<dyn StreamLink>> {
        let (ours, theirs) = UnixStream::pair()?;
        drop(self.broker.serve_stream(Arc::new(theirs), self.key)?);
        Ok(Arc::new(ours))
    }
}

/// A client of `mac`'s `bus.log`, adopted as `key`, holding `grants`: refused where the broker
/// revokes the key or a grant's principal.
fn try_client(mac: &Mac, key: [u8; 32], grants: &[String]) -> io::Result<AnyClient> {
    let (ours, theirs) = UnixStream::pair()?;
    drop(mac.broker.serve_stream(Arc::new(theirs), key)?);
    ours.set_read_timeout(Some(MUST))?;
    let (mut client, _closer) = astream_broker::connect_stream(Arc::new(ours));
    for grant in grants {
        let cap = Mac::mint(&mac.secret, grant)?;
        client
            .attach(&cap.filter, &cap.tag)
            .map_err(|e| io::Error::other(e.to_string()))?;
    }
    Ok(client)
}

/// The pushes Mac `i` runs over `macs`, as [`Routes::start`] builds them, the negative controls
/// read on this thread. With `wide` each peer serves the mail grant of the ring before C3, which
/// took a peer's mail for any Mac.
fn shared_of(macs: &[Mac; 3], i: usize, wide: bool) -> Shared {
    let me = macs[i].node.clone();
    let key = macs[i].key;
    let secret = macs[i].secret.clone();
    let peers: Vec<Peer> = (0..3)
        .filter(|j| *j != i)
        .map(|j| Peer::of(&macs[j]))
        .collect();
    let from = me.clone();
    Shared {
        cfg: RoutesConfig {
            me,
            key,
            source: Arc::new(Dial {
                broker: Arc::clone(&macs[i].broker),
                key,
            }),
            mint: Arc::new(move |grant| Mac::mint(&secret, grant)),
            dest: Arc::new(move |node| {
                let peer = peers
                    .iter()
                    .find(|peer| peer.node == node)
                    .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such Mac"))?;
                let mut ring = peer_ring(FLEET, &peer.node, &from, &BTreeSet::new());
                if wide {
                    ring[0] = format!("rw,p={from}:/f/{FLEET}/in/*/*/{from}/*");
                }
                Ok(Dest {
                    dial: Arc::new(Dial {
                        broker: Arc::clone(&peer.broker),
                        key,
                    }),
                    grants: ring
                        .iter()
                        .map(|grant| Mac::mint(&peer.secret, grant))
                        .collect::<io::Result<_>>()?,
                })
            }),
            note: Arc::new(|_| {}),
            seats: Arc::new(|_| true),
            bridge_state: Arc::new(|| None),
        },
        forward_all: forward_all(),
        another_group: another_group(),
        own_halts_only: own_halts_only(),
        one_node: one_node(),
        node_order_olds: node_order_olds(),
        unsealed_carry: unsealed_carry(),
        ungated_inbox: ungated_inbox(),
        give_back_carried: false,
        state: Mutex::new(State::default()),
        bell: Condvar::new(),
    }
}

/// A's session log record, which no push carries.
fn ev_subject(node: &str) -> String {
    format!("/f/{FLEET}/pub/{node}/s-1/ev")
}

/// One run of `FabricRouting`'s world.
struct Routing {
    macs: [Mac; 3],
    shared: [Shared; 3],
    sp: i64,
    cs: i64,
    lb: i64,
    bad: i64,
    ft: i64,
    r1: i64,
    r3: i64,
}

impl Routing {
    fn new(wide: bool) -> Self {
        let macs = [Mac::new(1), Mac::new(2), Mac::new(3)];
        let shared = [0, 1, 2].map(|i| shared_of(&macs, i, wide));
        let a = &macs[A];
        let mut client = try_client(
            a,
            a.key,
            &[format!("rw,p={}:/f/{FLEET}/pub/{}/>", a.node, a.node)],
        )
        .expect("A's own log");
        client
            .publish(
                pid(&a.node),
                1,
                &ev_subject(&a.node),
                b"v=1 t=1 kind=output",
            )
            .expect("A's session log record lands");
        Self {
            macs,
            shared,
            sp: 0,
            cs: 0,
            lb: 0,
            bad: 0,
            ft: 0,
            r1: 0,
            r3: 0,
        }
    }

    fn node(&self, i: usize) -> &str {
        &self.macs[i].node
    }

    /// Whether `a` and `b` share an island.
    fn linked(&self, a: &str, b: &str) -> bool {
        self.sp != 1 || (a != self.node(C) && b != self.node(C))
    }

    /// The hand-off Mac `i`'s node service writes: every Mac a member, but C on B while B's read
    /// has it removed.
    fn handoff(&self, i: usize) -> Handoff {
        Handoff {
            me: self.node(i).to_owned(),
            members: (0..3)
                .filter(|j| !(i == B && *j == C && self.lb == 1))
                .map(|j| self.node(j).to_owned())
                .collect(),
            moved: BTreeMap::new(),
            gone: BTreeSet::new(),
        }
    }

    /// Run every lane Mac `i`'s plan names once, within its island: one burst each, settled.
    fn push(&self, i: usize) {
        let me = self.node(i);
        let written: BTreeSet<String> = self.macs[i]
            .holds(&every_lane())
            .iter()
            .filter_map(|(_, subject, _, _)| mail_lane(subject))
            .filter(|(_, src)| *src == me)
            .map(|(dst, _)| dst.to_owned())
            .collect();
        let shared = &self.shared[i];
        let lanes = plan(
            me,
            &BTreeSet::new(),
            &self.handoff(i),
            &BTreeSet::new(),
            &written,
            shared.forward_all,
        );
        for lane in lanes {
            let Some(to) = lane.to() else { continue };
            if !self.linked(me, to) {
                continue;
            }
            if let Ok((mut bridge, _ungranted)) =
                shared.open(&lane, &shared.group_of(&lane), &BTreeSet::new())
            {
                let _refused_or_carried = bridge.sync_once(256);
            }
        }
    }

    /// A frame B holds, re-sent under its origin's producer and sequence by the Mac `from`, over
    /// the ring B serves it: refused while B revokes `from`.
    fn replay(&self, from: usize) {
        let (b, src) = (&self.macs[B], &self.macs[from]);
        let held = b.holds(&lane(&b.node, &src.node));
        let Some((_, subject, body, Some((producer, seq)))) = held.into_iter().next() else {
            panic!("B holds {}'s frame to replay", src.node)
        };
        let ring = peer_ring(FLEET, &b.node, &src.node, &BTreeSet::new());
        if let Ok(mut client) = try_client(b, src.key, &ring) {
            let _held_once = client.publish(producer, seq, &subject, &body);
        }
    }

    fn apply(&mut self, action: &str) {
        let m3b = self.m3b();
        match action {
            "Split" => self.sp = 1,
            "Join" => self.sp = 2,
            "Remove" => self.cs = 1,
            "Readmit" => self.cs = 2,
            "RevokeB" => {
                let (b, c) = (&self.macs[B], &self.macs[C]);
                b.broker.revoke_key(c.key);
                b.broker.revoke_principal(&c.node);
                self.lb = 1;
            }
            "UnrevokeB" => {
                let (b, c) = (&self.macs[B], &self.macs[C]);
                b.broker.unrevoke_key(&c.key);
                b.broker.unrevoke_principal(&c.node);
                self.lb = 2;
            }
            "PostA" => {
                self.macs[A].post(self.node(B), 2, b"v=1 t=2 note from A");
            }
            "PostC" => {
                self.macs[C].post(self.node(B), 2, b"v=1 t=2 note from C");
            }
            "PushA" => self.push(A),
            "PushC" => self.push(C),
            "Replay1" => {
                self.replay(A);
                self.r1 = 1;
            }
            "Replay3" => {
                self.replay(C);
                self.r3 = 1;
            }
            "Forge" => {
                let (b, c) = (&self.macs[B], &self.macs[C]);
                let ring = peer_ring(FLEET, &b.node, &c.node, &BTreeSet::new());
                if let Ok(mut client) = try_client(b, c.key, &ring) {
                    let _refused = client.publish(
                        pid(&c.node),
                        9,
                        &lane(&b.node, self.node(A)),
                        b"v=1 t=9 forged",
                    );
                }
                self.ft = 1;
            }
            other => panic!("FabricRouting has no action {other} the code takes"),
        }
        if self.lb == 1 && self.m3b() > m3b {
            self.bad = 1;
        }
    }

    fn m3b(&self) -> i64 {
        let b = &self.macs[B];
        b.holds(&lane(&b.node, self.node(C))).len() as i64
    }

    /// The model's variables, read off the brokers and what this run played.
    fn project(&self) -> ModelState {
        let (a, b, c) = (&self.macs[A], &self.macs[B], &self.macs[C]);
        let m1_at_b = b.holds(&lane(&b.node, &a.node));
        let count = |n: usize| n as i64;
        state(&[
            ("sp", self.sp),
            ("cs", self.cs),
            ("lb", self.lb),
            ("vb", i64::from(b.broker.is_identity_revoked(&c.key))),
            ("p1", count(a.holds(&lane(&b.node, &a.node)).len())),
            (
                "m1b",
                count(
                    m1_at_b
                        .iter()
                        .filter(|(.., origin)| origin.is_some_and(|(p, _)| p == pid(&a.node)))
                        .count(),
                ),
            ),
            ("m1c", count(c.holds(&lane(&b.node, &a.node)).len())),
            ("p3", count(c.holds(&lane(&b.node, &c.node)).len())),
            ("m3b", self.m3b()),
            ("r1", self.r1),
            ("r3", self.r3),
            ("ft", self.ft),
            (
                "fg",
                count(
                    m1_at_b
                        .iter()
                        .filter(|(.., origin)| !origin.is_some_and(|(p, _)| p == pid(&a.node)))
                        .count(),
                ),
            ),
            ("bad", self.bad),
            ("eb", count(b.holds(crate::broker_rules::EV_FILTER).len())),
            ("kb", 0),
            ("lost", 0),
        ])
    }

    /// A fresh world run down `path`, with the state projected after each step.
    fn replayed(path: &[&str], wide: bool) -> (Self, Vec<ModelState>) {
        let mut run = Self::new(wide);
        let seen = path
            .iter()
            .map(|action| {
                run.apply(action);
                routing_projection(&run)
            })
            .collect();
        (run, seen)
    }
}

/// **THE MAIL ROUTE REFINES `FabricRouting`**: in every state the model reaches, every action it
/// enables takes the brokers where the model goes. Mail goes one hop, to the Mac its subject names
/// alone; a re-sent frame lands once; B takes from C only what C's proven link may write, nothing
/// while its read has C removed, and everything again once C is seated again; A's session log
/// stays on A; and the mail is appended once a push runs within one island.
/// NEGATIVE CONTROL: [`FORWARD_ALL`] (`forward-all`), each peer serving the mail grant of the ring
/// before C3: the run leaves the model at `PushA`, and M1 lands on C, breaking
/// `MailOnlyToItsAddressee`. With today's ring the same control leaves C holding nothing: the
/// ring is the second wall, and the push to C is refused.
#[test]
fn the_mail_route_conforms_to_the_fabric_routing_model() {
    let model = aterm_models_swarm::fabric_routing_model();
    checked(
        &model,
        &aterm_models_swarm::mail_arrives_liveness(),
        &[
            (1, "MailOnlyToItsAddressee"),
            (1, "GroupPassesOnlyCarried"),
            (2, "AtMostOnce"),
            (3, "SenderIsTheProvenPeer"),
            (4, "NothingFromARemovedNode"),
            (6, "NoCurrentSeatRevoked"),
            (8, "EvNeverLeaves"),
        ],
    );
    let order = paths(&model);
    assert_eq!(
        order.len(),
        aterm_spec::interp::reachable_states(&model).len(),
        "the walk reaches every state"
    );
    let jobs: Vec<(&ModelState, &[&'static str], &'static str)> = order
        .iter()
        .flat_map(|(state, path)| {
            model
                .actions
                .iter()
                .filter(|action| model.action_enabled(action.name, state))
                .map(move |action| (state, path.as_slice(), action.name))
        })
        .collect();
    let next = std::sync::atomic::AtomicUsize::new(0);
    let workers = std::thread::available_parallelism()
        .map_or(4, std::num::NonZeroUsize::get)
        .min(8);
    let judged: usize = std::thread::scope(|s| {
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                s.spawn(|| {
                    let mut judged = 0;
                    while let Some((state, path, action)) =
                        jobs.get(next.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
                    {
                        let mut steps = path.to_vec();
                        steps.push(action);
                        let (_, seen) = Routing::replayed(&steps, false);
                        assert_eq!(
                            seen.last(),
                            Some(&successor(&model, action, state)),
                            "FabricRouting: {action} from {state:?} (down {path:?})"
                        );
                        judged += 1;
                    }
                    judged
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).sum()
    });
    eprintln!(
        "FabricRouting: {} states, {judged} transitions judged",
        order.len()
    );

    let path = ["PostA", "PushA"];
    FORWARD_ALL.with(|on| on.set(true));
    let (_, wide) = Routing::replayed(&path, true);
    let (_, narrow) = Routing::replayed(&path, false);
    FORWARD_ALL.with(|on| on.set(false));
    assert_eq!(diverges(&model, &path, &wide), "PushA");
    let last = wide.last().unwrap();
    assert_eq!(last["m1c"], 1, "A's note to B lands on C");
    assert!(!model.check_invariant("MailOnlyToItsAddressee", last));
    assert_eq!(
        narrow,
        walked(&model, &path),
        "under today's ring the push to C is refused"
    );
}

// ---- Halts --------------------------------------------------------------------------------------

/// The halt ids: H1 from A's person, H2 from C's.
fn h(tag: u8) -> String {
    format!("{tag:02x}").repeat(crate::halt::ID_HEX / 2)
}

const T0: u64 = 1_793_836_800_000;
const DAY_MS: u64 = 24 * 60 * 60 * 1000;

/// One record on a Mac's halt subject, as a log holds it.
#[derive(Clone, Debug)]
struct Rec {
    origin: String,
    seq: u64,
    said: Said,
    /// When this log appended it.
    at: u64,
}

/// One Mac's bridge, as far as halts go.
#[derive(Clone, Debug)]
struct HaltMac {
    node: String,
    person: String,
    halts: Halts,
    said_at: BTreeMap<String, u64>,
    answered: BTreeMap<String, u64>,
    gone: BTreeSet<String>,
    log: Vec<Rec>,
    applied: BTreeSet<(String, u64)>,
    seq: u64,
}

/// One run of `FabricHalts`' world.
#[derive(Clone, Debug)]
struct HaltWorld {
    m: [HaltMac; 3],
    now: u64,
    sp: i64,
    cs: i64,
    expired_at: Option<u64>,
    /// The ids a Mac said a halt-off for, which outlives every log.
    offs: BTreeSet<String>,
    ra: i64,
    rc: i64,
}

impl HaltWorld {
    fn new() -> Self {
        let mac = |tag: u8, person: &str| HaltMac {
            node: node_of(&super::tests::key_of(tag)),
            person: person.to_owned(),
            halts: Halts::default(),
            said_at: BTreeMap::new(),
            answered: BTreeMap::new(),
            gone: BTreeSet::new(),
            log: Vec::new(),
            applied: BTreeSet::new(),
            seq: 0,
        };
        Self {
            m: [mac(1, "h-a"), mac(2, "h-b"), mac(3, "h-c")],
            now: T0,
            sp: 0,
            cs: 0,
            expired_at: None,
            offs: BTreeSet::new(),
            ra: 0,
            rc: 0,
        }
    }

    /// Mac `x`'s bridge says `said` on its halt subject and applies it, as `say_halt` and
    /// `apply_said` do; A and B hear each other in the same step.
    fn say(&mut self, x: usize, said: Said) {
        let now = self.now;
        if said.word == Word::Off {
            self.offs.insert(said.id.clone());
        }
        let mac = &mut self.m[x];
        mac.seq += 1;
        let rec = Rec {
            origin: mac.node.clone(),
            seq: mac.seq,
            said,
            at: now,
        };
        mac.applied.insert((rec.origin.clone(), rec.seq));
        mac.log.push(rec.clone());
        let me = mac.node.clone();
        let outcome = mac.halts.apply(&me, &me, &rec.said, &mac.gone, now);
        assert!(
            !matches!(outcome, Outcome::Answer { .. }),
            "a Mac answers itself"
        );
        if x != C {
            self.hear(1 - x, &rec);
        }
    }

    /// Mac `y` hears `rec`, once: its log keeps it and its bridge applies it, answering an origin
    /// that said a halt again, or an ask, after a halt-off this Mac applied for every Mac.
    fn hear(&mut self, y: usize, rec: &Rec) {
        let now = self.now;
        let mac = &mut self.m[y];
        if !mac.applied.insert((rec.origin.clone(), rec.seq)) {
            return;
        }
        mac.log.push(Rec {
            at: now,
            ..rec.clone()
        });
        let me = mac.node.clone();
        if let Outcome::Answer { by } = mac.halts.apply(&me, &rec.origin, &rec.said, &mac.gone, now)
        {
            self.answer(y, &rec.said.id, &by);
        }
    }

    /// `answer_halt`: the halt-off said back, at most once a `RESAY_MS` per id.
    fn answer(&mut self, y: usize, id: &str, by: &str) {
        let now = self.now;
        if self.m[y]
            .answered
            .get(id)
            .is_some_and(|at| now < at.saturating_add(crate::halt::RESAY_MS))
        {
            return;
        }
        self.say(
            y,
            Said {
                id: id.to_owned(),
                by: by.to_owned(),
                word: Word::Off,
                t_ms: now,
            },
        );
        let mac = &mut self.m[y];
        mac.answered.insert(id.to_owned(), now);
        let known = mac.halts.clone();
        mac.answered.retain(|id, _| known.knows(id));
    }

    /// Mac `y` hears every record the logs of `from` hold, in their order.
    fn hear_from(&mut self, y: usize, from: &[usize]) {
        let recs: Vec<Rec> = from.iter().flat_map(|x| self.m[*x].log.clone()).collect();
        for rec in &recs {
            self.hear(y, rec);
        }
    }

    /// A person on Mac `x` asks `word` of halt `id`: said, and the time kept, as `said_ask` does.
    fn person(&mut self, x: usize, id: &str, word: Word) {
        let said = Said {
            id: id.to_owned(),
            by: self.m[x].person.clone(),
            word,
            t_ms: self.now,
        };
        self.m[x].said_at.insert(id.to_owned(), self.now);
        self.say(x, said);
    }

    /// `resay_due_halts`.
    fn resay(&mut self, x: usize) {
        let now = self.now;
        let mac = &mut self.m[x];
        let known = mac.halts.clone();
        mac.said_at.retain(|id, _| known.knows(id));
        let due = mac.halts.due(&mac.node, &mac.said_at, now);
        for said in due {
            let id = said.id.clone();
            self.say(x, said);
            self.m[x].said_at.insert(id, now);
        }
    }

    fn apply(&mut self, action: &str) {
        match action {
            "Split" => self.sp = 1,
            "Join" => self.sp = 2,
            "Remove" => self.cs = 1,
            "HaltA" => self.person(A, &h(1), Word::On("stop".into())),
            "HaltC" => self.person(C, &h(2), Word::On("stop".into())),
            "OffB1" => self.person(B, &h(1), Word::Off),
            "OffA2" => self.person(A, &h(2), Word::Off),
            "Revoke" => {
                let gone = self.m[C].node.clone();
                let now = self.now;
                for x in [A, B] {
                    let mac = &mut self.m[x];
                    mac.gone.insert(gone.clone());
                    let me = mac.node.clone();
                    mac.halts.lift_gone(&me, &mac.gone, now);
                }
            }
            "HearC" => self.hear_from(C, &[A, B]),
            "HearAB" => {
                self.hear_from(A, &[C]);
                self.hear_from(B, &[C]);
            }
            "Expire" => {
                self.now += crate::halt::OFF_RESAY_SPAN_MS + DAY_MS;
                self.expired_at = Some(self.now);
                let max_age = crate::broker_rules::MAX_AGE.as_millis() as u64;
                let now = self.now;
                for mac in &mut self.m {
                    mac.log.retain(|rec| rec.at.saturating_add(max_age) >= now);
                }
            }
            "ResayAB" => {
                self.resay(A);
                self.resay(B);
                self.ra = 1;
            }
            "ResayC" => {
                self.resay(C);
                self.rc = 1;
            }
            other => panic!("FabricHalts has no action {other} the code takes"),
        }
    }

    /// Mac `x`'s view of halt `id`, and whether it holds it, or held it when it lifted it, under a
    /// saying from after the span.
    fn view(&self, x: usize, id: &str) -> (i64, i64) {
        let halts = &self.m[x].halts;
        let fresh = |t_ms: u64| i64::from(self.expired_at.is_some_and(|at| t_ms >= at));
        if let Some(halt) = halts.standing().get(id) {
            return (1, fresh(halt.t_ms));
        }
        match halts.lifted().get(id) {
            None => (0, 0),
            Some(lifted) => {
                let was = lifted.was.as_ref().map_or(0, |was| fresh(was.t_ms));
                match lifted.how {
                    Lift::Fleet { .. } => (2, was),
                    Lift::Gone => (3, was),
                    Lift::Here { .. } => panic!("FabricHalts has no lift for one Mac"),
                }
            }
        }
    }

    /// Whether Mac `x`'s log holds a record of `id` saying `word`, from after the span when
    /// `fresh`.
    fn logged(&self, x: usize, id: &str, word: &dyn Fn(&Word) -> bool, fresh: bool) -> bool {
        self.m[x].log.iter().any(|rec| {
            rec.said.id == id
                && word(&rec.said.word)
                && (!fresh || self.expired_at.is_some_and(|at| rec.said.t_ms >= at))
        })
    }

    fn project(&self) -> ModelState {
        let ab = |id: &str| {
            let (a, b) = (self.view(A, id), self.view(B, id));
            assert_eq!(a, b, "A and B hold one view of {id}");
            a
        };
        let off = |id: &str| -> i64 {
            if self.logged(A, id, &|w| *w == Word::Off, false) {
                1
            } else {
                i64::from(self.offs.contains(id)) * 2
            }
        };
        let on = |w: &Word| matches!(w, Word::On(_));
        let (h1, _) = ab(&h(1));
        let (h2, h2f) = ab(&h(2));
        let (c1, c1f) = self.view(C, &h(1));
        let (c2, _) = self.view(C, &h(2));
        state(&[
            ("sp", self.sp),
            ("cs", self.cs),
            ("g", i64::from(self.m[A].gone.contains(&self.m[C].node))),
            ("h1", h1),
            ("c1", c1),
            ("c1f", c1f),
            ("h2", h2),
            ("h2f", h2f),
            ("c2", c2),
            ("o1", off(&h(1))),
            ("o2", off(&h(2))),
            ("ex", i64::from(self.expired_at.is_some())),
            ("f1", i64::from(self.logged(A, &h(1), &on, true))),
            ("f2", i64::from(self.logged(C, &h(2), &on, true))),
            (
                "q",
                i64::from(self.logged(C, &h(1), &|w| *w == Word::Ask, false)),
            ),
            (
                "qab",
                i64::from(self.logged(A, &h(2), &|w| *w == Word::Ask, false)),
            ),
            ("ra", self.ra),
            ("rc", self.rc),
        ])
    }

    fn replayed(path: &[&str]) -> Vec<ModelState> {
        let mut run = Self::new();
        path.iter()
            .map(|action| {
                run.apply(action);
                halts_projection(&run)
            })
            .collect()
    }
}

/// **THE HALTS REFINE `FabricHalts`**: in every state the model reaches, every action it enables
/// takes each Mac's halts where the model goes. A halt-off lifts the id it names on every Mac;
/// the removal of a halt's origin lifts its halts where it is applied, and nothing the origin
/// says after applies; and a Mac away past the span, after every log gave the halt-off up, asks
/// for the halt it still holds, is answered by a Mac that lifted it, and lifts it.
/// NEGATIVE CONTROLS, each leaving the model at the step named and breaking its law:
/// [`crate::halt::OFF_LIFTS_EVERY`] at `OffB1` (`HaltOffLiftsOnlyItsHalt`),
/// [`crate::halt::NO_LIFT_ON_GONE`] at `Revoke` (`NoHaltOutlivesItsOrigin`), and
/// [`crate::halt::NO_STALE_ASK`], the path before C7, at `ResayC` (`StaleHaltAsks`): C never asks,
/// and holds the halt every other Mac lifted.
#[test]
fn the_halts_conform_to_the_fabric_halts_model() {
    let model = aterm_models_swarm::fabric_halts_model();
    checked(
        &model,
        &aterm_models_swarm::halt_lifts_everywhere_liveness(),
        &[
            (1, "StaleHaltAsks"),
            (5, "HaltOffLiftsOnlyItsHalt"),
            (7, "NoHaltOutlivesItsOrigin"),
        ],
    );
    let init = model.init_state();
    assert_eq!(halts_projection(&HaltWorld::new()), init, "the start");
    let mut order = vec![init.clone()];
    let mut realized: BTreeMap<ModelState, HaltWorld> = [(init, HaltWorld::new())].into();
    let (mut judged, mut next) = (0usize, 0);
    while let Some(state) = order.get(next).cloned() {
        next += 1;
        for action in &model.actions {
            if !model.action_enabled(action.name, &state) {
                continue;
            }
            let mut run = realized[&state].clone();
            run.apply(action.name);
            let observed = halts_projection(&run);
            assert_eq!(
                observed,
                successor(&model, action.name, &state),
                "FabricHalts: {} from {state:?}",
                action.name
            );
            judged += 1;
            if let std::collections::btree_map::Entry::Vacant(slot) =
                realized.entry(observed.clone())
            {
                slot.insert(run);
                order.push(observed);
            }
        }
    }
    assert_eq!(
        order.len(),
        aterm_spec::interp::reachable_states(&model).len()
    );
    eprintln!(
        "FabricHalts: {} states, {judged} transitions judged",
        order.len()
    );

    let control = |cell: &'static std::thread::LocalKey<std::cell::Cell<bool>>,
                   path: &[&'static str],
                   at: &str,
                   law: &str| {
        cell.with(|on| on.set(true));
        let seen = HaltWorld::replayed(path);
        cell.with(|on| on.set(false));
        assert_eq!(diverges(&model, path, &seen), at, "{law}");
        let last = seen.last().unwrap();
        assert!(!model.check_invariant(law, last), "{law} at {last:?}");
        seen
    };
    control(
        &crate::halt::OFF_LIFTS_EVERY,
        &["HaltA", "HaltC", "HearAB", "OffB1"],
        "OffB1",
        "HaltOffLiftsOnlyItsHalt",
    );
    control(
        &crate::halt::NO_LIFT_ON_GONE,
        &["HaltC", "HearAB", "Remove", "Revoke"],
        "Revoke",
        "NoHaltOutlivesItsOrigin",
    );
    let away = [
        "HaltA", "HearC", "Split", "OffB1", "Expire", "Join", "ResayC",
    ];
    control(&crate::halt::NO_STALE_ASK, &away, "ResayC", "StaleHaltAsks");
    // The rest of the story: with the ask, C hears the answer and lifts; without it, nothing
    // reaches C however long it listens.
    let mut heard = away.to_vec();
    heard.extend(["HearAB", "HearC"]);
    assert_eq!(
        HaltWorld::replayed(&heard).last().unwrap()["c1"],
        2,
        "C lifts H1"
    );
    crate::halt::NO_STALE_ASK.with(|on| on.set(true));
    let stuck = HaltWorld::replayed(&heard);
    crate::halt::NO_STALE_ASK.with(|on| on.set(false));
    let last = stuck.last().unwrap();
    assert_eq!(
        (last["h1"], last["c1"]),
        (2, 1),
        "C holds what every other Mac lifted"
    );
}

/// The projection `FabricRouting`'s anchors name: the model's variables read off a run.
fn routing_projection(run: &Routing) -> ModelState {
    run.project()
}

/// The projection `FabricHalts`' anchors name: the model's variables read off a run.
fn halts_projection(world: &HaltWorld) -> ModelState {
    world.project()
}
