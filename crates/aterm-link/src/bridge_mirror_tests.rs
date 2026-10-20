// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// TIER-1: the bus mirror's bridge half against the derived `LogMirror`
// model (`aterm_models_session::log_mirror_model`, Tier-0 in aterm-models'
// `derived_log_mirror_proves_and_catches_each_law`).
//
// The REAL bridge — its take, pin, pipelined send at pinned sequences,
// checked dedup, collision re-pin and landing ([`super::super::mirror_run`]) —
// over a fake endpoint that answers with REAL session logs (`aterm_slog::Log`:
// their bus class, `ms`, take, fence and landing, the carry an update makes)
// and a REAL broker (its producer-sequence dedup). After every step the world
// is projected onto the model's variables — the ring's live `ms`, the log's
// cursor, the bridge's pin, and the READER'S VIEW of the broker's `ev` face,
// decoded and deduped by `(ep, seq)` as every reader does (ruling Q0) — and
// must be the model's state after the matching actions (a crash and a
// refused landing are stutters). NEGATIVE CONTROLS: for each mutant, the
// real system driven through the deliberately broken call the mutant stands
// for, and the projected law breaks (ruling Q12).

use super::*;
use aterm_slog::{CarryAdopt, CarryBudget, Conf, Kind, Limits, Log, Mark, Src, Stamp};
use aterm_spec::interp::State;

use super::super::mirror_run::{MirrorRun, Outcome};

const SID: &str = "s-a";

/// The ring: eight records, so an append past it evicts exactly the oldest
/// one it can (the low water is an eighth under).
fn limits() -> Limits {
    Limits::new(8, 1 << 20).expect("a small ring")
}

/// Fields drop in this order: the bridge, then the broker, then the lane and
/// the scratch dir that holds the broker's socket. A broker whose socket was
/// removed under it cannot dial its own acceptor awake, and its shutdown waits
/// out astream's whole wake bound (a second) instead.
struct World {
    bridge: Bridge,
    _server: astream_broker::BrokerHandle,
    lane: FakeVerbLane,
    model: aterm_spec::derive::Model,
    expect: State,
    clock: u64,
    fh: i64,
    lc: i64,
    succ: bool,
    adopted: bool,
    foreign: bool,
    carry: Option<Vec<String>>,
    /// The successor's log before it adopts the session.
    pending: Option<Log>,
}

impl World {
    /// A session whose log holds its open (`ms` 1, the model's first
    /// `Append`), mirrored one slot a batch.
    fn new() -> World {
        let (lane, mut bridge, server) = rig_with_broker(&[]);
        bridge.mirror = Some(MirrorRun::acked_for_tests(&bridge.state, 1));
        let mut log = Log::with_limits(1, limits());
        log.open_session(
            Kind::Spawn,
            Src::Aterm,
            Conf::Exact,
            aterm_slog::Actor::Aterm,
            aterm_slog::Fields::new(),
            Stamp::new(1, 1),
        )
        .expect("the session opens");
        lane.mirror
            .lock()
            .unwrap()
            .logs
            .insert(SID.to_string(), log);
        let model = aterm_models_session::log_mirror_model();
        let expect = model.init_state();
        let mut w = World {
            bridge,
            _server: server,
            lane,
            model,
            expect,
            clock: 10,
            fh: 0,
            lc: 0,
            succ: false,
            adopted: true,
            foreign: false,
            carry: None,
            pending: None,
        };
        w.fire(&["Append"]);
        w.check("the session's open");
        w
    }

    fn at(&mut self) -> Stamp {
        self.clock += 1;
        Stamp::new(self.clock, self.clock)
    }

    fn log<R>(&mut self, f: impl FnOnce(&mut Log) -> R) -> R {
        if let Some(pending) = self.pending.as_mut() {
            return f(pending);
        }
        let mut m = self.lane.mirror.lock().unwrap();
        f(m.logs.get_mut(SID).expect("the session's log"))
    }

    fn fire(&mut self, actions: &[&str]) {
        for a in actions {
            assert!(
                self.model.fire(a, &mut self.expect),
                "the model does not admit {a} at {:?}",
                self.expect
            );
        }
    }

    /// The reader's view of the session's `ev` face: per `ms`, the distinct
    /// `(ep, seq)` records holding it, and whether a cover covers it.
    fn view(&mut self) -> ([i64; 3], [i64; 3]) {
        let face = subject::session_face("f", &self.bridge.node, SID, "ev");
        let (mut conn, _) = self.bridge.connect().expect("a read connection");
        let (rows, _) = conn.fetch(0, &face, 1000).expect("the face");
        let mut recs: BTreeMap<u64, BTreeSet<(u32, u64)>> = BTreeMap::new();
        let mut cov = [0i64; 3];
        for (_, _, bytes) in rows {
            let Ok(b) = aterm_slog::decode_bus(&String::from_utf8_lossy(&bytes)) else {
                continue;
            };
            let ms = b.ms.unwrap_or(0);
            match b.rec.f.get_u(aterm_slog::Key::N) {
                Some(n) if b.rec.op == aterm_slog::Op::Gap => {
                    for i in ms..ms + n {
                        if (1..=3).contains(&i) {
                            cov[usize::try_from(i - 1).unwrap()] = 1;
                        }
                    }
                }
                _ if (1..=3).contains(&ms) => {
                    recs.entry(ms).or_default().insert((b.ep, b.rec.seq));
                }
                _ => {}
            }
        }
        let rec = [1u64, 2, 3].map(|i| recs.get(&i).map_or(0, |s| s.len() as i64));
        (rec, cov)
    }

    /// The world, as the model's variables.
    fn project(&mut self) -> State {
        let (head, live, landed, fence) = self.log(|log| {
            (
                i64::try_from(log.ms_head()).unwrap(),
                log.live_ms().collect::<BTreeSet<u64>>(),
                log.mirror_landed()
                    .map_or(0, |c| i64::try_from(c.ms).unwrap()),
                i64::from(log.read_fence().is_some()),
            )
        });
        let evicted =
            |ms: u64| i64::from(i64::try_from(ms).unwrap() <= head && !live.contains(&ms));
        // A snapshot is a seed, no `ms`: the model's pin is the batch's
        // first record or cover.
        let first = self
            .bridge
            .mirror
            .as_deref()
            .map(MirrorRun::batch)
            .and_then(|(slots, outcomes)| {
                slots
                    .iter()
                    .zip(outcomes)
                    .find(|(s, _)| !s.body.contains(" op=snap "))
                    .map(|(s, o)| (s.body.clone(), *o))
            });
        let (pin, plo, phi, pgap, onbus) = match first {
            Some((body, outcome)) => {
                let b = aterm_slog::decode_bus(&body).expect("a pinned body");
                let lo = i64::try_from(b.ms.unwrap_or(0)).unwrap();
                let (hi, gap) = match b.rec.f.get_u(aterm_slog::Key::N) {
                    Some(n) => (lo + i64::try_from(n).unwrap() - 1, 1),
                    None => (lo, 0),
                };
                let on = matches!(
                    outcome,
                    Outcome::OnBus(_) | Outcome::Deduped(_) | Outcome::Verified(_)
                );
                (1, lo, hi, gap, i64::from(on))
            }
            None => (
                0,
                self.expect["plo"],
                self.expect["phi"],
                self.expect["pgap"],
                0,
            ),
        };
        let (rec, cov) = self.view();
        let mut s = State::new();
        for (k, v) in [
            ("head", head),
            ("e2", evicted(2)),
            ("e3", evicted(3)),
            ("landed", landed),
            ("pin", pin),
            ("plo", plo),
            ("phi", phi),
            ("pgap", pgap),
            ("onbus", onbus),
            ("foreign", i64::from(self.foreign)),
            ("rec1", rec[0]),
            ("rec2", rec[1]),
            ("rec3", rec[2]),
            ("cov1", cov[0]),
            ("cov2", cov[1]),
            ("cov3", cov[2]),
            ("fence", fence),
            ("fh", self.fh),
            ("lc", self.lc),
            ("succ", i64::from(self.succ)),
            ("adopted", i64::from(self.adopted)),
            // The bridge's world never closes its session: the GUI's Tier-1
            // binds `Close` (`session_log_mirror_tests`).
            ("closed", 0),
        ] {
            s.insert(k, v);
        }
        s
    }

    /// The projected world is the model's state, and every law holds of it.
    fn check(&mut self, what: &str) {
        let got = self.project();
        assert_eq!(
            got, self.expect,
            "{what}: the real world is not the model's"
        );
        for law in [
            "ReaderViewAtMostOnce",
            "MsDense",
            "QuiescentMirrorComplete",
            "NoLandUnderFence",
        ] {
            assert!(self.model.check_invariant(law, &got), "{what}: {law}");
        }
    }

    /// The projected world breaks `law` (a negative control).
    fn breaks(&mut self, law: &str, what: &str) {
        let got = self.project();
        assert!(
            !self.model.check_invariant(law, &got),
            "{what}: the broken call should break {law}: {got:?}"
        );
    }

    // ---- the real steps ------------------------------------------------

    /// A bus-class record: `mark - seen`.
    fn append(&mut self) {
        let at = self.at();
        self.log(|log| {
            log.mark(Mark::new(Kind::Seen, Src::Status, Conf::Strong), at)
                .expect("a seen mark");
        });
    }

    /// A local record (a bell): the ring's, never the bus's.
    fn bell(&mut self) {
        let at = self.at();
        self.log(|log| {
            log.mark(Mark::new(Kind::Bell, Src::Engine, Conf::Exact), at)
                .expect("a bell");
        });
    }

    /// Local records until the ring evicts `ms`. An eviction batch takes an
    /// eighth of the ring, oldest first: a bell between `ms` and the next
    /// bus-class record is the batch's second victim, so `ms` goes alone.
    fn evict(&mut self, ms: u64) {
        for _ in 0..64 {
            if !self.log(|log| log.live_ms().any(|m| m == ms)) {
                return;
            }
            let at = self.at();
            self.log(|log| {
                log.mark(Mark::new(Kind::Bell, Src::Engine, Conf::Exact), at)
                    .expect("a bell");
            });
        }
        panic!("the ring never evicted ms {ms}");
    }

    fn take(&mut self) {
        self.bridge.mirror_take();
    }

    fn publish(&mut self) {
        self.bridge.mirror_send();
        self.bridge.settle_acks();
    }

    fn settle(&mut self) {
        self.bridge.mirror_settle();
    }

    fn export(&mut self) {
        let (carry, fh, lc) = self.log(|log| {
            let carry = log.carry(CarryBudget::SESSION, &[]).expect("carries").lines;
            log.fence_reads();
            (
                carry,
                i64::try_from(log.ms_head()).unwrap(),
                log.mirror_landed()
                    .map_or(0, |c| i64::try_from(c.ms).unwrap()),
            )
        });
        self.carry = Some(carry);
        self.fh = fh;
        self.lc = lc;
    }

    /// The successor adopts the carry; the endpoint does not hold the session
    /// until [`World::adopt_late`].
    fn commit(&mut self) {
        let carry = self.carry.take().expect("an export");
        let old = self
            .lane
            .mirror
            .lock()
            .unwrap()
            .logs
            .remove(SID)
            .expect("the log");
        let at = self.at();
        let (next, _, _) = Log::from_carry(
            &carry,
            CarryAdopt {
                ep: old.ep() + 1,
                next_seq: old.last_seq() + 1000,
                ids: old.id_counters(),
                limits: limits(),
                at,
            },
        )
        .expect("the successor reads the carry");
        self.pending = Some(next);
        self.lane.mirror.lock().unwrap().adopting = true;
        self.succ = true;
        self.adopted = false;
    }

    fn adopt_late(&mut self) {
        let next = self.pending.take().expect("a successor");
        let mut m = self.lane.mirror.lock().unwrap();
        m.logs.insert(SID.to_string(), next);
        m.adopting = false;
        drop(m);
        self.adopted = true;
    }

    fn rollback(&mut self) {
        self.log(Log::lift_read_fence);
    }

    /// Another writer's record at the pinned sequence (ruling Q3).
    fn plant_foreign(&mut self) {
        let pseq = self
            .bridge
            .mirror
            .as_deref()
            .map(|m| m.batch().0[0].pseq)
            .expect("a pin");
        let face = subject::session_face("f", &self.bridge.node, SID, "ev");
        let (mut god, _) = self.bridge.connect().expect("a connection");
        god.publish(self.bridge.producer_id, pseq, &face, b"v=1 t=1 ev=foreign")
            .expect("the foreign record lands");
        self.foreign = true;
    }

    /// The reattach a collision drops the connection for, and the re-pin.
    fn repin(&mut self) {
        let (conn, _) = self.bridge.connect().expect("a reattach");
        self.bridge.conn = Some(conn.into());
        self.bridge.mirror_repin();
        self.foreign = false;
    }
}

/// A HOLE IS SAID ONCE: three records, the first landed, the second evicted
/// before a take reached it — one cover, then the third record.
#[test]
fn log_mirror_conforms_to_derived_model() {
    let mut w = World::new();
    w.append();
    w.bell();
    w.append();
    w.fire(&["Append", "Append"]);
    w.check("three records");
    w.take();
    w.fire(&["TakeRecord"]);
    w.check("ms 1 pinned");
    w.publish();
    w.fire(&["Publish"]);
    w.check("ms 1 on the bus");
    w.settle();
    w.fire(&["Landed", "Unpin"]);
    w.check("ms 1 landed");
    w.evict(2);
    w.fire(&["Evict2"]);
    w.check("ms 2 evicted, ms 3 live");
    w.take();
    w.fire(&["TakeGap"]);
    w.check("a cover of ms 2 alone");
    w.publish();
    w.settle();
    w.fire(&["Publish", "Landed", "Unpin"]);
    w.check("the cover landed");
    w.take();
    w.publish();
    w.settle();
    w.fire(&["TakeRecord", "Publish", "Landed", "Unpin"]);
    w.check("ms 3 landed: quiescent and complete");
    assert_eq!(
        (w.expect["rec1"], w.expect["cov2"], w.expect["rec3"]),
        (1, 1, 1)
    );
}

/// RULINGS Q1 AND Q12 THROUGH AN UPDATE: a slot pinned and published under
/// the export's fence is refused its landing (`fenced`), the successor that
/// has not adopted the session answers `adopting` and the bridge keeps its
/// pin, and once adopted the pin lands on the carried cursor — past the
/// record the successor's ring has since evicted, which is never covered.
#[test]
fn log_mirror_conforms_across_an_update_adopted_late() {
    let mut w = World::new();
    w.append();
    w.bell();
    w.fire(&["Append"]);
    w.take();
    w.publish();
    w.settle();
    w.fire(&["TakeRecord", "Publish", "Landed", "Unpin"]);
    w.check("ms 1 landed");
    w.export();
    w.fire(&["Export"]);
    w.check("exported");
    w.take();
    w.publish();
    w.fire(&["TakeRecord", "Publish"]);
    w.check("ms 2 on the bus under the fence");
    w.settle();
    w.check("fenced: the pin stays (a stutter)");
    w.commit();
    // The successor's own `mark - handoff` is its first record: an append.
    w.fire(&["Commit", "Append"]);
    w.check("committed, not adopted");
    w.settle();
    w.check("adopting: the pin stays (a stutter)");
    w.evict(2);
    w.fire(&["Evict2"]);
    w.check("the successor's ring evicted ms 2");
    w.adopt_late();
    w.fire(&["AdoptLate"]);
    w.settle();
    w.fire(&["Landed", "Unpin"]);
    w.check("landed on the carried cursor, past the evicted record");
    w.take();
    w.publish();
    w.settle();
    w.fire(&["TakeRecord", "Publish", "Landed", "Unpin"]);
    w.check("the handoff mark landed: quiescent and complete");
    assert_eq!(
        (w.expect["rec2"], w.expect["cov2"]),
        (1, 0),
        "never covered"
    );
    // A rollback lifts the fence and the landing goes on in the same process.
    let mut r = World::new();
    r.append();
    r.fire(&["Append"]);
    r.export();
    r.take();
    r.publish();
    r.fire(&["Export", "TakeRecord", "Publish"]);
    r.settle();
    r.check("fenced");
    r.rollback();
    r.fire(&["Rollback"]);
    r.settle();
    r.fire(&["Landed", "Unpin"]);
    r.check("stood down: landed");
}

/// RULING Q3: another writer's record at the pinned sequence is a collision
/// (the bridge drops its connection and keeps the evidence), never a
/// landing; the slot is re-pinned at a fresh sequence and lands.
#[test]
fn log_mirror_conforms_when_a_foreign_record_holds_the_pinned_seq() {
    let mut w = World::new();
    w.take();
    w.fire(&["TakeRecord"]);
    w.plant_foreign();
    w.fire(&["Foreign"]);
    w.check("a foreign record at the pinned seq");
    w.publish();
    w.check("the dedup is a collision: nothing on the bus (a stutter)");
    assert!(
        w.bridge.conn.is_none(),
        "the collision dropped the connection"
    );
    assert!(w.bridge.collided.is_some(), "and kept the evidence");
    w.repin();
    w.fire(&["Repin"]);
    w.check("re-pinned at a fresh seq");
    w.publish();
    w.settle();
    w.fire(&["Publish", "Landed", "Unpin"]);
    w.check("landed");
}

/// A BRIDGE KILLED WITH A PIN OUTSTANDING: the next bridge on the state dir
/// reads the pin, republishes at the pinned sequence, reads the deduped
/// answer back, and lands — the record on the bus once (§12 M6's count).
#[test]
fn log_mirror_conforms_across_a_bridge_crash_with_a_pin_outstanding() {
    let (lane, mut first, _server) = rig_with_broker(&[]);
    first.mirror = Some(MirrorRun::acked_for_tests(&first.state, 1));
    let mut log = Log::with_limits(1, limits());
    log.open_session(
        Kind::Spawn,
        Src::Aterm,
        Conf::Exact,
        aterm_slog::Actor::Aterm,
        aterm_slog::Fields::new(),
        Stamp::new(1, 1),
    )
    .expect("the session opens");
    lane.mirror
        .lock()
        .unwrap()
        .logs
        .insert(SID.to_string(), log);
    first.mirror_take();
    first.mirror_send();
    first.settle_acks();
    assert!(matches!(
        first.mirror.as_deref().unwrap().batch().1[0],
        Outcome::OnBus(_)
    ));
    let state = first.state.root().to_path_buf();
    let broker = first.cfg.broker.clone();
    let node = first.node.clone();
    // Killed: nothing of it survives but the state dir and the broker.
    drop(first);
    let (next_lane, mut next) = FakeVerbLane::bridge_with_state(&[], Some(&state));
    next.cfg.broker = broker;
    let (conn, _) = next.connect().unwrap();
    next.conn = Some(conn.into());
    assert_eq!(next.node, node);
    // Its endpoint is the same instance: the logs move over.
    let logs = std::mem::take(&mut lane.mirror.lock().unwrap().logs);
    next_lane.mirror.lock().unwrap().logs = logs;
    next.mirror = MirrorRun::new(crate::mirror::LogMode::Turns, &next.state);
    next.mirror_hello_answered("OK lane=2 build=b log=turns");
    let (slots, outcomes) = next.mirror.as_deref().map(MirrorRun::batch).unwrap();
    assert_eq!(
        (slots.len(), outcomes[0]),
        (1, Outcome::Owed),
        "the pin recovered"
    );
    next.mirror_send();
    next.settle_acks();
    assert!(matches!(
        next.mirror.as_deref().unwrap().batch().1[0],
        Outcome::Deduped(_)
    ));
    next.mirror_settle();
    assert!(
        next.mirror.as_deref().unwrap().batch().0.is_empty(),
        "landed, read back and emptied"
    );
    let landed = next_lane.mirror.lock().unwrap().logs[SID].mirror_landed();
    assert_eq!(landed.map(|c| c.ms), Some(1));
    let face = subject::session_face("f", &node, SID, "ev");
    let (mut conn, _) = next.connect().unwrap();
    let (rows, _) = conn.fetch(0, &face, 100).unwrap();
    assert_eq!(rows.len(), 1, "on the bus once: {rows:?}");
}

/// NEGATIVE CONTROLS (ruling Q12): each mutant's witness replayed against
/// the real system through the broken call it stands for, and the projected
/// law breaks.
#[test]
fn log_mirror_conformance_rejects_each_mutant_through_its_broken_call() {
    // DropUnadopted: the endpoint answers `gone` for a session a successor
    // has not adopted yet; the bridge drops its pin; the carried cursor is
    // behind it, and the successor covers what its ring evicted.
    let mut w = World::new();
    w.append();
    w.bell();
    w.take();
    w.publish();
    w.settle();
    w.export();
    w.take();
    w.publish();
    w.commit();
    w.lane.mirror.lock().unwrap().gone_while_adopting = true;
    w.settle();
    assert!(
        w.bridge.mirror.as_deref().unwrap().batch().0.is_empty(),
        "the pin dropped"
    );
    w.evict(2);
    w.adopt_late();
    w.take();
    w.publish();
    w.breaks("ReaderViewAtMostOnce", "DropUnadopted");

    // TailPastFence: a take that does not stop at the fence; the overlap's
    // records reach the bus, and the successor numbers those ms again.
    let mut t = World::new();
    t.take();
    t.publish();
    t.settle();
    t.export();
    t.append();
    t.append();
    t.log(Log::lift_read_fence);
    for _ in 0..2 {
        t.take();
        t.publish();
        t.settle();
    }
    t.log(Log::fence_reads);
    t.commit();
    t.adopt_late();
    for _ in 0..4 {
        t.take();
        t.publish();
        t.settle();
    }
    t.breaks("ReaderViewAtMostOnce", "TailPastFence");

    // SkipGap: a take whose cover is dropped before it is pinned.
    let mut g = World::new();
    g.append();
    g.bell();
    g.append();
    g.take();
    g.publish();
    g.settle();
    g.evict(2);
    // Two slots a take, so a record follows the cover the broken take drops.
    g.bridge.mirror = Some(MirrorRun::acked_for_tests(&g.bridge.state, 2));
    g.lane.mirror.lock().unwrap().skip_covers = true;
    g.take();
    g.publish();
    g.breaks("MsDense", "SkipGap");

    // AckBeforePublish: the slot landed before its ack, the pin emptied.
    let mut a = World::new();
    a.take();
    let at = a.bridge.mirror.as_deref().unwrap().batch().0[0].at;
    a.log(|log| log.land(at)).expect("lands");
    a.bridge
        .mirror
        .as_deref_mut()
        .unwrap()
        .drop_batch_for_tests();
    a.breaks("QuiescentMirrorComplete", "AckBeforePublish");

    // TrustDeduped: a foreign record's dedup taken as ours, landed.
    let mut d = World::new();
    d.take();
    d.plant_foreign();
    let at = d.bridge.mirror.as_deref().unwrap().batch().0[0].at;
    d.log(|log| log.land(at)).expect("lands");
    d.bridge
        .mirror
        .as_deref_mut()
        .unwrap()
        .drop_batch_for_tests();
    d.foreign = false;
    d.breaks("QuiescentMirrorComplete", "TrustDeduped");

    // LandUnderFence: a landing answered while the export's cursor stands.
    let mut f = World::new();
    f.append();
    f.export();
    f.take();
    f.publish();
    let at = f.bridge.mirror.as_deref().unwrap().batch().0[0].at;
    f.log(|log| {
        log.lift_read_fence();
        let r = log.land(at);
        log.fence_reads();
        r
    })
    .expect("lands");
    f.breaks("NoLandUnderFence", "LandUnderFence");
    // And the real landing refuses it.
    let mut real = World::new();
    real.append();
    real.export();
    real.take();
    real.publish();
    let at = real.bridge.mirror.as_deref().unwrap().batch().0[0].at;
    assert_eq!(
        real.log(|log| log.land(at)),
        Err(aterm_slog::LandRefused::Fenced),
        "CONTROL: the fence refuses the landing"
    );
}

/// RULING Q5 ON THE WIRE: a bridge with the mirror off sends the `hello`
/// it always did, and no `log-take` or `log-landed` ever.
#[test]
fn a_bridge_with_the_mirror_off_sends_no_mirror_byte() {
    let (lane, mut bridge) = FakeVerbLane::bridge(&[(1, SID)]);
    assert!(bridge.mirror.is_none());
    assert_eq!(bridge.mirror_hello_token(), "");
    bridge.introduce();
    let hellos = lane.hellos.lock().unwrap().clone();
    assert_eq!(hellos.len(), 1);
    assert!(
        hellos[0].ends_with(" refill=in,say") && !hellos[0].contains("log="),
        "{hellos:?}"
    );
    bridge.mirror_wake();
    bridge.mirror_step();
    assert_eq!(lane.mirror.lock().unwrap().takes, 0);
    assert!(
        bridge.state.logpin().unwrap().is_none(),
        "no pin is ever written"
    );
    // ON: the hello asks, and only an acknowledgment turns it on.
    let (lane, mut on) = FakeVerbLane::bridge(&[(1, SID)]);
    on.mirror = MirrorRun::new(crate::mirror::LogMode::Turns, &on.state);
    on.introduce();
    assert!(lane.hellos.lock().unwrap()[0].ends_with(" refill=in,say log=turns"));
    assert!(
        on.mirror_due(Instant::now()).is_none(),
        "not acknowledged: off"
    );
}

/// RULING Q10: a busy landing (`fenced`) keeps the pin and waits its retry,
/// never a pass every round; a definite refusal that is not the budget stops
/// the mirror for this bridge's life, said to the endpoint, the pin kept.
#[test]
fn a_busy_landing_waits_and_a_definite_refusal_stops_the_mirror() {
    let mut w = World::new();
    w.export();
    w.take();
    w.publish();
    // Read before the landing: the retry is set after it, so it lies past
    // this however long the host takes (a state check, no wall-clock budget).
    let before = Instant::now();
    w.settle();
    let retry = w
        .bridge
        .mirror
        .as_deref()
        .and_then(MirrorRun::retry_at)
        .expect("a fenced landing waits its retry");
    assert!(retry > before, "the retry lies past the landing");
    let due = w.bridge.mirror_due(before).expect("work is owed");
    assert_eq!(due, retry, "the loop's wait is the retry's, not zero");
    assert_eq!(
        w.bridge.mirror.as_deref().unwrap().batch().0.len(),
        1,
        "the pin kept"
    );
    // A refusal that is not the budget: the endpoint refuses the take.
    let mut r = World::new();
    r.lane.mirror.lock().unwrap().refuse_take = Some("ERR denied\n".to_string());
    r.take();
    assert!(
        r.bridge.mirror.as_deref().unwrap().stopped().is_some(),
        "stopped for this bridge's life"
    );
    assert!(
        r.bridge.mirror_due(Instant::now()).is_none(),
        "and never tried again"
    );
    assert!(
        r.lane
            .links
            .lock()
            .unwrap()
            .iter()
            .any(|l| l == "link mirror stopped reason=endpoint"),
        "said to the endpoint (`fabric status mirror=stopped:endpoint`)"
    );
}

/// RULING Q13: the node's total of mirrored bytes rides the pin across
/// batches (and bridges); at the cap the mirror stops, said as `capped`.
#[test]
fn the_nodes_total_rides_the_pin_and_the_cap_stops_the_mirror() {
    let mut w = World::new();
    w.take();
    w.publish();
    w.settle();
    let total = w.bridge.mirror.as_deref().unwrap().total();
    assert!(total > 0);
    let pin = crate::mirror::Pin::parse(&w.bridge.state.logpin().unwrap().unwrap()).unwrap();
    assert_eq!(
        (pin.slots.len(), pin.total),
        (0, total),
        "between batches: n=0, the total kept"
    );
    w.bridge
        .mirror
        .as_deref_mut()
        .unwrap()
        .set_total_for_tests(crate::mirror::TOTAL_MAX_BYTES);
    w.append();
    w.take();
    assert!(w.bridge.mirror.as_deref().unwrap().stopped().is_some());
    assert!(
        w.bridge.mirror.as_deref().unwrap().batch().0.is_empty(),
        "nothing taken"
    );
    assert!(w
        .lane
        .links
        .lock()
        .unwrap()
        .iter()
        .any(|l| l == "link mirror capped reason=total"));
}

/// RULING Q3, IN ORDER: a foreign record at the pinned seq of a batch's
/// middle slot re-pins that slot and every later slot of its session at
/// fresh seqs, in order; the slot before it keeps its own.
#[test]
fn a_collision_re_pins_the_slot_and_every_later_one_of_its_session_in_order() {
    let mut w = World::new();
    w.append();
    w.append();
    w.bridge.mirror = Some(MirrorRun::acked_for_tests(&w.bridge.state, 3));
    w.take();
    let before: Vec<u64> = w
        .bridge
        .mirror
        .as_deref()
        .unwrap()
        .batch()
        .0
        .iter()
        .map(|s| s.pseq)
        .collect();
    assert_eq!(before.len(), 3, "{before:?}");
    // Another writer's record at the middle slot's seq.
    let face = subject::session_face("f", &w.bridge.node, SID, "ev");
    let (mut god, _) = w.bridge.connect().unwrap();
    god.publish(
        w.bridge.producer_id,
        before[1],
        &face,
        b"v=1 t=1 ev=foreign",
    )
    .unwrap();
    w.publish();
    let outcomes = w.bridge.mirror.as_deref().unwrap().batch().1.to_vec();
    assert!(matches!(outcomes[0], Outcome::OnBus(_)), "{outcomes:?}");
    assert_eq!(outcomes[1], Outcome::Collided, "{outcomes:?}");
    w.repin();
    let after: Vec<u64> = w
        .bridge
        .mirror
        .as_deref()
        .unwrap()
        .batch()
        .0
        .iter()
        .map(|s| s.pseq)
        .collect();
    assert_eq!(
        after[0], before[0],
        "the slot before the collision keeps its seq"
    );
    assert!(
        after[1] > before[2] && after[2] > after[1],
        "fresh seqs, in order: {before:?} -> {after:?}"
    );
    w.publish();
    w.settle();
    let m = w.lane.mirror.lock().unwrap();
    assert_eq!(m.logs[SID].mirror_landed().map(|c| c.ms), Some(3));
    drop(m);
    let (rec, cov) = w.view();
    assert_eq!(
        (rec, cov),
        ([1, 1, 1], [0, 0, 0]),
        "each once, none missing"
    );
}

/// RULING Q3 ON A REPUBLISH: a bridge killed with a batch pinned and never
/// sent; another writer's record lands at the pinned seq meanwhile. The next
/// bridge's republish is answered deduped, the record at that offset is read
/// back, is not these bytes — a collision, never a landing — and the slot
/// lands at a fresh seq.
#[test]
fn a_republished_slots_dedup_is_read_back_and_a_foreign_one_is_a_collision() {
    let (lane, mut first, _server) = rig_with_broker(&[]);
    first.mirror = Some(MirrorRun::acked_for_tests(&first.state, 1));
    let mut log = Log::with_limits(1, limits());
    log.open_session(
        Kind::Spawn,
        Src::Aterm,
        Conf::Exact,
        aterm_slog::Actor::Aterm,
        aterm_slog::Fields::new(),
        Stamp::new(1, 1),
    )
    .expect("the session opens");
    lane.mirror
        .lock()
        .unwrap()
        .logs
        .insert(SID.to_string(), log);
    first.mirror_take();
    let pseq = first.mirror.as_deref().unwrap().batch().0[0].pseq;
    let (state, broker, node, producer) = (
        first.state.root().to_path_buf(),
        first.cfg.broker.clone(),
        first.node.clone(),
        first.producer_id,
    );
    drop(first);
    let face = subject::session_face("f", &node, SID, "ev");
    let (next_lane, mut next) = FakeVerbLane::bridge_with_state(&[], Some(&state));
    next.cfg.broker = broker;
    let (conn, _) = next.connect().unwrap();
    next.conn = Some(conn.into());
    let (mut god, _) = next.connect().unwrap();
    god.publish(producer, pseq, &face, b"v=1 t=1 ev=foreign")
        .unwrap();
    let logs = std::mem::take(&mut lane.mirror.lock().unwrap().logs);
    next_lane.mirror.lock().unwrap().logs = logs;
    next.mirror = MirrorRun::new(crate::mirror::LogMode::Turns, &next.state);
    next.mirror_hello_answered("OK lane=2 build=b log=turns");
    next.mirror_send();
    next.settle_acks();
    assert!(
        matches!(
            next.mirror.as_deref().unwrap().batch().1[0],
            Outcome::Deduped(_)
        ),
        "a republish's dedup is not yet a landing"
    );
    next.mirror_settle();
    assert_eq!(
        next.mirror.as_deref().unwrap().batch().1[0],
        Outcome::Collided,
        "read back: another writer's bytes"
    );
    assert!(next.collided.is_some() && next.conn.is_none());
    assert_eq!(
        next_lane.mirror.lock().unwrap().logs[SID]
            .mirror_landed()
            .map(|c| c.ms),
        Some(0),
        "nothing landed"
    );
    let (conn, _) = next.connect().unwrap();
    next.conn = Some(conn.into());
    next.mirror_repin();
    assert_ne!(next.mirror.as_deref().unwrap().batch().0[0].pseq, pseq);
    next.mirror_send();
    next.settle_acks();
    next.mirror_settle();
    assert_eq!(
        next_lane.mirror.lock().unwrap().logs[SID]
            .mirror_landed()
            .map(|c| c.ms),
        Some(1),
        "landed at its fresh seq"
    );
}

/// THE BUDGET IS ASKED ONCE A FACE (fixer F20): a batch of three slots of
/// one session makes one `Budget` round trip, not three (each round trip
/// reads every owed ack first, so per slot it serialized the batch one strict
/// ack at a time), and once a slot is on the bus the face is in the index:
/// the next batch asks nothing.
#[test]
fn a_batch_asks_the_subject_budget_once_a_face() {
    let mut w = World::new();
    w.append();
    w.append();
    w.bridge.mirror = Some(MirrorRun::acked_for_tests(&w.bridge.state, 3));
    let face = subject::session_face("f", &w.bridge.node, SID, "ev");
    let before = w.bridge.budget_queries;
    w.take();
    assert_eq!(w.bridge.mirror.as_deref().unwrap().batch().0.len(), 3);
    w.publish();
    assert_eq!(
        w.bridge.budget_queries - before,
        1,
        "one round trip for the batch's one new face"
    );
    assert!(
        w.bridge.indexed_subjects.contains(&face),
        "on the bus: in the index"
    );
    w.settle();
    w.append();
    w.append();
    w.take();
    w.publish();
    w.settle();
    assert_eq!(
        w.bridge.budget_queries - before,
        1,
        "an indexed face asks nothing"
    );
    let m = w.lane.mirror.lock().unwrap();
    assert_eq!(m.logs[SID].mirror_landed().map(|c| c.ms), Some(5));
}

/// A SESSION THE BUDGET REFUSES IS NOT LANDED (fixer F3): with the node's
/// subject budget at the mirror's reserve, the session's `ev` face is refused
/// and its cursor stays (said once, `log-refused`); once room comes back the
/// face opens at the session's first `ms` — every record on the bus, no hole
/// nobody said. Landing the refused slots (the defect) left the cursor at
/// the head and the face empty forever.
#[test]
fn a_session_the_budget_refuses_waits_and_opens_with_no_hole() {
    let (lane, mut bridge) = FakeVerbLane::bridge(&[]);
    let max = astream_broker::store::MAX_SUBJECTS_PER_PRODUCER as u64;
    let (_broker, _server) = seeded_broker(&lane, &mut bridge, &[], &[], max - max / 4);
    bridge.mirror = Some(MirrorRun::acked_for_tests(&bridge.state, 8));
    let mut log = Log::with_limits(1, limits());
    log.open_session(
        Kind::Spawn,
        Src::Aterm,
        Conf::Exact,
        aterm_slog::Actor::Aterm,
        aterm_slog::Fields::new(),
        Stamp::new(1, 1),
    )
    .expect("the session opens");
    log.mark(
        Mark::new(Kind::Seen, Src::Status, Conf::Strong),
        Stamp::new(2, 2),
    )
    .expect("a seen");
    lane.mirror
        .lock()
        .unwrap()
        .logs
        .insert(SID.to_string(), log);
    bridge.mirror_take();
    bridge.mirror_send();
    bridge.settle_acks();
    assert!(
        bridge
            .mirror
            .as_deref()
            .unwrap()
            .batch()
            .1
            .iter()
            .all(|o| *o == Outcome::Refused),
        "the face is past the reserve"
    );
    bridge.mirror_settle();
    assert_eq!(
        lane.mirror.lock().unwrap().logs[SID]
            .mirror_landed()
            .map(|c| c.ms),
        Some(0),
        "nothing refused lands"
    );
    assert!(
        bridge.mirror.as_deref().unwrap().batch().0.is_empty(),
        "the pin is dropped"
    );
    assert!(
        bridge.mirror.as_deref().unwrap().retry_at().is_some(),
        "and the next take waits"
    );
    // Room comes back: a filler subject retired.
    let (mut god, _) = bridge.connect().expect("a connection");
    god.retire_subject(
        bridge.producer_id,
        &format!("/f/f/pub/{}/node/filler/0", bridge.node),
    )
    .expect("retired");
    bridge.mirror_take();
    bridge.mirror_send();
    bridge.settle_acks();
    bridge.mirror_settle();
    assert_eq!(
        lane.mirror.lock().unwrap().logs[SID]
            .mirror_landed()
            .map(|c| c.ms),
        Some(2),
        "landed once on the bus"
    );
    let face = subject::session_face("f", &bridge.node, SID, "ev");
    let (rows, _) = god.fetch(0, &face, 100).expect("the face");
    let mut view = aterm_slog::bus::BusSpace::new(1);
    for (_, _, b) in rows {
        if let Ok(b) = aterm_slog::decode_bus(&String::from_utf8_lossy(&b)) {
            view.offer(b);
        }
    }
    assert!(
        view.holds_start(),
        "the face opens at the session's first ms"
    );
    assert!(view.holes().is_empty(), "{:?}", view.holes());
    assert_eq!(view.max_ms(), 2);
}

/// THE RETAKE AFTER A REFUSAL OWES ITS SNAPSHOT (fixer F3, completed): the
/// refused batch held the session's snapshot, and nothing of it landed; the
/// ring then evicted the session's claim, so the face opens with a cover —
/// and the retake's snapshot, whose `upto=` reaches it, seeds the reader's
/// fold with the open need. Counting the snapshot at the take (the defect)
/// left the face with a cover and no seed: `partial why=lost`, the need gone,
/// until 64 more bus-class records.
#[test]
fn a_retake_after_a_budget_refusal_owes_its_snapshot() {
    let (lane, mut bridge) = FakeVerbLane::bridge(&[]);
    let max = astream_broker::store::MAX_SUBJECTS_PER_PRODUCER as u64;
    let (_broker, _server) = seeded_broker(&lane, &mut bridge, &[], &[], max - max / 4);
    bridge.mirror = Some(MirrorRun::acked_for_tests(&bridge.state, 8));
    let mut log = Log::with_limits(1, limits());
    log.open_session(
        Kind::Spawn,
        Src::Aterm,
        Conf::Exact,
        aterm_slog::Actor::Aterm,
        aterm_slog::Fields::new(),
        Stamp::new(1, 1),
    )
    .expect("the session opens");
    log.mark(
        Mark::new(Kind::Need, Src::Mark, Conf::Claim)
            .by(aterm_slog::Actor::Harness)
            .fields(
                aterm_slog::Fields::new()
                    .word(aterm_slog::Key::Word, "help")
                    .expect("a word"),
            ),
        Stamp::new(2, 2),
    )
    .expect("a claim");
    lane.mirror
        .lock()
        .unwrap()
        .logs
        .insert(SID.to_string(), log);
    bridge.mirror_take();
    assert!(
        bridge
            .mirror
            .as_deref()
            .unwrap()
            .batch()
            .0
            .iter()
            .any(|s| s.body.contains(" op=snap ")),
        "PRECONDITION: the refused batch holds the snapshot"
    );
    bridge.mirror_send();
    bridge.settle_acks();
    bridge.mirror_settle();
    assert!(
        bridge.mirror.as_deref().unwrap().batch().0.is_empty(),
        "PRECONDITION: refused, the pin is dropped"
    );
    // While refused, the ring evicts the claim.
    for i in 0..40 {
        lane.mirror
            .lock()
            .unwrap()
            .logs
            .get_mut(SID)
            .expect("the log")
            .mark(
                Mark::new(Kind::Bell, Src::Engine, Conf::Exact),
                Stamp::new(10 + i, 10 + i),
            )
            .expect("a bell");
    }
    assert!(
        !lane.mirror.lock().unwrap().logs[SID]
            .live_ms()
            .any(|m| m == 2),
        "PRECONDITION: the claim is evicted"
    );
    let (mut god, _) = bridge.connect().expect("a connection");
    god.retire_subject(
        bridge.producer_id,
        &format!("/f/f/pub/{}/node/filler/0", bridge.node),
    )
    .expect("retired");
    bridge.mirror_take();
    bridge.mirror_send();
    bridge.settle_acks();
    bridge.mirror_settle();
    let face = subject::session_face("f", &bridge.node, SID, "ev");
    let (rows, _) = god.fetch(0, &face, 100).expect("the face");
    let mut view = aterm_slog::bus::BusSpace::new(1);
    for (_, _, b) in rows {
        if let Ok(b) = aterm_slog::decode_bus(&String::from_utf8_lossy(&b)) {
            view.offer(b);
        }
    }
    assert!(
        !view.covers().is_empty(),
        "PRECONDITION: the face lost the claim"
    );
    assert!(view.holes().is_empty(), "{:?}", view.holes());
    let (fold, seed) = view.fold_seeded();
    assert_eq!(seed, aterm_slog::bus::Seed::Snap, "seeded past the cover");
    assert!(
        fold.state()
            .needs
            .iter()
            .flatten()
            .any(|n| n.word == "help"),
        "the open need is held"
    );
}

/// A REFUSAL THAT IS NOT FOR GOOD IS SENT AGAIN (fixer F24): astream's
/// `ERR_REFUSED` (a failed append or fsync) and a lapsed capability keep the
/// slot owed and wait the retry — the mirror lands once the broker takes it;
/// only a refusal for good (`ERR_BAD_SUBJECT`) stops it for the bridge's life.
#[test]
fn a_transient_broker_refusal_is_retried_and_only_one_for_good_stops_the_mirror() {
    use astream_broker::proto::{
        CAP_EXPIRED_PREFIX, ERR_BAD_SUBJECT, ERR_REFUSED, ERR_UNAUTHORIZED,
    };
    let refusal = |code: u8, msg: &str| -> io::Result<(u64, bool)> {
        Err(astream_broker::BrokerRefusal {
            code,
            msg: msg.to_string(),
        }
        .into())
    };
    for (code, msg) in [
        (ERR_REFUSED, "refused: the log could not commit".to_string()),
        (
            ERR_UNAUTHORIZED,
            format!("{CAP_EXPIRED_PREFIX} at 1 (now 2, unix ms)"),
        ),
    ] {
        let mut w = World::new();
        w.take();
        let pseq = w.bridge.mirror.as_deref().unwrap().batch().0[0].pseq;
        w.bridge.conclude_mirror(0, pseq, true, refusal(code, &msg));
        let m = w.bridge.mirror.as_deref().unwrap();
        assert!(m.stopped().is_none(), "{msg}: not for good");
        assert_eq!(m.batch().1[0], Outcome::Owed, "{msg}: sent again");
        assert!(m.retry_at().is_some(), "{msg}: after the retry's wait");
        w.publish();
        w.settle();
        assert_eq!(
            w.lane.mirror.lock().unwrap().logs[SID]
                .mirror_landed()
                .map(|c| c.ms),
            Some(1),
            "{msg}: landed once the broker took it"
        );
    }
    let mut w = World::new();
    w.take();
    let pseq = w.bridge.mirror.as_deref().unwrap().batch().0[0].pseq;
    w.bridge
        .conclude_mirror(0, pseq, true, refusal(ERR_BAD_SUBJECT, "bad subject"));
    assert!(
        w.bridge.mirror.as_deref().unwrap().stopped().is_some(),
        "a refusal for good stops the mirror"
    );
}

/// THE ALIAS KEY IS THE NODE'S, NOT ITS CAPS' (fixer F12, ruling Q6): the key
/// a bridge mirrors under rides its pin, and the next bridge of the same
/// state dir — whatever caps it holds, renewed or none — takes the same key,
/// so an edge is one alias on the bus across bridge lives.
#[test]
fn the_alias_key_survives_a_bridge_restart_and_a_cap_change() {
    let mut w = World::new();
    w.take();
    w.publish();
    w.settle();
    let key = w.bridge.mirror.as_deref().unwrap().key().to_string();
    assert_eq!(key.len(), 32, "a key is minted");
    let pin = crate::mirror::Pin::parse(&w.bridge.state.logpin().unwrap().unwrap()).unwrap();
    assert_eq!(
        pin.key.map(|k| aterm_codec::hex::encode_lower(&k)),
        Some(key.clone()),
        "the emptied pin keeps it"
    );
    let next = MirrorRun::new(crate::mirror::LogMode::Turns, &w.bridge.state).expect("on");
    assert_eq!(next.key(), key, "the next bridge's key is the pin's");
    let (_lane, other) = FakeVerbLane::bridge(&[]);
    let elsewhere = MirrorRun::new(crate::mirror::LogMode::Turns, &other.state).expect("on");
    assert_ne!(elsewhere.key(), key, "another node's is its own");
}

/// RULING Q10 ON PRODUCTION TIMERS: the round that reads a batch's acks
/// parks for nothing while the landing they concluded is owed, so
/// `log-landed` follows the last ack, not the roster's tick. Judging the park
/// before the round read the acks (the defect), while every slot was still
/// `Sending` and the mirror owed nothing it could do then, made each batch
/// wait out up to `ROSTER_REFRESH` for its landing: a backfill of one batch a
/// tick, where the bucket's burst allows 2048 slots. The fast-timers seam the
/// world tests run under shortened that wait and hid it.
#[test]
fn the_round_that_reads_a_batchs_acks_parks_for_nothing_before_its_landing() {
    let mut w = World::new();
    w.append();
    w.append();
    w.bridge.mirror = Some(MirrorRun::acked_for_tests(&w.bridge.state, 3));
    // Production timers: the roster's tick a whole period off.
    w.bridge.roster_due = Instant::now() + ROSTER_REFRESH;
    assert!(
        w.bridge.deadlines.is_empty() && w.bridge.ghosts.is_empty(),
        "PRECONDITION: no other duty owed"
    );
    assert!(
        w.bridge.mailbox.is_idle(),
        "PRECONDITION: nothing queued ends a park early"
    );
    // THE LOOP'S PASS: the take and the pipelined send, then the round.
    assert!(w.bridge.mirror_then_round());
    let outcomes = w.bridge.mirror.as_deref().unwrap().batch().1.to_vec();
    assert_eq!(outcomes.len(), 3, "{outcomes:?}");
    assert!(
        outcomes.iter().all(|o| matches!(o, Outcome::OnBus(_))),
        "the round read every ack: {outcomes:?}"
    );
    assert!(
        w.lane.mirror.lock().unwrap().landings.is_empty(),
        "PRECONDITION: the landing is the next pass's"
    );
    assert_eq!(
        w.bridge.parked,
        Some(Duration::ZERO),
        "the landing is owed now: the round parks for nothing"
    );
    // The next pass lands, and the take after it is owed at once too.
    assert!(w.bridge.mirror_then_round());
    let m = w.lane.mirror.lock().unwrap();
    assert_eq!(m.landings.len(), 1, "landed on the pass after the acks");
    assert_eq!(m.logs[SID].mirror_landed().map(|c| c.ms), Some(3));
    drop(m);
    assert_eq!(
        w.bridge.parked,
        Some(Duration::ZERO),
        "the next take is owed now"
    );
    // The space's first snapshot, due once its records landed, is that next
    // take: sent, its ack read by the round, and landed by the pass after.
    assert!(w.bridge.mirror_then_round());
    let (slots, outcomes) = w.bridge.mirror.as_deref().unwrap().batch();
    assert!(
        !slots.is_empty() && slots.iter().all(|s| s.body.contains(" op=snap ")),
        "PRECONDITION: the take after the landing is the snapshot: {slots:?}"
    );
    assert!(outcomes.iter().all(|o| matches!(o, Outcome::OnBus(_))));
    assert_eq!(
        w.bridge.parked,
        Some(Duration::ZERO),
        "its landing is owed now"
    );
    assert!(w.bridge.mirror_then_round());
    assert_eq!(
        w.lane.mirror.lock().unwrap().landings.len(),
        2,
        "the snapshot landed"
    );
    assert!(w.bridge.mirror.as_deref().unwrap().batch().0.is_empty());
    // CONTROL: the zero is owed work, not every park. Caught up, a take
    // finds nothing and owes nothing more, and the park is the roster's.
    let takes = w.lane.mirror.lock().unwrap().takes;
    w.bridge.mirror_step();
    assert_eq!(
        w.lane.mirror.lock().unwrap().takes,
        takes + 1,
        "PRECONDITION: the pass asked a take"
    );
    assert!(
        w.bridge.mirror.as_deref().unwrap().batch().0.is_empty(),
        "PRECONDITION: the take is empty"
    );
    let now = Instant::now();
    w.bridge.roster_due = now + ROSTER_REFRESH;
    assert_eq!(w.bridge.mirror_due(now), None, "nothing is owed");
    assert_eq!(
        w.bridge.park_wait(now),
        ROSTER_REFRESH,
        "nothing owed: the park is the roster's"
    );
}

/// AN EMPTY TAKE THAT SAYS `more=1` IS BUSY, NOT WORK (ruling Q10): the
/// endpoint copied nothing because a session's timeline lock was held at the
/// take (`try_lock`'s `WouldBlock`: the lane never waits on the UI's lock).
/// The take stays owed and is asked again after `BUSY_WAIT`, so the loop
/// parks until then. Keeping it owed with no retry (the defect)
/// made the park zero and sent `log-take` again at once, for as long as the
/// UI held the lock.
#[test]
fn an_empty_take_that_says_more_waits_its_retry_and_never_spins() {
    let mut w = World::new();
    w.lane.mirror.lock().unwrap().refuse_take = Some("OK 0 slots=0 more=1\n".to_string());
    let before = Instant::now();
    w.bridge.roster_due = before + ROSTER_REFRESH;
    w.bridge.mirror_step();
    assert_eq!(
        w.lane.mirror.lock().unwrap().takes,
        1,
        "PRECONDITION: a take was asked"
    );
    assert!(
        w.bridge.mirror.as_deref().unwrap().batch().0.is_empty(),
        "PRECONDITION: it took nothing"
    );
    let retry = w
        .bridge
        .mirror
        .as_deref()
        .and_then(MirrorRun::retry_at)
        .expect("a busy take waits its retry");
    assert!(retry > before, "the retry lies past the take");
    assert_eq!(
        w.bridge.mirror_due(before),
        Some(retry),
        "the take is still owed, at its retry"
    );
    assert_eq!(
        w.bridge.park_wait(before),
        ROSTER_REFRESH.min(retry.duration_since(before)),
        "the loop parks until the retry, never for nothing"
    );
    assert!(w.bridge.park_wait(before) > Duration::ZERO);
    // CONTROL: once the endpoint can copy, the take takes the record.
    w.lane.mirror.lock().unwrap().refuse_take = None;
    w.take();
    assert_eq!(w.bridge.mirror.as_deref().unwrap().batch().0.len(), 1);
}

/// A directory's permissions, put back when the test ends however it ends.
#[cfg(unix)]
struct RestorePermissions(std::path::PathBuf, std::fs::Permissions);

#[cfg(unix)]
impl Drop for RestorePermissions {
    fn drop(&mut self) {
        let _ = std::fs::set_permissions(&self.0, self.1.clone());
    }
}

/// A RE-PIN THAT FAILS LANDS NOTHING (ruling Q3): after a collision, a re-pin
/// that cannot reserve a sequence, or cannot write its pin, leaves the
/// collided slot owed its re-pin — the pass sends nothing and lands nothing,
/// and the re-pin is tried again after `DEADLINE_RETRY`. Settling the batch
/// anyway (the defect) landed the collided slot's cursor: the endpoint moved
/// past a record whose bytes never reached the bus, a hole nobody said.
#[cfg(unix)]
#[test]
fn a_failed_re_pin_lands_nothing_and_is_tried_again() {
    use std::os::unix::fs::PermissionsExt as _;

    let mut w = World::new();
    w.take();
    w.fire(&["TakeRecord"]);
    w.plant_foreign();
    w.fire(&["Foreign"]);
    w.publish();
    let outcome = |w: &World| w.bridge.mirror.as_deref().unwrap().batch().1[0];
    assert_eq!(outcome(&w), Outcome::Collided, "PRECONDITION: a collision");
    w.check("the dedup is a collision: nothing on the bus (a stutter)");
    // The reattach the collision dropped the connection for, its sequences
    // from a block not reserved yet, onto a state dir that refuses writes.
    let (conn, _) = w.bridge.connect().expect("a reattach");
    w.bridge.conn = Some(conn.into());
    w.bridge.seq_block = None;
    let root = w.bridge.state.root().to_path_buf();
    let writable = std::fs::metadata(&root).unwrap().permissions();
    let _restore = RestorePermissions(root.clone(), writable.clone());
    let read_only = std::fs::Permissions::from_mode(0o500);
    std::fs::set_permissions(&root, read_only.clone()).unwrap();
    let pseq = w.bridge.mirror.as_deref().unwrap().batch().0[0].pseq;
    let before = Instant::now();
    w.bridge.mirror_step();
    assert!(
        w.lane.mirror.lock().unwrap().landings.is_empty(),
        "no landing while the re-pin is owed"
    );
    assert_eq!(outcome(&w), Outcome::Collided, "still owed its re-pin");
    let retry = w
        .bridge
        .mirror
        .as_deref()
        .and_then(MirrorRun::retry_at)
        .expect("the re-pin is tried again after its wait");
    assert!(retry > before);
    assert_eq!(
        w.bridge.mirror_due(before),
        Some(retry),
        "owed, at its retry"
    );
    // Nor does a settle reached any other way land a collided slot.
    w.settle();
    assert!(w.lane.mirror.lock().unwrap().landings.is_empty());
    // A sequence reserved, its pin not written: nothing changes either.
    std::fs::set_permissions(&root, writable.clone()).unwrap();
    w.bridge.next_seq().expect("a block");
    w.bridge.unsent = None;
    std::fs::set_permissions(&root, read_only).unwrap();
    assert!(!w.bridge.mirror_repin(), "a re-pin not on disk is not done");
    let m = w.bridge.mirror.as_deref().unwrap();
    assert_eq!(
        (m.batch().0[0].pseq, m.batch().1[0]),
        (pseq, Outcome::Collided),
        "the pin on disk stands"
    );
    std::fs::set_permissions(&root, writable).unwrap();
    w.check("the failed re-pins: nothing landed (a stutter)");
    // The state dir writes again: the re-pin is done, and the slot lands.
    assert!(w.bridge.mirror_repin());
    w.foreign = false;
    w.fire(&["Repin"]);
    w.check("re-pinned at a fresh seq");
    w.publish();
    w.settle();
    w.fire(&["Publish", "Landed", "Unpin"]);
    w.check("landed");
}

/// A BATCH OWED ITS RE-PIN SENDS NOTHING (ruling Q3, in order): two slots of
/// one session, the first collided and the second in flight when the
/// collision dropped the connection, so owed again after the reattach. While
/// the re-pin fails, neither the pass nor a send reached any other way puts
/// the second slot out at its old sequence: one the broker never got would
/// reach the bus ahead of the collided slot, and again at the fresh sequence
/// its re-pin gives it. Once the re-pin is done, both go out at fresh
/// sequences, in order, and land.
#[cfg(unix)]
#[test]
fn a_batch_owed_its_re_pin_sends_no_slot_at_its_old_sequence() {
    use std::os::unix::fs::PermissionsExt as _;

    let pseqs = |w: &World| -> Vec<u64> {
        let (slots, _) = w.bridge.mirror.as_deref().unwrap().batch();
        slots.iter().map(|s| s.pseq).collect()
    };
    let outcomes = |w: &World| w.bridge.mirror.as_deref().unwrap().batch().1.to_vec();
    let unacked = |w: &World| w.bridge.conn.as_ref().map(|c| c.unacked());
    let mut w = World::new();
    w.append();
    w.bridge.mirror = Some(MirrorRun::acked_for_tests(&w.bridge.state, 2));
    w.take();
    let old = pseqs(&w);
    assert_eq!(old.len(), 2, "PRECONDITION: two slots of one session");
    w.plant_foreign();
    w.publish();
    assert_eq!(
        outcomes(&w),
        [Outcome::Collided, Outcome::Sending],
        "PRECONDITION: the first slot collided, the second's ack went with the connection"
    );
    // The reattach the collision dropped the connection for rolls the
    // incarnation ([`Bridge::bring_presence_up`]): what was in flight is owed
    // again, and the re-pin's sequences come from a block not reserved yet,
    // on a state dir that refuses writes.
    let (conn, _) = w.bridge.connect().expect("a reattach");
    w.bridge.conn = Some(conn.into());
    w.bridge.inc += 1;
    let root = w.bridge.state.root().to_path_buf();
    let writable = std::fs::metadata(&root).unwrap().permissions();
    let _restore = RestorePermissions(root.clone(), writable.clone());
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o500)).unwrap();
    w.bridge.mirror_step();
    assert!(
        w.bridge.mirror.as_deref().unwrap().retry_at().is_some(),
        "PRECONDITION: the re-pin failed and waits its retry"
    );
    assert_eq!(
        (outcomes(&w), pseqs(&w), unacked(&w)),
        (vec![Outcome::Collided, Outcome::Owed], old.clone(), Some(0)),
        "the pass sent nothing: no slot at its old sequence"
    );
    // Nor does a send reached any other way.
    w.publish();
    assert_eq!(
        (outcomes(&w), unacked(&w)),
        (vec![Outcome::Collided, Outcome::Owed], Some(0)),
        "a send while the re-pin is owed sends nothing"
    );
    assert!(
        w.lane.mirror.lock().unwrap().landings.is_empty(),
        "nothing landed"
    );
    // The state dir writes again: both slots at fresh sequences, in order.
    std::fs::set_permissions(&root, writable).unwrap();
    assert!(w.bridge.mirror_repin(), "the re-pin is done");
    let fresh = pseqs(&w);
    assert!(
        fresh[0] > old[1] && fresh[1] > fresh[0],
        "fresh seqs, in order: {old:?} -> {fresh:?}"
    );
    w.publish();
    w.settle();
    assert_eq!(
        w.lane.mirror.lock().unwrap().logs[SID]
            .mirror_landed()
            .map(|c| c.ms),
        Some(2),
        "both landed"
    );
    let (rec, cov) = w.view();
    assert_eq!(
        (rec, cov),
        ([1, 1, 0], [0, 0, 0]),
        "each once, none missing"
    );
}

/// OFF IS NOTHING, IN TIME (the module doc): with the mirror off the loop's
/// park is judged BEFORE the round settles what the items before left owed,
/// as it was before the mirror. A pipelined ask whose ack that settle reads
/// arms its deadline for the next pass's park, not this one. Judging every
/// bridge's park after the settle (the defect) changed the default path's
/// wait: this park became the ask's deadline's, not the roster's.
#[test]
fn with_the_mirror_off_the_park_is_judged_before_the_rounds_settle() {
    let (lane, mut bridge, _server) = rig_with_broker(&[(1, "s-a"), (2, "s-b")]);
    assert!(bridge.mirror.is_none() && !bridge.mirror_engaged());
    bridge.refresh_sessions().unwrap();
    *lane.outbox.lock().unwrap() =
        Some("post sid=s-a id=1 to=@s-b kind=ask dl=1 len=4\nping".to_string());
    // The drain as the loop runs it, a round item ([`Bridge::in_round`]): its
    // post's ack stays owed until the round settles. A drain outside a round
    // reads its own acks when it ends, and arms the deadline before any park.
    bridge.in_round = true;
    bridge.drain_outbox();
    bridge.in_round = false;
    assert!(
        bridge
            .conn
            .as_ref()
            .is_some_and(|c| c.owed().any(|t| t.post().is_some())),
        "PRECONDITION: the ask's ack is owed, the round's to read"
    );
    assert!(
        bridge.deadlines.is_empty() && bridge.ghosts.is_empty(),
        "PRECONDITION: no deadline is armed before the round's settle"
    );
    assert!(
        bridge.mailbox.is_idle(),
        "PRECONDITION: nothing queued keeps the round from its settle"
    );
    // The roster a hang detector's horizon off, so only the deadline the
    // settle arms could make this park short; the park ends at an item queued
    // once the settle has retired the ask.
    bridge.roster_due = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    let (mailbox, sent) = (bridge.mailbox.clone(), lane.sent.clone());
    let waker = std::thread::spawn(move || {
        let give_up = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
        while !sent
            .lock()
            .unwrap()
            .iter()
            .any(|l| l.starts_with("outbox sent s-a 1 off="))
        {
            if Instant::now() >= give_up {
                return false;
            }
            std::thread::yield_now();
        }
        mailbox.push_event("EVENT".to_string());
        true
    });
    assert!(bridge.mirror_then_round());
    assert!(waker.join().unwrap(), "the round's settle retired the ask");
    assert_eq!(
        bridge.deadlines.len(),
        1,
        "the settle read the ask's ack and armed its deadline"
    );
    let parked = bridge.parked.expect("the round parked");
    assert!(
        parked > aterm_spec::wait::MUST_HAPPEN / 2,
        "judged before the settle: the roster's, not the ask's deadline: {parked:?}"
    );
    // CONTROL: the next pass's park is the deadline's.
    assert!(bridge.mirror_then_round());
    let parked = bridge.parked.expect("the round parked");
    assert!(
        parked < aterm_spec::wait::MUST_HAPPEN / 2,
        "the next park is the ask's deadline's: {parked:?}"
    );
}

/// A session's log holding its open, as the endpoint's store does.
fn opened_log() -> Log {
    let mut log = Log::with_limits(1, limits());
    log.open_session(
        Kind::Spawn,
        Src::Aterm,
        Conf::Exact,
        aterm_slog::Actor::Aterm,
        aterm_slog::Fields::new(),
        Stamp::new(1, 1),
    )
    .expect("the session opens");
    log
}

/// A bridge on `lane` against the broker at `socket`, the mirror on and
/// one slot a take, the session's log at the endpoint.
fn mirroring(lane: &FakeVerbLane, bridge: &mut Bridge, socket: &std::path::Path) {
    bridge.cfg.broker = socket.to_string_lossy().into_owned();
    let (conn, _) = bridge.connect().expect("a connection");
    bridge.conn = Some(conn.into());
    bridge.mirror = Some(MirrorRun::acked_for_tests(&bridge.state, 1));
    lane.mirror
        .lock()
        .unwrap()
        .logs
        .insert(SID.to_string(), opened_log());
}

/// The next bridge of `first`'s state dir, on `socket`: it recovers the pin
/// `first` left, with the endpoint's logs moved to its own lane.
fn successor(
    lane: &FakeVerbLane,
    first: Bridge,
    socket: &std::path::Path,
) -> (FakeVerbLane, Bridge) {
    let state = first.state.root().to_path_buf();
    drop(first);
    let (next_lane, mut next) = FakeVerbLane::bridge_with_state(&[], Some(&state));
    next.cfg.broker = socket.to_string_lossy().into_owned();
    let (conn, _) = next.connect().expect("a reattach");
    next.conn = Some(conn.into());
    let logs = std::mem::take(&mut lane.mirror.lock().unwrap().logs);
    next_lane.mirror.lock().unwrap().logs = logs;
    next.mirror = MirrorRun::new(crate::mirror::LogMode::Turns, &next.state);
    next.mirror_hello_answered("OK lane=2 build=b log=turns");
    (next_lane, next)
}

/// RULING Q3 UNDER RETENTION: a republished slot's dedup whose record
/// retention took before the read-back is NOT a collision. The broker drops a
/// record's dedup entry with the record, so the slot, sent again at its
/// pinned seq, appends anew and lands: no `cap-compromised`, no incarnation
/// rolled. Calling every unseen record another writer's (the defect) said the
/// node's cap compromised for a compaction, as `publish_reserved`, whose rule
/// Q3 names, never does for a post.
#[test]
fn a_dedup_whose_record_retention_took_is_sent_again_never_a_collision() {
    let (lane, mut first) = FakeVerbLane::bridge(&[]);
    let socket = lane.dir.path().join("broker.sock");
    let broker = crate::test_broker(lane.dir.path().join("broker.log")).unwrap();
    let _server = broker.serve(&socket).unwrap();
    mirroring(&lane, &mut first, &socket);
    // A batch on the bus, its bridge gone before the landing: the pin stands.
    first.mirror_take();
    first.mirror_send();
    first.settle_acks();
    let Outcome::OnBus(off) = first.mirror.as_deref().unwrap().batch().1[0] else {
        panic!("PRECONDITION: the slot is on the bus");
    };
    let (next_lane, mut next) = successor(&lane, first, &socket);
    let landed = |lane: &FakeVerbLane| {
        lane.mirror.lock().unwrap().logs[SID]
            .mirror_landed()
            .map(|c| c.ms)
    };
    next.mirror_send();
    next.settle_acks();
    assert_eq!(
        next.mirror.as_deref().unwrap().batch().1[0],
        Outcome::Deduped(off),
        "PRECONDITION: the republish is deduped at its record"
    );
    // RETENTION between the answer and its read-back: the record and its
    // dedup entry go together.
    assert!(
        broker.retain_before(off + 1).unwrap() > 0,
        "PRECONDITION: retention took it"
    );
    next.mirror_settle();
    assert!(
        next.collided.is_none(),
        "no collision is said for a record retention took"
    );
    assert!(next.conn.is_some(), "and the connection stands");
    assert_eq!(
        next.mirror.as_deref().unwrap().batch().1[0],
        Outcome::Owed,
        "sent again at its pinned seq"
    );
    assert_eq!(landed(&next_lane), Some(0), "nothing landed yet");
    next.mirror_send();
    next.settle_acks();
    let outcome = next.mirror.as_deref().unwrap().batch().1[0];
    assert!(
        matches!(outcome, Outcome::OnBus(at) if at > off),
        "no dedup entry is left: appended anew, {outcome:?}"
    );
    next.mirror_settle();
    assert!(next.collided.is_none());
    assert_eq!(landed(&next_lane), Some(1), "landed");
}

/// AND A SECOND DEDUP NAMING THE SAME UNSEEN OFFSET IS ANOTHER WRITER'S (the
/// control for the test above): a foreign record at the pinned seq on a face
/// the slot's is not — another session's, as a copy of this state dir would
/// mirror — is unseen by the read-back too, so the slot is sent again; the
/// broker, which still holds that record and its dedup entry, names the same
/// offset again, and that is the collision, never a landing. A slot that is
/// only ever sent again would hold its batch, and the mirror, forever.
#[test]
fn a_second_dedup_at_an_offset_the_face_does_not_hold_is_a_collision() {
    let (lane, mut first) = FakeVerbLane::bridge(&[]);
    let socket = lane.dir.path().join("broker.sock");
    let broker = crate::test_broker(lane.dir.path().join("broker.log")).unwrap();
    let _server = broker.serve(&socket).unwrap();
    mirroring(&lane, &mut first, &socket);
    first.mirror_take();
    let pseq = first.mirror.as_deref().unwrap().batch().0[0].pseq;
    let (producer, node) = (first.producer_id, first.node.clone());
    let (next_lane, mut next) = successor(&lane, first, &socket);
    let (mut god, _) = next.connect().unwrap();
    let elsewhere = subject::session_face("f", &node, "s-other", "ev");
    let (foreign, _) = god
        .publish(producer, pseq, &elsewhere, b"v=1 t=1 ev=foreign")
        .unwrap();
    let landed = |lane: &FakeVerbLane| {
        lane.mirror.lock().unwrap().logs[SID]
            .mirror_landed()
            .map(|c| c.ms)
    };
    next.mirror_send();
    next.settle_acks();
    assert_eq!(
        next.mirror.as_deref().unwrap().batch().1[0],
        Outcome::Deduped(foreign)
    );
    next.mirror_settle();
    assert!(next.collided.is_none(), "unseen once: not yet a collision");
    assert_eq!(next.mirror.as_deref().unwrap().batch().1[0], Outcome::Owed);
    next.mirror_send();
    next.settle_acks();
    assert_eq!(
        next.mirror.as_deref().unwrap().batch().1[0],
        Outcome::Deduped(foreign),
        "the broker holds the record: the same offset again"
    );
    next.mirror_settle();
    assert_eq!(
        next.mirror.as_deref().unwrap().batch().1[0],
        Outcome::Collided
    );
    assert!(
        next.collided.is_some() && next.conn.is_none(),
        "the collision path"
    );
    assert_eq!(landed(&next_lane), Some(0), "nothing landed");
    let (conn, _) = next.connect().unwrap();
    next.conn = Some(conn.into());
    next.mirror_repin();
    assert_ne!(next.mirror.as_deref().unwrap().batch().0[0].pseq, pseq);
    next.mirror_send();
    next.settle_acks();
    next.mirror_settle();
    assert_eq!(landed(&next_lane), Some(1), "landed at its fresh seq");
}
