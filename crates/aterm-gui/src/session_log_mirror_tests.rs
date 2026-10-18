// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The bus mirror's GUI half (session log phase 4): the two verbs over a
//! real store and real session logs, the adoption latch (ruling Q1), closed
//! sessions' tails across an update (Q4), the fence (Q12), the alias (Q6),
//! the old space an uncarried update ends (Q7), and the default that engages
//! nothing (Q5).

use aterm_slog::{
    Actor, BusCursor, Close, Conf, Fields, Key, Kind, Mark, Open, Out, Prior, RunOut, ShortId, Src,
    decode_bus,
};

use super::*;
use crate::session_store::{ExitActor, ExitReason, test_handle};

/// A key, as the bridge mints one.
const KEY: &str = "000102030405060708090a0b0c0d0e0f";

/// A bridge attached and its hello recorded with `log=turns`: the verbs are
/// served (inside `with_link_reset`).
fn mirroring() -> crate::fabric::BridgeGeneration {
    let generation = crate::fabric::next_bridge_generation();
    crate::fabric::bridge_attached(generation);
    let hello = crate::fabric::cmd_hello("lane=2 build=x log=turns", Some(generation));
    assert!(hello.trim_end().ends_with(" log=turns"), "{hello}");
    generation
}

/// A session registered in `store`, its log opened and a few commands run.
fn session(store: &crate::session_store::Store, id: u64) -> crate::session_store::SessionHandle {
    let h = test_handle(id);
    store.write().unwrap().register(h.clone());
    {
        let mut tl = h.ctx.timeline.lock().unwrap();
        let now = tl.slog().now();
        tl.slog_mut().open_session(None, now);
    }
    commands(&h, 3);
    h
}

fn commands(h: &crate::session_store::SessionHandle, n: usize) {
    let mut tl = h.ctx.timeline.lock().unwrap();
    let now = tl.slog().now();
    let log = tl.slog_mut().log_mut_for_tests();
    for _ in 0..n {
        let c = log
            .open(Open::new(Kind::Cmd, Src::Shell, Conf::Strong), now)
            .expect("cmd");
        log.close(
            c,
            Close::new(Out::Run(RunOut::Ok), Src::Shell, Conf::Exact),
            now,
        )
        .expect("closes");
    }
}

/// A take's slots: `(sid, cursor, body)`, the header's byte count checked
/// against the body (the Bytes framing a bridge reads, ruling Q11).
fn slots(reply: &str) -> (Vec<(String, BusCursor, String)>, bool) {
    let (header, body) = reply.split_once('\n').expect("a header line");
    let mut it = header.split(' ');
    assert_eq!(it.next(), Some("OK"), "{reply}");
    let n: usize = it
        .next()
        .and_then(|n| n.parse().ok())
        .expect("a byte count");
    assert_eq!(n, body.len(), "the header counts the body's bytes: {reply}");
    let k: usize = it
        .next()
        .and_then(|t| t.strip_prefix("slots="))
        .and_then(|k| k.parse().ok())
        .expect("slots=");
    let more = header.ends_with(" more=1");
    let out: Vec<(String, BusCursor, String)> = body
        .lines()
        .map(|l| {
            let mut p = l.splitn(3, ' ');
            let sid = p.next().expect("sid").to_string();
            let at = BusCursor::parse(p.next().expect("cursor")).expect("a cursor");
            (sid, at, p.next().expect("body").to_string())
        })
        .collect();
    assert_eq!(out.len(), k, "{reply}");
    (out, more)
}

/// The last cursor of each session in each seq space of a take, as the
/// bridge lands them (`aterm_link::mirror::landed_pairs`).
fn landings(slots: &[(String, BusCursor, String)]) -> String {
    let mut last: Vec<(String, BusCursor)> = Vec::new();
    for (sid, at, _) in slots {
        match last.iter_mut().find(|(s, c)| s == sid && c.ep == at.ep) {
            Some(slot) => slot.1 = *at,
            None => last.push((sid.clone(), *at)),
        }
    }
    last.iter()
        .map(|(s, at)| format!("{s} {}", at.word()))
        .collect::<Vec<_>>()
        .join(" ")
}

/// THE TWO VERBS: a take hands out each session's slots in a Bytes frame,
/// moves nothing; a landing moves the cursor, monotone; a ghost lane is
/// served nothing; the grammar is held.
#[test]
fn a_take_hands_out_slots_and_a_landing_moves_the_cursor() {
    crate::fabric::with_link_reset(|| {
        let generation = mirroring();
        let store = crate::session_store::new_store();
        let a = session(&store, 1);
        let b = session(&store, 2);
        let (first, more) = slots(&cmd_log_take(
            &store,
            &format!("n=64 key={KEY}"),
            Some(generation),
        ));
        assert!(!more);
        assert!(first.iter().any(|(s, ..)| s == a.sid.as_str()));
        assert!(first.iter().any(|(s, ..)| s == b.sid.as_str()));
        for (_, _, body) in &first {
            assert!(decode_bus(body).is_ok(), "{body}");
        }
        // A take moves nothing: the same records and covers again (the
        // space's first snapshot is not taken twice), each session's as a
        // set: the order the take visits the sessions in is not its promise.
        let unsnapped = |v: &[(String, BusCursor, String)]| -> Vec<(String, String)> {
            let mut out: Vec<(String, String)> = v
                .iter()
                .filter(|(.., b)| !b.contains(" op=snap "))
                .map(|(sid, _, b)| (sid.clone(), b.clone()))
                .collect();
            out.sort();
            out
        };
        let (again, _) = slots(&cmd_log_take(&store, "", Some(generation)));
        assert_eq!(unsnapped(&again), unsnapped(&first));
        assert_eq!(
            first.len() - unsnapped(&first).len(),
            2,
            "one snapshot each"
        );
        let land = landings(&first);
        assert_eq!(
            cmd_log_landed(&store, &land, Some(generation)),
            "OK ok ok\n",
            "{land}"
        );
        assert_eq!(
            cmd_log_landed(&store, &land, Some(generation)),
            "OK stale stale\n"
        );
        let (after, _) = slots(&cmd_log_take(&store, "", Some(generation)));
        assert!(after.is_empty(), "everything landed: {after:?}");
        commands(&a, 1);
        let (one, _) = slots(&cmd_log_take(&store, "n=1", Some(generation)));
        assert_eq!(one.len(), 1, "n= caps the take");
        // A ghost lane is served nothing and moves nothing.
        let ghost = crate::fabric::next_bridge_generation();
        assert_eq!(
            cmd_log_take(&store, "", Some(ghost)),
            "OK 0 slots=0 stale=1\n"
        );
        assert_eq!(cmd_log_landed(&store, &land, Some(ghost)), "OK stale=1\n");
        // The grammar.
        for bad in ["n=0", "n=65", "tier=content", "key=xyz", "frob=1"] {
            assert!(
                cmd_log_take(&store, bad, Some(generation)).starts_with("ERR usage"),
                "{bad}"
            );
        }
        for bad in ["", "s-a", "s-a 1:2", "s-a 1:2:3 s-b"] {
            assert!(
                cmd_log_landed(&store, bad, Some(generation)).starts_with("ERR usage"),
                "{bad:?}"
            );
        }
        let gone = format!("s-{} 1:1:1", "9".repeat(20));
        assert_eq!(cmd_log_landed(&store, &gone, Some(generation)), "OK gone\n");
    });
}

/// RULING Q1: until a seamless successor has registered every handed-off
/// session, a landing for a sid it does not hold is `adopting` (the bridge
/// keeps its pin) and a take is refused; once the session is registered the
/// same landing moves its carried cursor; once adoption has drained, an
/// unknown sid is `gone`.
#[test]
fn a_landing_before_adoption_keeps_the_pin_and_lands_after_adoption() {
    crate::fabric::with_link_reset(|| {
        let generation = mirroring();
        let store = crate::session_store::new_store();
        store.read().unwrap().mirror().set_adopting(true);
        let h = test_handle(1);
        {
            let mut tl = h.ctx.timeline.lock().unwrap();
            let now = tl.slog().now();
            tl.slog_mut().open_session(None, now);
        }
        commands(&h, 2);
        // The carried cursor the export drew: the session's open landed.
        let landed = {
            let mut tl = h.ctx.timeline.lock().unwrap();
            let log = tl.slog_mut().mirror_log();
            let first = BusCursor::new(log.base_ep(), 1, 1);
            log.land(first).expect("lands");
            first
        };
        let pinned = BusCursor::new(landed.ep, 3, 3);
        let pair = format!("{} {}", h.sid.as_str(), pinned.word());
        assert_eq!(
            cmd_log_landed(&store, &pair, Some(generation)),
            "OK adopting\n"
        );
        assert_eq!(
            cmd_log_take(&store, "", Some(generation)),
            "ERR busy adopting=1\n"
        );
        store.write().unwrap().register(h.clone());
        assert_eq!(
            cmd_log_landed(&store, &pair, Some(generation)),
            "OK ok\n",
            "registered: the pin lands on the carried cursor"
        );
        assert_eq!(
            h.ctx.timeline.lock().unwrap().slog().log().mirror_landed(),
            Some(pinned)
        );
        store.read().unwrap().mirror().set_adopting(false);
        let unknown = format!("s-{} {}", "7".repeat(20), pinned.word());
        assert_eq!(
            cmd_log_landed(&store, &unknown, Some(generation)),
            "OK gone\n"
        );
        // NEGATIVE CONTROL: the same landing on a store not adopting is `gone`
        // at once — what the bridge would drop its pin on.
        let fresh = crate::session_store::new_store();
        assert_eq!(cmd_log_landed(&fresh, &pair, Some(generation)), "OK gone\n");
    });
}

/// RULING Q4: a session that closes with an unlanded tail keeps it for the
/// bus; an update's manifest carries it as a tombstone; the successor's take
/// says it as one exact `gap why=dropped n=<k>`, which lands `done`. A
/// landing past the draw stands the Commit down, and once the Commit has
/// read the draw no closed tail lands here.
#[test]
fn a_closed_sessions_unlanded_tail_crosses_an_update_as_one_dropped_cover() {
    crate::fabric::with_link_reset(|| {
        let generation = mirroring();
        let store = crate::session_store::new_store();
        let h = session(&store, 1);
        // Mirrored in part: the session's open lands.
        let (first, _) = slots(&cmd_log_take(&store, "n=1", Some(generation)));
        assert_eq!(
            cmd_log_landed(&store, &landings(&first), Some(generation)),
            "OK ok\n"
        );
        let landed = first[0].1;
        store
            .write()
            .unwrap()
            .deregister_local_as(1, ExitReason::ShellExit, ExitActor::Unknown);
        assert_eq!(
            store.read().unwrap().mirror().counts(),
            (1, 0),
            "the tail is kept"
        );
        let head = h.ctx.timeline.lock().unwrap().slog().log().ms_head();
        // The update's manifest draws it.
        let manifest = crate::session_store::SessionHandoff::from_store(&store.read().unwrap());
        assert_eq!(
            manifest.mirror_dropped,
            vec![format!("{} {}:{head}", h.sid.as_str(), landed.word())]
        );
        assert!(
            !store.read().unwrap().mirror().past_draw(),
            "nothing moved since"
        );
        store.read().unwrap().mirror().lift_commit_fence();
        // The successor says the rest as one cover.
        let successor = crate::session_store::new_store();
        successor
            .read()
            .unwrap()
            .mirror()
            .adopt_tombstones(&manifest.mirror_dropped);
        let (cover, _) = slots(&cmd_log_take(&successor, "", Some(generation)));
        assert_eq!(cover.len(), 1, "{cover:?}");
        let b = decode_bus(&cover[0].2).expect("a cover");
        assert_eq!(b.rec.f.get_word(Key::Why), Some("dropped"));
        assert_eq!(b.rec.f.get_u(Key::N), Some(head - landed.ms), "exact");
        assert_eq!((b.ep, b.ms), (landed.ep, Some(landed.ms + 1)));
        assert_eq!(
            cmd_log_landed(&successor, &landings(&cover), Some(generation)),
            "OK done\n"
        );
        assert_eq!(successor.read().unwrap().mirror().counts(), (0, 0));
        // This process went on landing the tail after the draw: the Commit
        // stands down, and from its reading no closed tail lands here.
        let (rest, _) = slots(&cmd_log_take(&store, "n=1", Some(generation)));
        assert_eq!(
            cmd_log_landed(&store, &landings(&rest), Some(generation)),
            "OK ok\n"
        );
        assert!(
            store.read().unwrap().mirror().past_draw(),
            "landed past the draw"
        );
        let (more, _) = slots(&cmd_log_take(&store, "", Some(generation)));
        assert_eq!(
            cmd_log_landed(&store, &landings(&more), Some(generation)),
            "OK fenced\n",
            "the Commit's reading fences the closed tails"
        );
        store.read().unwrap().mirror().lift_commit_fence();
        assert_eq!(
            cmd_log_landed(&store, &landings(&more), Some(generation)),
            "OK done\n",
            "stood down: the tail lands, its close among it, and is forgotten"
        );
        assert_eq!(store.read().unwrap().mirror().counts(), (0, 0));
    });
}

/// RULINGS Q12 and Q2: a landing under an update's fence moves nothing and
/// says `fenced`; once the update stands down it lands.
#[test]
fn a_landing_is_refused_under_the_commit_fence() {
    crate::fabric::with_link_reset(|| {
        let generation = mirroring();
        let store = crate::session_store::new_store();
        let h = session(&store, 1);
        let (all, _) = slots(&cmd_log_take(&store, "", Some(generation)));
        let _carry = h.ctx.timeline.lock().unwrap().slog_carry_source();
        assert_eq!(
            cmd_log_landed(&store, &landings(&all), Some(generation)),
            "OK fenced\n"
        );
        assert!(
            h.ctx
                .timeline
                .lock()
                .unwrap()
                .slog()
                .log()
                .mirror_landed()
                .is_some_and(|c| c.ms == 0),
            "nothing moved"
        );
        h.ctx.timeline.lock().unwrap().slog_mut().lift_read_fence();
        assert_eq!(
            cmd_log_landed(&store, &landings(&all), Some(generation)),
            "OK ok\n"
        );
    });
}

/// A LANDING RACING THE COMMIT'S READING: a closed session's tail whose
/// landing is past the store's guard when the Commit reads the draw — and
/// finds nothing moved, so the Commit goes on — is `fenced`, its cursor
/// where the draw left it, never landed past the draw the successor's cover
/// starts from: the landing reads the fence under the tails' lock, which
/// the reading takes after setting it. After the stand-down it lands.
#[test]
fn a_landing_racing_the_commits_reading_is_fenced() {
    crate::fabric::with_link_reset(|| {
        let generation = mirroring();
        let store = crate::session_store::new_store();
        let h = session(&store, 1);
        let sid = h.sid.as_str().to_string();
        let (all, _) = slots(&cmd_log_take(&store, "", Some(generation)));
        store
            .write()
            .unwrap()
            .deregister_local_as(1, ExitReason::ShellExit, ExitActor::Unknown);
        assert_eq!(store.read().unwrap().mirror().counts(), (1, 0), "a tail");
        let manifest = crate::session_store::SessionHandoff::from_store(&store.read().unwrap());
        assert_eq!(manifest.mirror_dropped.len(), 1, "the tail is drawn");
        let reading = std::sync::Arc::new(std::sync::Mutex::new(None));
        {
            let reading = std::sync::Arc::clone(&reading);
            store
                .read()
                .unwrap()
                .mirror()
                .arm_before_landing(move |m| *reading.lock().unwrap() = Some(m.past_draw()));
        }
        assert_eq!(
            cmd_log_landed(&store, &landings(&all), Some(generation)),
            "OK fenced\n",
            "the Commit read the draw first"
        );
        assert_eq!(
            *reading.lock().unwrap(),
            Some(false),
            "PRECONDITION: the reading ran inside the landing and found nothing moved"
        );
        assert_eq!(
            store
                .read()
                .unwrap()
                .mirror()
                .tail_view(&sid, |log| log.mirror_landed().map(|c| c.ms)),
            Some(Some(0)),
            "nothing landed past the draw"
        );
        store.read().unwrap().mirror().lift_commit_fence();
        assert_eq!(
            cmd_log_landed(&store, &landings(&all), Some(generation)),
            "OK ok\n",
            "after the stand-down it lands"
        );
    });
}

/// RULING Q6: an edge's actor crosses as `by=e<8 hex>`, the bridge key's
/// hash of its sid, never the sid; one edge is one alias. Without a key the
/// actor is `-`.
#[test]
fn an_edges_actor_crosses_as_its_alias_never_its_sid() {
    crate::fabric::with_link_reset(|| {
        let generation = mirroring();
        let store = crate::session_store::new_store();
        let h = session(&store, 1);
        let edge = ShortId::new("s-0123456789abcdef0123").expect("sid");
        {
            let mut tl = h.ctx.timeline.lock().unwrap();
            let now = tl.slog().now();
            for word in ["help", "review"] {
                tl.slog_mut()
                    .mirror_log()
                    .mark(
                        Mark::new(Kind::Need, Src::Mark, Conf::Claim)
                            .by(Actor::Sid(edge))
                            .fields(Fields::new().word(Key::Word, word).expect("w")),
                        now,
                    )
                    .expect("a claim");
            }
        }
        let mut key = [0u8; 16];
        assert!(aterm_codec::hex::decode_into(KEY.as_bytes(), &mut key));
        let alias = alias_of(&key, &edge).expect("an alias");
        assert!(alias.as_str().starts_with('e') && alias.as_str().len() == 9);
        let (with_key, _) = slots(&cmd_log_take(
            &store,
            &format!("key={KEY}"),
            Some(generation),
        ));
        let needs: Vec<&String> = with_key
            .iter()
            .map(|(.., b)| b)
            .filter(|b| b.contains(" k=need "))
            .collect();
        assert_eq!(needs.len(), 2);
        for b in &needs {
            assert!(b.contains(&format!(" by={alias} ")), "{b}");
            assert!(!b.contains(edge.as_str()), "a sid on the bus: {b}");
        }
        let (without, _) = slots(&cmd_log_take(&store, "", Some(generation)));
        assert!(
            without
                .iter()
                .filter(|(.., b)| b.contains(" k=need "))
                .all(|(.., b)| !b.contains(" by=") && !b.contains(edge.as_str())),
            "{without:?}"
        );
    });
}

/// RULING Q7, the host's half: an adoption whose update did not carry the
/// log but carried the mirror's fence-time cursor owes the old space's tail
/// as one `why=reset` cover in that space.
#[test]
fn an_uncarried_adoption_owes_the_old_spaces_tail() {
    let mut slog = crate::session_log::SessionLog::default();
    let prior = Prior {
        landed: BusCursor::new(3, 40, 9),
        ms_head: 12,
    };
    let at = slog.now();
    slog.adopt(
        crate::session_log::LogAdopt {
            ids: crate::session_log::LogCarry::parse("3 1 1 1"),
            seq: 100,
            lines: None,
            keeper_ep: None,
            mirror: Some(prior),
        },
        false,
        at,
    );
    assert_eq!(slog.log().mirror_prior(), Some(prior));
    let now = slog.now();
    let take = slog
        .mirror_log()
        .bus_copy(8, now)
        .encode(aterm_slog::Tier::Wire, &|_| None);
    let first = decode_bus(&take.slots[0].body).expect("decodes");
    assert_eq!((first.ep, first.ms), (3, Some(10)));
    assert_eq!(first.rec.f.get_u(Key::N), Some(3));
    assert_eq!(first.rec.f.get_word(Key::Why), Some("reset"));
    // A keeper's crash adoption is another seq space with no cursor: none.
    let mut crashed = crate::session_log::SessionLog::default();
    let at = crashed.now();
    crashed.adopt(
        crate::session_log::LogAdopt {
            mirror: Some(prior),
            ..Default::default()
        },
        true,
        at,
    );
    assert_eq!(crashed.log().mirror_prior(), None);
}

/// RULING Q5: with the mirror off nothing engages. A session that runs and
/// closes leaves no tail, no tombstone, no manifest row, and its log no
/// cursor; its carry holds no `ms=`.
#[test]
fn the_default_engages_nothing() {
    let store = crate::session_store::new_store();
    let h = session(&store, 1);
    commands(&h, 5);
    assert_eq!(
        h.ctx.timeline.lock().unwrap().slog().log().mirror_landed(),
        None
    );
    let carry = h
        .ctx
        .timeline
        .lock()
        .unwrap()
        .slog()
        .log()
        .carry(aterm_slog::CarryBudget::SESSION, &[])
        .expect("carries");
    assert!(!carry.lines[0].contains(" ms="), "{}", carry.lines[0]);
    assert!(carry.lines.iter().all(|l| !l.starts_with("R ")));
    store
        .write()
        .unwrap()
        .deregister_local_as(1, ExitReason::ShellExit, ExitActor::Unknown);
    assert_eq!(store.read().unwrap().mirror().counts(), (0, 0));
    let manifest = crate::session_store::SessionHandoff::from_store(&store.read().unwrap());
    assert!(manifest.mirror_dropped.is_empty());
}

// ---- fixer round (phase 4 review) ------------------------------------------

/// A session registered in `store` over a ring of eight records, so a few
/// bells evict; its open is `ms` 1.
fn small_session(
    store: &crate::session_store::Store,
    id: u64,
) -> crate::session_store::SessionHandle {
    let h = test_handle(id);
    h.ctx
        .timeline
        .lock()
        .unwrap()
        .slog_mut()
        .replace_log(aterm_slog::Log::with_limits(
            1,
            aterm_slog::Limits::new(8, 1 << 20).expect("a ring"),
        ));
    store.write().unwrap().register(h.clone());
    h
}

/// `mark - seen`: one bus-class record.
fn seen(h: &crate::session_store::SessionHandle) {
    let mut tl = h.ctx.timeline.lock().unwrap();
    let now = tl.slog().now();
    tl.slog_mut()
        .log_mut_for_tests()
        .mark(Mark::new(Kind::Seen, Src::Status, Conf::Strong), now)
        .expect("a seen");
}

/// Local records until the ring no longer holds `ms`.
fn evict(h: &crate::session_store::SessionHandle, ms: u64) {
    for _ in 0..64 {
        let mut tl = h.ctx.timeline.lock().unwrap();
        if !tl.slog().log().live_ms().any(|m| m == ms) {
            return;
        }
        let now = tl.slog().now();
        tl.slog_mut()
            .log_mut_for_tests()
            .mark(Mark::new(Kind::Bell, Src::Engine, Conf::Exact), now)
            .expect("a bell");
    }
    panic!("the ring never evicted ms {ms}");
}

/// Every slot `store` holds, taken and landed until nothing is left, into
/// `view` per sid: what a bridge's drain puts on the bus.
fn drain(
    store: &crate::session_store::Store,
    generation: crate::fabric::BridgeGeneration,
    views: &mut std::collections::BTreeMap<String, aterm_slog::bus::BusSpace>,
) {
    for _ in 0..1000 {
        let (taken, _) = slots(&cmd_log_take(store, "n=64", Some(generation)));
        if taken.is_empty() {
            return;
        }
        for (sid, _, body) in &taken {
            let b = decode_bus(body).expect("a body");
            views
                .entry(sid.clone())
                .or_insert_with(|| aterm_slog::bus::BusSpace::new(b.ep))
                .offer(b);
        }
        let reply = cmd_log_landed(store, &landings(&taken), Some(generation));
        assert!(
            !reply.contains("fenced") && !reply.contains("adopting"),
            "{reply}"
        );
    }
    panic!("the drain never ended");
}

/// FIXER F1: a session that closes while an update's export holds its
/// readers' fence keeps its tail even when its cursor stands AT the fence
/// (judged against its live `ms` head, not the fence's); the tail's landings
/// are refused under the fence, and when the update stands down the fence is
/// lifted with it: the close lands `done`, and a later session's slots flow.
#[test]
fn a_session_closing_under_the_fence_keeps_its_tail_and_lands_after_the_stand_down() {
    crate::fabric::with_link_reset(|| {
        let generation = mirroring();
        let store = crate::session_store::new_store();
        let h = session(&store, 1);
        let mut views = std::collections::BTreeMap::new();
        drain(&store, generation, &mut views);
        let _carry = h.ctx.timeline.lock().unwrap().slog_carry_source();
        commands(&h, 1);
        store
            .write()
            .unwrap()
            .deregister_local_as(1, ExitReason::ShellExit, ExitActor::Unknown);
        assert_eq!(
            store.read().unwrap().mirror().counts(),
            (1, 0),
            "the tail is kept although the cursor stands at the fence"
        );
        let (fenced, _) = slots(&cmd_log_take(&store, "", Some(generation)));
        assert!(fenced.is_empty(), "nothing past the fence: {fenced:?}");
        // The update stands down (the session it drew is gone): the App lifts
        // every live log's fence and the closed tails' with them.
        store.read().unwrap().mirror().lift_commit_fence();
        let (rest, _) = slots(&cmd_log_take(&store, "", Some(generation)));
        assert!(
            rest.iter().any(|(.., b)| b.contains(" op=close node=- ")),
            "the close is taken: {rest:?}"
        );
        assert_eq!(
            cmd_log_landed(&store, &landings(&rest), Some(generation)),
            "OK done\n"
        );
        assert_eq!(store.read().unwrap().mirror().counts(), (0, 0));
        let later = session(&store, 2);
        let (next, _) = slots(&cmd_log_take(&store, "", Some(generation)));
        assert!(
            next.iter().any(|(sid, ..)| sid == later.sid.as_str()),
            "a later session's slots flow: {next:?}"
        );
    });
}

/// FIXERS F2 AND F19: while the node mirrors, a session whose whole life
/// falls where no take runs (a broker outage, before the bridge's first take)
/// is kept whole when it closes: its take says from `ms` 1, the evicted ones
/// as one exact cover, its close among the rest; and a live log no take
/// reached carries its `ms` across an update. With the mirror off (the
/// default) the same session keeps nothing.
#[test]
fn a_session_no_take_reached_is_kept_whole_while_the_node_mirrors() {
    crate::fabric::with_link_reset(|| {
        let generation = mirroring();
        let store = crate::session_store::new_store();
        store.read().unwrap().mirror().set_served(true);
        let h = small_session(&store, 1);
        commands(&h, 6);
        assert_eq!(
            h.ctx.timeline.lock().unwrap().slog().log().mirror_landed(),
            None,
            "PRECONDITION: no take reached it"
        );
        let live = small_session(&store, 2);
        let carry = live
            .ctx
            .timeline
            .lock()
            .unwrap()
            .slog_carry_source()
            .prepare()
            .encode(aterm_slog::CarryBudget::SESSION)
            .expect("carries");
        assert!(
            carry.lines[0].contains(" ms="),
            "the carry numbers its bus class: {}",
            carry.lines[0]
        );
        live.ctx
            .timeline
            .lock()
            .unwrap()
            .slog_mut()
            .lift_read_fence();
        store
            .write()
            .unwrap()
            .deregister_local_as(1, ExitReason::ShellExit, ExitActor::Unknown);
        assert_eq!(store.read().unwrap().mirror().counts(), (1, 0), "kept");
        let mut views = std::collections::BTreeMap::new();
        drain(&store, generation, &mut views);
        let view = &views[h.sid.as_str()];
        assert!(view.holds_start(), "from its first ms");
        assert!(view.holes().is_empty(), "{:?}", view.holes());
        assert!(view.covered() > 0, "PRECONDITION: the ring evicted");
        let head = h.ctx.timeline.lock().unwrap().slog().log().ms_head();
        assert_eq!(view.max_ms(), head, "every ms said, the close last");
        assert_eq!(store.read().unwrap().mirror().counts(), (0, 0), "done");
        // NEGATIVE CONTROL: the default keeps nothing of the same session.
        let off = crate::session_store::new_store();
        let h = small_session(&off, 1);
        commands(&h, 6);
        off.write()
            .unwrap()
            .deregister_local_as(1, ExitReason::ShellExit, ExitActor::Unknown);
        assert_eq!(off.read().unwrap().mirror().counts(), (0, 0));
    });
}

/// FIXERS F13, F14 AND F22: a replaced bus log (`link log` `forgot=1`) is
/// forgotten without locking any timeline — a busy one included — and every
/// cursor starts over: a live session is taken again from its first record,
/// a closed session's partly landed tail opens with one exact cover of what
/// it no longer holds, a tombstone covers from `ms` 1. A new log's reader
/// sees no hole.
#[test]
fn a_replaced_bus_log_starts_every_cursor_over_and_locks_no_timeline() {
    crate::fabric::with_link_reset(|| {
        let generation = mirroring();
        let store = crate::session_store::new_store();
        let a = session(&store, 1);
        let b = session(&store, 2);
        let mut old = std::collections::BTreeMap::new();
        drain(&store, generation, &mut old);
        commands(&b, 2);
        // b's tail lands in part, then b closes.
        let (part, _) = slots(&cmd_log_take(&store, "n=1", Some(generation)));
        let part: Vec<_> = part
            .into_iter()
            .filter(|(s, ..)| s == b.sid.as_str())
            .collect();
        if !part.is_empty() {
            cmd_log_landed(&store, &landings(&part), Some(generation));
        }
        store
            .write()
            .unwrap()
            .deregister_local_as(2, ExitReason::ShellExit, ExitActor::Unknown);
        assert_eq!(store.read().unwrap().mirror().counts(), (1, 0));
        // The forget, with a's timeline held by another thread all along.
        let (held_tx, held_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let ctx = a.ctx.clone();
        let holder = std::thread::spawn(move || {
            let _tl = ctx.timeline.lock().unwrap();
            held_tx.send(()).unwrap();
            let _ = release_rx.recv();
        });
        held_rx
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("the timeline is held");
        // A predecessor's tombstone, landed to `ms` 3 of 9 in the old log.
        let tomb = "s-tomb";
        store
            .read()
            .unwrap()
            .mirror()
            .adopt_tombstones(&[format!("{tomb} 1:5:3:9")]);
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let forgetting = store.clone();
        let forget = std::thread::spawn(move || {
            forget_landed(&forgetting);
            done_tx.send(()).unwrap();
        });
        let returned = done_rx.recv_timeout(aterm_spec::wait::MUST_HAPPEN);
        release_tx.send(()).unwrap();
        holder.join().unwrap();
        forget.join().unwrap();
        assert!(
            returned.is_ok(),
            "the forget waited on a session's timeline"
        );
        let mut new = std::collections::BTreeMap::new();
        drain(&store, generation, &mut new);
        for (who, sid) in [("live", a.sid.as_str()), ("closed", b.sid.as_str())] {
            let view = new
                .get(sid)
                .unwrap_or_else(|| panic!("{who}: nothing taken again"));
            assert!(view.holds_start(), "{who}: from ms 1");
            assert!(view.holes().is_empty(), "{who}: {:?}", view.holes());
        }
        assert!(
            new[b.sid.as_str()].covered() > 0,
            "the tail's landed part is one exact cover in the new log"
        );
        // The tombstone starts over too: one cover of all nine, from `ms` 1.
        let view = new.get(tomb).expect("the tombstone is taken again");
        assert_eq!(view.covers(), &[(1, 9, "dropped")], "one exact cover");
        assert!(view.holds_start(), "tombstone: from ms 1");
        assert!(view.holes().is_empty(), "tombstone: {:?}", view.holes());
        assert_eq!(store.read().unwrap().mirror().counts(), (0, 0), "done");
    });
}

/// FIXER F13's drawn generation, OFF (ruling Q5): on a store that never
/// mirrored — no hello asked, nothing taken, nothing engaged — a bus log
/// replaced between an update's draw and its Commit stands nothing down.
/// The export's own syncs (each session's carry source) engage nothing.
#[test]
fn a_replaced_bus_log_stands_no_update_down_on_a_store_that_never_mirrored() {
    let store = crate::session_store::new_store();
    let h = session(&store, 1);
    let manifest = crate::session_store::SessionHandoff::from_store(&store.read().unwrap());
    assert!(manifest.mirror_dropped.is_empty());
    // The export: the session's carry source, its log synced and fenced.
    let _carry = h.ctx.timeline.lock().unwrap().slog_carry_source();
    // `link log` answered `forgot=1` (the bridge's lane, mirror on or off).
    forget_landed(&store);
    assert!(
        !store.read().unwrap().mirror().past_draw(),
        "off is nothing: no cursor was drawn, so the Commit goes on"
    );
    store.read().unwrap().mirror().lift_commit_fence();
    let mut tl = h.ctx.timeline.lock().unwrap();
    tl.slog_mut().lift_read_fence();
    assert_eq!(tl.slog().log().mirror_landed(), None, "nothing engaged");
}

/// FIXER F13's drawn generation, ON: once a log holds a cursor, a bus log
/// replaced between the draw and the Commit stands the update down — the
/// cursor the export drew is the replaced log's — and so does one whose
/// only cursor the export's own sync met after the manifest was drawn (a
/// carried cursor no take had touched in this process).
#[test]
fn a_replaced_bus_log_stands_down_an_update_that_drew_a_cursor() {
    crate::fabric::with_link_reset(|| {
        let generation = mirroring();
        let store = crate::session_store::new_store();
        let h = session(&store, 1);
        let mut views = std::collections::BTreeMap::new();
        drain(&store, generation, &mut views);
        let draw = |store: &crate::session_store::Store| {
            let _ = crate::session_store::SessionHandoff::from_store(&store.read().unwrap());
        };
        draw(&store);
        let _carry = h.ctx.timeline.lock().unwrap().slog_carry_source();
        assert!(
            !store.read().unwrap().mirror().past_draw(),
            "PRECONDITION: nothing moved since the draw"
        );
        store.read().unwrap().mirror().lift_commit_fence();
        h.ctx.timeline.lock().unwrap().slog_mut().lift_read_fence();
        draw(&store);
        let _carry = h.ctx.timeline.lock().unwrap().slog_carry_source();
        forget_landed(&store);
        assert!(
            store.read().unwrap().mirror().past_draw(),
            "the export drew the replaced log's cursor: the Commit stands down"
        );
        store.read().unwrap().mirror().lift_commit_fence();
        h.ctx.timeline.lock().unwrap().slog_mut().lift_read_fence();
        // A log whose cursor no sync has met yet (a successor's carried one).
        let carried = crate::session_store::new_store();
        let c = session(&carried, 1);
        c.ctx
            .timeline
            .lock()
            .unwrap()
            .slog_mut()
            .log_mut_for_tests()
            .engage_mirror();
        draw(&carried);
        let _carry = c.ctx.timeline.lock().unwrap().slog_carry_source();
        forget_landed(&carried);
        assert!(
            carried.read().unwrap().mirror().past_draw(),
            "the export met the cursor after the draw: the Commit stands down"
        );
        carried.read().unwrap().mirror().lift_commit_fence();
    });
}

/// FIXER F13's drawn generation, ADOPTED: a successor whose only cursors are
/// its predecessor's tombstones — no live log synced, no recorded hello
/// asking for the mirror — holds cursors all the same. A bus log replaced
/// between the draw and the Commit stands the update down (the drawn row is
/// the replaced log's cursor: a successor's cover from it would start at
/// `ms` 4 of the new log, leaving 1..3 unsaid), and the retry's draw carries
/// the tombstone from `ms` 0.
#[test]
fn a_replaced_bus_log_stands_down_an_update_whose_only_cursors_are_adopted() {
    let store = crate::session_store::new_store();
    assert!(
        !store.read().unwrap().mirror().served(),
        "PRECONDITION: no hello asks"
    );
    store
        .read()
        .unwrap()
        .mirror()
        .adopt_tombstones(&["s-tomb 1:5:3:9".to_string()]);
    let manifest = crate::session_store::SessionHandoff::from_store(&store.read().unwrap());
    assert_eq!(manifest.mirror_dropped, ["s-tomb 1:5:3:9"], "drawn");
    forget_landed(&store);
    assert!(
        store.read().unwrap().mirror().past_draw(),
        "the drawn tombstone is the replaced log's: the Commit stands down"
    );
    store.read().unwrap().mirror().lift_commit_fence();
    let again = crate::session_store::SessionHandoff::from_store(&store.read().unwrap());
    assert_eq!(
        again.mirror_dropped,
        ["s-tomb 1:0:0:9"],
        "the retry draws the tombstone from ms 0 of the new log"
    );
    assert!(
        !store.read().unwrap().mirror().past_draw(),
        "nothing moved since the retry's draw"
    );
    store.read().unwrap().mirror().lift_commit_fence();
}

/// FIXER F15: the verbs are served only while the owning bridge's RECORDED
/// hello asked for the mirror — a bridge whose hello did not is refused
/// definitely, never engages a log — and retained state no bridge serves is
/// said in `fabric status` (`mirror=unserved tails=<k> tombstones=<j>`).
#[test]
fn the_verbs_are_refused_unless_the_recorded_hello_asked_and_retained_state_is_said() {
    crate::fabric::with_link_reset(|| {
        let generation = mirroring();
        let store = crate::session_store::new_store();
        let h = session(&store, 1);
        let (first, _) = slots(&cmd_log_take(&store, "n=1", Some(generation)));
        assert_eq!(first.len(), 1);
        store
            .write()
            .unwrap()
            .deregister_local_as(1, ExitReason::ShellExit, ExitActor::Unknown);
        assert_eq!(store.read().unwrap().mirror().counts(), (1, 0));
        assert_eq!(
            store.read().unwrap().mirror().unserved_facts(),
            "",
            "served: the bridge's own facts say the mirror"
        );
        // The owner turned the mirror off; the bridge restarted and said so.
        let off = crate::fabric::next_bridge_generation();
        crate::fabric::bridge_attached(off);
        let hello = crate::fabric::cmd_hello("lane=2 build=x", Some(off));
        assert!(!hello.contains("log="), "{hello}");
        let fresh = session(&store, 2);
        assert!(
            cmd_log_take(&store, "", Some(off)).starts_with("ERR mirror off"),
            "a take is refused"
        );
        assert!(
            cmd_log_landed(&store, &format!("{} 1:1:1", fresh.sid.as_str()), Some(off))
                .starts_with("ERR mirror off"),
            "a landing is refused"
        );
        assert_eq!(
            fresh
                .ctx
                .timeline
                .lock()
                .unwrap()
                .slog()
                .log()
                .mirror_landed(),
            None,
            "and no log engaged"
        );
        assert_eq!(
            store.read().unwrap().mirror().unserved_facts(),
            " mirror=unserved tails=1 tombstones=0"
        );
        drop(h);
    });
}

// ---- TIER-1: the GUI half against the derived `LogMirror` model ----------
//
// The REAL GUI half — `log-take` and `log-landed` over a real store, its
// adoption latch, the export's fence and its stand-down, a closed session's
// tail ([`StoreMirror::retire`]) — driven by a test bridge that pins one
// slot a take and publishes it to an in-memory `ev` face. After every step
// the world is projected onto the model's variables and must be the model's
// state after the matching actions (a refused landing is a stutter).
// NEGATIVE CONTROLS: each GUI-half mutant replayed through the broken
// handler it stands for ([`Broken`]), and the projected law breaks.

struct GuiWorld {
    store: crate::session_store::Store,
    generation: crate::fabric::BridgeGeneration,
    h: crate::session_store::SessionHandle,
    sid: String,
    /// The successor store and its session, not registered until AdoptLate.
    succ: Option<(
        crate::session_store::Store,
        crate::session_store::SessionHandle,
    )>,
    pin: Option<(BusCursor, String)>,
    onbus: bool,
    bus: Vec<String>,
    model: aterm_spec::derive::Model,
    expect: aterm_spec::interp::State,
    e: [i64; 2],
    fh: i64,
    lc: i64,
    adopted: bool,
    closed: bool,
    head_gone: i64,
    carry: Option<Vec<String>>,
}

impl GuiWorld {
    fn new(generation: crate::fabric::BridgeGeneration) -> GuiWorld {
        let store = crate::session_store::new_store();
        let h = small_session(&store, 1);
        let model = aterm_models_session::log_mirror_model();
        let expect = model.init_state();
        let sid = h.sid.as_str().to_string();
        let mut w = GuiWorld {
            store,
            generation,
            h,
            sid,
            succ: None,
            pin: None,
            onbus: false,
            bus: Vec::new(),
            model,
            expect,
            e: [0, 0],
            fh: 0,
            lc: 0,
            adopted: true,
            closed: false,
            head_gone: 0,
            carry: None,
        };
        w.fire(&["Append"]);
        w.check("the session's open");
        w
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

    /// The store the bridge's verbs reach: the successor's once committed.
    fn store_now(&self) -> &crate::session_store::Store {
        self.succ.as_ref().map_or(&self.store, |(s, _)| s)
    }

    fn handle_now(&self) -> &crate::session_store::SessionHandle {
        self.succ.as_ref().map_or(&self.h, |(_, h)| h)
    }

    /// `f` of the session's log as the GUI holds it: live (or the successor's,
    /// registered or not), or its kept tail once closed; `None` once nothing
    /// of it is kept.
    fn source<R>(&self, f: impl FnOnce(&aterm_slog::Log) -> R) -> Option<R> {
        if self.closed {
            return self
                .store_now()
                .read()
                .unwrap()
                .mirror()
                .tail_view(&self.sid, f);
        }
        Some(f(self
            .handle_now()
            .ctx
            .timeline
            .lock()
            .unwrap()
            .slog()
            .log()))
    }

    fn view(&self) -> ([i64; 3], [i64; 3]) {
        let mut recs: std::collections::BTreeMap<u64, std::collections::BTreeSet<(u32, u64)>> =
            std::collections::BTreeMap::new();
        let mut cov = [0i64; 3];
        for body in &self.bus {
            let b = decode_bus(body).expect("a body");
            let ms = b.ms.unwrap_or(0);
            match b.rec.f.get_u(Key::N) {
                Some(n) if b.rec.op == aterm_slog::Op::Gap => {
                    for i in ms..ms + n {
                        if (1..=3).contains(&i) {
                            cov[usize::try_from(i - 1).unwrap()] = 1;
                        }
                    }
                }
                _ if b.rec.op != aterm_slog::Op::Snap && (1..=3).contains(&ms) => {
                    recs.entry(ms).or_default().insert((b.ep, b.rec.seq));
                }
                _ => {}
            }
        }
        let rec = [1u64, 2, 3].map(|i| recs.get(&i).map_or(0, |s| s.len() as i64));
        (rec, cov)
    }

    fn project(&mut self) -> aterm_spec::interp::State {
        let got = self.source(|log| {
            (
                i64::try_from(log.ms_head()).unwrap(),
                log.mirror_landed()
                    .map_or(0, |c| i64::try_from(c.ms).unwrap()),
                i64::from(log.read_fence().is_some()),
                log.live_ms().collect::<std::collections::BTreeSet<u64>>(),
            )
        });
        let (head, landed, fence) = match &got {
            Some((head, landed, fence, live)) => {
                if !self.closed {
                    for (i, ms) in [2u64, 3].iter().enumerate() {
                        let evicted =
                            i64::from(i64::try_from(*ms).unwrap() <= *head && !live.contains(ms));
                        assert_eq!(evicted, self.e[i], "the ring's ms {ms}");
                    }
                }
                (*head, *landed, *fence)
            }
            // Nothing kept: nothing more is owed of it.
            None => (self.head_gone, self.head_gone, 0),
        };
        let (pin, plo, phi, pgap, onbus) = match &self.pin {
            Some((_, body)) => {
                let b = decode_bus(body).expect("a pinned body");
                let lo = i64::try_from(b.ms.unwrap_or(0)).unwrap();
                let (hi, gap) = match b.rec.f.get_u(Key::N) {
                    Some(n) => (lo + i64::try_from(n).unwrap() - 1, 1),
                    None => (lo, 0),
                };
                (1, lo, hi, gap, i64::from(self.onbus))
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
        let mut s = aterm_spec::interp::State::new();
        for (k, v) in [
            ("head", head),
            ("e2", self.e[0]),
            ("e3", self.e[1]),
            ("landed", landed),
            ("pin", pin),
            ("plo", plo),
            ("phi", phi),
            ("pgap", pgap),
            ("onbus", onbus),
            ("foreign", 0),
            ("rec1", rec[0]),
            ("rec2", rec[1]),
            ("rec3", rec[2]),
            ("cov1", cov[0]),
            ("cov2", cov[1]),
            ("cov3", cov[2]),
            ("fence", fence),
            ("fh", self.fh),
            ("lc", self.lc),
            ("succ", i64::from(self.succ.is_some())),
            ("adopted", i64::from(self.adopted)),
            ("closed", i64::from(self.closed)),
        ] {
            s.insert(k, v);
        }
        s
    }

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

    fn breaks(&mut self, law: &str, what: &str) {
        let got = self.project();
        assert!(
            !self.model.check_invariant(law, &got),
            "{what}: the broken handler should break {law}: {got:?}"
        );
    }

    // ---- the real steps ------------------------------------------------

    fn append(&mut self) {
        seen(self.handle_now());
    }

    /// A local record (a bell): the ring's, never the bus's.
    fn bell(&mut self) {
        let mut tl = self.handle_now().ctx.timeline.lock().unwrap();
        let now = tl.slog().now();
        tl.slog_mut()
            .log_mut_for_tests()
            .mark(Mark::new(Kind::Bell, Src::Engine, Conf::Exact), now)
            .expect("a bell");
    }

    fn evict(&mut self, ms: u64) {
        evict(self.handle_now(), ms);
        self.e[usize::try_from(ms - 2).unwrap()] = 1;
    }

    /// One slot a take (`n=1`): a snapshot is a seed, put on the face as it
    /// comes, never pinned; the record or cover is the pin.
    fn take(&mut self) {
        let reply = cmd_log_take(self.store_now(), "n=1", Some(self.generation));
        if reply.starts_with("ERR busy") {
            return;
        }
        let (taken, _) = slots(&reply);
        for (sid, at, body) in taken {
            assert_eq!(sid, self.sid);
            if body.contains(" op=snap ") {
                self.bus.push(body);
            } else {
                assert!(self.pin.is_none(), "one slot a batch");
                self.pin = Some((at, body));
                self.onbus = false;
            }
        }
    }

    fn publish(&mut self) {
        if let Some((_, body)) = &self.pin
            && !self.onbus
        {
            self.bus.push(body.clone());
            self.onbus = true;
        }
    }

    /// `log-landed` the pin: the word; a settled one unpins.
    fn land(&mut self) -> String {
        let (at, _) = self.pin.clone().expect("a pin");
        let reply = cmd_log_landed(
            self.store_now(),
            &format!("{} {}", self.sid, at.word()),
            Some(self.generation),
        );
        let word = reply
            .strip_prefix("OK ")
            .map(str::trim)
            .unwrap_or_else(|| panic!("{reply}"))
            .to_string();
        if !matches!(word.as_str(), "fenced" | "adopting") {
            self.pin = None;
            self.onbus = false;
        }
        word
    }

    fn export(&mut self) {
        let (carry, fh, lc) = {
            let mut tl = self.h.ctx.timeline.lock().unwrap();
            let carry = tl
                .slog_carry_source()
                .prepare()
                .encode(aterm_slog::CarryBudget::SESSION)
                .expect("carries")
                .lines;
            let log = tl.slog().log();
            (
                carry,
                i64::try_from(log.ms_head()).unwrap(),
                log.mirror_landed()
                    .map_or(0, |c| i64::try_from(c.ms).unwrap()),
            )
        };
        self.carry = Some(carry);
        self.fh = fh;
        self.lc = lc;
    }

    /// The update stood down: the App lifts every live log's fence, and the
    /// store's closed tails' with the commit fence.
    fn rollback(&mut self) {
        self.h
            .ctx
            .timeline
            .lock()
            .unwrap()
            .slog_mut()
            .lift_read_fence();
        self.store.read().unwrap().mirror().lift_commit_fence();
    }

    /// The successor reads the carry; its store holds the session only from
    /// [`GuiWorld::adopt_late`], its adoption latch on until then.
    fn commit(&mut self) {
        let carry = self.carry.take().expect("an export");
        let (ep, last, ids) = {
            let tl = self.h.ctx.timeline.lock().unwrap();
            let log = tl.slog().log();
            (log.ep(), log.last_seq(), log.id_counters())
        };
        let at = aterm_slog::Stamp::new(1_760_000_100_000, 100_000);
        let (next, _, _) = aterm_slog::Log::from_carry(
            &carry,
            aterm_slog::CarryAdopt {
                ep: ep + 1,
                next_seq: last + 1000,
                ids,
                limits: aterm_slog::Limits::new(8, 1 << 20).expect("a ring"),
                at,
            },
        )
        .expect("the successor reads the carry");
        let mut h2 = test_handle(2);
        h2.sid = self.h.sid.clone();
        h2.ctx.timeline.lock().unwrap().slog_mut().replace_log(next);
        let succ = crate::session_store::new_store();
        succ.read().unwrap().mirror().set_adopting(true);
        self.succ = Some((succ, h2));
        self.adopted = false;
        self.e = [0, 0];
    }

    fn adopt_late(&mut self) {
        let (succ, h2) = self.succ.as_ref().expect("a successor");
        succ.write().unwrap().register(h2.clone());
        succ.read().unwrap().mirror().set_adopting(false);
        self.adopted = true;
    }

    fn close(&mut self) {
        let local = self.handle_now().local_id;
        self.store_now().write().unwrap().deregister_local_as(
            local,
            ExitReason::ShellExit,
            ExitActor::Unknown,
        );
        let head = self
            .handle_now()
            .ctx
            .timeline
            .lock()
            .unwrap()
            .slog()
            .log()
            .ms_head();
        self.head_gone = i64::try_from(head).unwrap();
        self.closed = true;
    }
}

/// THE CLOSED SESSION'S TAIL AGAINST THE MODEL: a session that closes under
/// an update's fence — its cursor short of the fence, and at it — keeps its
/// tail, its landings refused under the fence (a stutter), and once the
/// update stands down the tail lands whole: quiescent and complete.
#[test]
fn a_closed_sessions_tail_conforms_to_the_model() {
    crate::fabric::with_link_reset(|| {
        let generation = mirroring();
        // The cursor short of the fence.
        let mut w = GuiWorld::new(generation);
        w.append();
        w.fire(&["Append"]);
        w.take();
        w.publish();
        assert_eq!(w.land(), "ok");
        w.fire(&["TakeRecord", "Publish", "Landed", "Unpin"]);
        w.check("ms 1 landed");
        w.export();
        w.fire(&["Export"]);
        w.check("exported");
        w.close();
        w.fire(&["Close"]);
        w.check("closed under the fence, its tail kept");
        w.take();
        w.publish();
        w.fire(&["TakeRecord", "Publish"]);
        w.check("the record under the fence on the bus");
        assert_eq!(w.land(), "fenced");
        w.check("fenced: the pin stays (a stutter)");
        w.rollback();
        w.fire(&["Rollback"]);
        w.check("stood down: the tail's fence lifted");
        assert_eq!(w.land(), "ok");
        w.fire(&["Landed", "Unpin"]);
        w.check("landed");
        w.take();
        w.publish();
        assert_eq!(w.land(), "done");
        w.fire(&["TakeRecord", "Publish", "Landed", "Unpin"]);
        w.check("the close landed: quiescent and complete");
        assert_eq!(
            (w.expect["rec1"], w.expect["rec2"], w.expect["rec3"]),
            (1, 1, 1)
        );
        // The cursor AT the fence: everything up to it landed.
        let mut a = GuiWorld::new(generation);
        a.append();
        a.fire(&["Append"]);
        for _ in 0..2 {
            a.take();
            a.publish();
            assert_eq!(a.land(), "ok");
            a.fire(&["TakeRecord", "Publish", "Landed", "Unpin"]);
        }
        a.check("ms 1 and 2 landed");
        a.export();
        a.fire(&["Export"]);
        a.close();
        a.fire(&["Close"]);
        a.check("closed at the fence: its close is owed");
        a.rollback();
        a.fire(&["Rollback"]);
        a.take();
        a.publish();
        assert_eq!(a.land(), "done");
        a.fire(&["TakeRecord", "Publish", "Landed", "Unpin"]);
        a.check("the close landed");
    });
}

/// THE ADOPTION LATCH AGAINST THE MODEL (ruling Q1): a slot published under
/// the export's fence is refused its landing; the successor answers
/// `adopting` until it registers the session, then lands on the carried
/// cursor.
#[test]
fn the_gui_half_conforms_across_an_update_adopted_late() {
    crate::fabric::with_link_reset(|| {
        let generation = mirroring();
        let mut w = GuiWorld::new(generation);
        w.take();
        w.publish();
        assert_eq!(w.land(), "ok");
        w.fire(&["TakeRecord", "Publish", "Landed", "Unpin"]);
        w.append();
        w.fire(&["Append"]);
        w.export();
        w.fire(&["Export"]);
        w.take();
        w.publish();
        w.fire(&["TakeRecord", "Publish"]);
        assert_eq!(w.land(), "fenced");
        w.check("fenced: the pin stays");
        w.commit();
        // The successor's own `mark - handoff` is its first record.
        w.fire(&["Commit", "Append"]);
        w.check("committed, not adopted");
        assert_eq!(w.land(), "adopting");
        w.check("adopting: the pin stays (a stutter)");
        w.adopt_late();
        w.fire(&["AdoptLate"]);
        assert_eq!(w.land(), "ok");
        w.fire(&["Landed", "Unpin"]);
        w.check("landed on the carried cursor");
        w.take();
        w.publish();
        assert_eq!(w.land(), "ok");
        w.fire(&["TakeRecord", "Publish", "Landed", "Unpin"]);
        w.check("the handoff mark landed: quiescent and complete");
    });
}

/// NEGATIVE CONTROLS: each GUI-half mutant's witness replayed through the
/// broken handler it stands for, and the projected law breaks.
#[test]
fn the_gui_half_conformance_rejects_each_mutant_through_its_broken_handler() {
    crate::fabric::with_link_reset(|| {
        let generation = mirroring();
        // CloseDropsTail: a closing session with an unlanded record keeps no
        // tail; nothing more is owed of it, and its close never reaches the bus.
        let mut c = GuiWorld::new(generation);
        c.append();
        c.take();
        c.publish();
        assert_eq!(c.land(), "ok");
        c.store
            .read()
            .unwrap()
            .mirror()
            .set_broken(Broken::DropTail);
        c.close();
        c.breaks("QuiescentMirrorComplete", "CloseDropsTail");

        // DropUnadopted: `gone` for a session the successor has not adopted;
        // the bridge drops its pin; the successor's carried cursor is behind
        // it and covers what its ring evicted since.
        let mut d = GuiWorld::new(generation);
        d.take();
        d.publish();
        assert_eq!(d.land(), "ok");
        d.append();
        // A bell after ms 2, so the successor's ring evicts ms 2 alone.
        d.bell();
        d.export();
        d.take();
        d.publish();
        assert_eq!(d.land(), "fenced");
        d.commit();
        d.store_now()
            .read()
            .unwrap()
            .mirror()
            .set_broken(Broken::GoneWhileAdopting);
        assert_eq!(d.land(), "gone", "the broken handler");
        d.evict(2);
        d.adopt_late();
        d.take();
        d.publish();
        d.breaks("ReaderViewAtMostOnce", "DropUnadopted");

        // LandUnderFence: a landing answered while the export's cursor stands.
        let mut f = GuiWorld::new(generation);
        f.append();
        f.export();
        f.take();
        f.publish();
        f.store
            .read()
            .unwrap()
            .mirror()
            .set_broken(Broken::LandUnderFence);
        assert_eq!(f.land(), "ok", "the broken handler");
        f.breaks("NoLandUnderFence", "LandUnderFence");
        // CONTROL: the real handler refuses the same landing.
        let mut real = GuiWorld::new(generation);
        real.append();
        real.export();
        real.take();
        real.publish();
        assert_eq!(real.land(), "fenced");
    });
}
