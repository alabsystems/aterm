// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! ROUND SEVEN OF THE UPDATE AUDIT, FINDINGS 6, 8 AND 28: what a seamless
//! update carries of a session's mailbox beside its topics and hold
//! ([`FabricCarry`]), driven through the real manifest — `from_store`, TOML
//! out and back, and the spawn's own seed.

use super::*;
use crate::SessionCtx;
use crate::session_store::{SessionHandoff, Store, new_store, test_handle};

use super::with_link_reset as with_link;

/// A registered session with process-local id `local_id`: the real
/// `SessionHandle` the store builds.
fn registered(store: &Store, local_id: u64) -> (String, std::sync::Arc<SessionCtx>) {
    let h = test_handle(local_id);
    let sid = h.sid.as_str().to_string();
    let ctx = h.ctx.clone();
    store.write().unwrap_or_else(|p| p.into_inner()).register(h);
    (sid, ctx)
}

/// An addressed delivery from the bridge.
fn deliver(store: &Store, sid: &str, off: u64, from: &str, kind: &str, text: &str) -> String {
    cmd_deliver(
        store,
        &format!("{sid} off={off} from={from} kind={kind} trust=agent text={text}"),
    )
}

/// A broadcast delivery on `topic`, as the bridge sends one to a subscribed
/// session.
fn broadcast(store: &Store, sid: &str, off: u64, topic: &str, text: &str) -> String {
    cmd_deliver(
        store,
        &format!(
            "{sid} off={off} from=s-peer@n-lab kind=note trust=agent topic={topic} text={text}"
        ),
    )
}

/// The outgoing store drawn into a manifest and read back as the
/// successor reads it, each record seeded onto a FRESH session through
/// the spawn's seam and registered in a fresh store.
fn hand_over(store: &Store) -> (Store, Vec<(String, std::sync::Arc<SessionCtx>)>) {
    let manifest = SessionHandoff::from_store(&store.read().unwrap());
    let wire = manifest.to_toml().expect("serializes");
    assert!(manifest.roundtrips(), "{wire}");
    let read = SessionHandoff::from_toml(&wire).expect("the successor reads it");
    let successor = new_store();
    let mut adopted = Vec::new();
    for rec in &read.sessions {
        let h = test_handle(rec.local_id);
        crate::spawn::seed_adopted_fabric(
            &h.ctx.fabric,
            &h.ctx.timeline,
            &rec.topics,
            &rec.hold_rows(),
            &rec.fabric,
        );
        adopted.push((h.sid.as_str().to_string(), h.ctx.clone()));
        successor.write().unwrap().register(h);
    }
    (successor, adopted)
}

/// **`dropped=` SURVIVES A SEAMLESS UPDATE, AND THE UNREAD BROADCASTS THE
/// UPDATE DROPS ARE COUNTED IN IT AND LOGGED.** The ring is not carried,
/// and under a bridge that does not refill broadcasts (no `hello`, as
/// here) every broadcast a session held unhandled at the draw is gone from
/// the successor's inbox; the counter is monotone so that a loss is
/// visible to the next drain, and the successor used to start it at 0 —
/// erasing the update's own loss and every drop counted before it.
/// NEGATIVE CONTROLS: a session that handled its broadcasts before the
/// draw keeps only its earlier drops, and an unhandled ADDRESSED row
/// (refilled from the floor) is never counted. The refilling bridge's
/// case is the next test.
///
/// FAILS WITHOUT THE FIX: both successors report `dropped=0`.
#[test]
fn a_seamless_update_keeps_dropped_and_counts_the_unread_broadcasts_it_drops() {
    crate::test_log::install();
    with_link(|| {
        let store = new_store();
        let (sid, ctx) = registered(&store, 1);
        let (bsid, bctx) = registered(&store, 2);
        for (s, c) in [(&sid, &ctx), (&bsid, &bctx)] {
            assert_eq!(c.fabric.topic_add("news", "head", &c.timeline), Ok(true));
            c.fabric.with_inbox_for_test(|i| i.dropped = 2);
            for off in 10..13 {
                assert!(broadcast(&store, s, off, "news", "b").starts_with("OK "));
            }
            assert_eq!(
                deliver(&store, s, 20, "h-andrew", "task", "t"),
                "OK 4\n",
                "an addressed row is refilled, so it is not counted"
            );
        }
        // Session 2 handled all three broadcasts before the draw; session 1
        // handled none of them.
        assert_eq!(cmd_inbox_seen(&bctx, "3"), "OK seen=3\n");

        let (_successor, adopted) = hand_over(&store);
        let header = |c: &SessionCtx| {
            cmd_inbox(c, "--peek")
                .lines()
                .next()
                .unwrap_or_default()
                .to_string()
        };
        let (_, unread) = &adopted[0];
        let (_, handled) = &adopted[1];
        assert!(
            header(unread).contains(" dropped=5 "),
            "the two drops counted before the update, and the three unread \
             broadcasts it could not carry: {}",
            header(unread)
        );
        assert!(
            header(handled).contains(" dropped=2 "),
            "handled broadcasts are not lost, and the earlier drops are kept: {}",
            header(handled)
        );
        let said = crate::test_log::containing(
            "3 broadcast(s) this session had not handled when the update was drawn",
        );
        assert!(
            said.iter().any(|n| n.level == aterm_log::Level::Warn),
            "the successor says what the update dropped: {said:?}"
        );
    });
}

/// **A BROADCAST ITS BRIDGE OFFERS AGAIN IS NOT COUNTED LOST.** Under a
/// bridge that said `refill=…say`, the successor's bridge rewinds each
/// topic to its oldest unhandled broadcast and offers it again, so
/// counting it in `dropped=` as well would report a loss that did not
/// happen. The carry says both numbers, so a successor from before the
/// refill (whose own bridge refills nothing) still counts every one.
/// NEGATIVE CONTROLS, each counted: a row a bridge from before the key
/// delivered (its floor was never written), and a row on a topic the
/// session dropped before the draw (a successor refuses it
/// `ERR not-subscribed`). A carry from an older aterm, with no
/// `broadcasts_refilled`, counts all of them.
///
/// FAILS WITHOUT THE FIX: the successor reports `dropped=6`, every unread
/// broadcast counted though four come back.
#[test]
fn a_seamless_update_does_not_count_the_broadcasts_its_bridge_offers_again() {
    crate::test_log::install();
    with_link(|| {
        let store = new_store();
        let (sid, ctx) = registered(&store, 1);
        for t in ["news", "gone"] {
            assert_eq!(ctx.fabric.topic_add(t, "head", &ctx.timeline), Ok(true));
        }
        ctx.fabric.with_inbox_for_test(|i| i.dropped = 2);
        // Delivered by a bridge from before `refill=`.
        assert_eq!(broadcast(&store, &sid, 10, "news", "b"), "OK 1\n");
        let generation = next_bridge_generation();
        bridge_attached(generation);
        assert!(cmd_hello("lane=2 refill=in,say", Some(generation)).starts_with("OK "));
        for off in 11..15 {
            assert!(broadcast(&store, &sid, off, "news", "b").starts_with("OK "));
        }
        assert_eq!(broadcast(&store, &sid, 15, "gone", "b"), "OK 6\n");
        assert!(ctx.fabric.topic_drop("gone", &ctx.timeline));

        let mut budget = CARRY_MAIL_BYTES;
        let carry = ctx.fabric.handoff_carry(&mut budget);
        assert_eq!(
            (carry.broadcasts_uncarried, carry.broadcasts_refilled),
            (6, 4)
        );

        let (_successor, adopted) = hand_over(&store);
        let (_, kept) = &adopted[0];
        let header = cmd_inbox(kept, "--peek")
            .lines()
            .next()
            .unwrap_or_default()
            .to_string();
        assert!(
            header.contains(" dropped=4 "),
            "the two earlier drops and the two no bridge offers again: {header}"
        );
        assert!(
            crate::test_log::containing("2 broadcast(s) this session had not handled")
                .iter()
                .any(|n| n.level == aterm_log::Level::Warn),
            "the loss is said"
        );
        assert!(
            !crate::test_log::containing("are offered again by its bridge").is_empty(),
            "and so is what comes back"
        );

        // An older aterm's carry: no `broadcasts_refilled`.
        let older = FabricCarry {
            broadcasts_refilled: 0,
            ..carry
        };
        let fresh = new_store();
        let (_, old_ctx) = registered(&fresh, 9);
        old_ctx.fabric.seed_carry(&older, &old_ctx.timeline);
        assert_eq!(old_ctx.fabric.with_inbox_for_test(|i| i.dropped), 2 + 6);
    });
}

/// **A BROADCAST ITS REFILLING BRIDGE HAS LET GO OF IS COUNTED LOST.** Two
/// ways that bridge stops holding the refill floor of a row this ring
/// still holds unhandled, and no successor is offered it: a `topic drop`
/// takes the topic's floor with it (aterm-link's
/// `Bridge::forget_unhandled_topic`) — and a `topic add` of the same name
/// resumes from a new cursor, so the topic being held again at the draw
/// says nothing about the old rows; and the bridge remembers a session's
/// newest [`RING_CAP`] unhandled broadcasts only (`UNHANDLED_KEEP`), so a
/// person's broadcast the ring keeps ahead of that many newer agent ones
/// (evicting those instead) is gone from the floor. Each is counted in
/// `dropped=`, not carried. NEGATIVE CONTROLS: the broadcast taken after
/// the re-add is carried, and the person's is carried while fewer than
/// [`RING_CAP`] rows came after it — the bridge still holds it then.
///
/// FAILS WITHOUT THE FIX: both are counted as offered again, so the
/// successor's `dropped=` misses them.
#[test]
fn a_broadcast_its_refilling_bridge_let_go_of_is_counted_lost() {
    with_link(|| {
        let store = new_store();
        let (sid, ctx) = registered(&store, 1);
        let (psid, pctx) = registered(&store, 2);
        let generation = next_bridge_generation();
        bridge_attached(generation);
        assert!(cmd_hello("lane=2 refill=in,say", Some(generation)).starts_with("OK "));
        let carried = |c: &SessionCtx| {
            let mut budget = CARRY_MAIL_BYTES;
            let carry = c.fabric.handoff_carry(&mut budget);
            (carry.broadcasts_uncarried, carry.broadcasts_refilled)
        };

        // DROPPED AND ADDED AGAIN, through the verb.
        assert!(cmd_topic(&ctx, "add news").0.contains("added=1"));
        assert_eq!(broadcast(&store, &sid, 10, "news", "b"), "OK 1\n");
        assert!(cmd_topic(&ctx, "drop news").0.contains("dropped=1"));
        assert!(cmd_topic(&ctx, "add news").0.contains("added=1"));
        assert_eq!(broadcast(&store, &sid, 11, "news", "b"), "OK 2\n");
        assert_eq!(carried(&ctx), (2, 1), "only the row taken since the add");

        // A PERSON'S BROADCAST, then agents' from distinct peers (one
        // quota each): the ring evicts the agents' first.
        assert!(cmd_topic(&pctx, "add news").0.contains("added=1"));
        let shout = |off: u64, from: &str| {
            cmd_deliver(
                &store,
                &format!("{psid} off={off} from={from} kind=note trust=agent topic=news text=b"),
            )
        };
        assert_eq!(shout(20, "h-andrew"), "OK 1\n");
        let cap = u64::try_from(RING_CAP).unwrap();
        for i in 1..cap {
            assert!(shout(100 + i, &format!("s-x@n-p{i}")).starts_with("OK "));
        }
        assert_eq!(
            carried(&pctx),
            (cap, cap),
            "fewer than the bound came after it"
        );
        assert!(shout(100 + cap, &format!("s-x@n-p{cap}")).starts_with("OK "));
        assert!(
            pctx.fabric
                .with_inbox_for_test(|i| i.rows.iter().any(|r| r.id == 1)),
            "the ring keeps the person's row"
        );
        assert_eq!(
            carried(&pctx),
            (cap, cap - 1),
            "the bridge's bound let it go: counted lost"
        );
    });
}

/// FINDING 6: a post still queued and a receipt still owed at the park are
/// on the SUCCESSOR's `outbox` peek — its bridge drains them — under the
/// ids the old process gave them. FINDINGS 8 AND 28: every id the
/// successor hands out after that is above the old process's, so a
/// pre-update anchor or sequence pin never names a new row, post or
/// receipt. NEGATIVE CONTROL: a post the bridge already retired is not
/// carried (it would go on the bus twice).
///
/// FAILS WITHOUT THE FIX: the successor's outbox is empty and its ids
/// start over at 1 (measured: `seed_carry` a no-op leaves `outbox` at
/// `OK 0`).
#[test]
fn a_seamless_update_carries_the_outbox_the_receipts_and_the_ids() {
    with_link(|| {
        let store = new_store();
        let (sid, ctx) = registered(&store, 1);
        let generation = next_bridge_generation();
        bridge_attached(generation);
        assert_eq!(
            cmd_link(
                &store,
                &format!("log {}.0", "aa".repeat(16)),
                Some(generation)
            ),
            "OK\n"
        );
        assert_eq!(
            deliver(&store, &sid, 10, "s-peer@n-lab", "ask", "one"),
            "OK 1\n"
        );
        assert_eq!(
            deliver(&store, &sid, 11, "h-andrew", "task", "two"),
            "OK 2\n"
        );
        // No bridge: both posts queue. The first is published and retired
        // before the park; the second waits.
        assert_eq!(cmd_post(&ctx, "to=@s-peer kind=note gone", None), "OK 1\n");
        assert_eq!(
            cmd_post(&ctx, "to=@s-peer kind=note waiting", None),
            "OK 2\n"
        );
        assert_eq!(
            cmd_outbox_verb(&store, &format!("sent {sid} 1 off=70")),
            "OK\n"
        );
        // A verdict on the ask owes its sender a receipt.
        assert_eq!(cmd_inbox_seen(&ctx, "1 handled"), "OK seen=1\n");

        let (successor, adopted) = hand_over(&store);
        let (new_sid, new_ctx) = &adopted[0];
        let drain = cmd_outbox(&successor, "");
        assert!(
            drain.contains(&format!(
                "post sid={new_sid} id=2 to=@s-peer kind=note len=7\nwaiting"
            )),
            "the queued post is on the successor's drain: {drain}"
        );
        assert!(
            !drain.contains(" id=1 "),
            "the retired post is not: {drain}"
        );
        assert!(
            drain.contains(&format!(
                "receipt sid={new_sid} rid=1 off=10 verdict=handled kind=ask from=s-peer@n-lab"
            )),
            "the owed receipt is on the successor's drain: {drain}"
        );
        // The ids go on above the old process's — the inbox's above its
        // overlap reserve too ([`INBOX_ID_RESERVE`]).
        let next_row = 2 + INBOX_ID_RESERVE + 1;
        assert_eq!(
            deliver(&successor, new_sid, 12, "h-andrew", "task", "three"),
            format!("OK {next_row}\n")
        );
        assert_eq!(
            cmd_post(new_ctx, "to=@s-peer kind=note next", None),
            "OK 3\n"
        );
        assert_eq!(
            cmd_inbox_seen(new_ctx, &format!("{next_row} handled")),
            format!("OK seen={next_row}\n")
        );
        assert!(
            cmd_outbox(&successor, "").contains("rid=2 off=12"),
            "the next receipt is rid 2, never a second rid 1"
        );
        // An old id the successor never minted names nothing.
        assert_eq!(
            cmd_inbox_seen(new_ctx, "1 handled"),
            "ERR no such message\n"
        );
    });
}

/// **A RECEIPT FOR A ROW OF AN EARLIER LOG IS LISTED AS ONE, AND STAYS
/// ONE ACROSS AN UPDATE** (r2-01's residual). Told log A, two asks land at
/// 10 and 11 and the first is decided. Told log B, both of A's rows'
/// receipts — the one owed at the switch and the one a verdict on A's
/// second row owes after it — are listed `log=earlier`, which the bridge
/// retires rather than put `re=` on B; an ask B delivers at A's offset 10
/// owes a plain receipt. The successor lists each as the old process did.
/// Negative control, run: without the mark all three are listed as B's.
#[test]
fn a_receipt_for_a_row_of_an_earlier_log_is_listed_as_one_across_an_update() {
    with_link(|| {
        let store = new_store();
        let (sid, ctx) = registered(&store, 1);
        let generation = next_bridge_generation();
        bridge_attached(generation);
        let tell = |ident: String| cmd_link(&store, &format!("log {ident}"), Some(generation));
        assert_eq!(tell(format!("{}.0", "aa".repeat(16))), "OK\n");
        for (off, text, row) in [(10, "a0", "OK 1\n"), (11, "a1", "OK 2\n")] {
            assert_eq!(deliver(&store, &sid, off, "s-peer@n-lab", "ask", text), row);
        }
        assert_eq!(cmd_inbox_seen(&ctx, "1 handled"), "OK seen=1\n");
        assert_eq!(tell(format!("{}.0", "bb".repeat(16))), "OK forgot=1\n");
        assert_eq!(cmd_inbox_seen(&ctx, "2 deferred"), "OK seen=2\n");
        assert_eq!(
            deliver(&store, &sid, 10, "s-peer@n-lab", "ask", "b0"),
            "OK 3\n"
        );
        assert_eq!(cmd_inbox_seen(&ctx, "3 handled"), "OK seen=3\n");
        let receipts = |store: &Store| -> Vec<String> {
            cmd_outbox(store, "earlier-receipts=1")
                .lines()
                .filter(|l| l.starts_with("receipt "))
                .map(str::to_string)
                .collect()
        };
        let want = |sid: &str| {
            [
                format!(
                    "receipt sid={sid} rid=1 off=10 verdict=handled kind=ask \
                     from=s-peer@n-lab log=earlier"
                ),
                format!(
                    "receipt sid={sid} rid=2 off=11 verdict=deferred kind=ask \
                     from=s-peer@n-lab log=earlier"
                ),
                format!(
                    "receipt sid={sid} rid=3 off=10 verdict=handled kind=ask from=s-peer@n-lab"
                ),
            ]
        };
        assert_eq!(receipts(&store), want(&sid));
        let legacy_drain = cmd_outbox(&store, "");
        assert!(!legacy_drain.contains("rid=1 "), "{legacy_drain}");
        assert!(!legacy_drain.contains("rid=2 "), "{legacy_drain}");
        assert!(legacy_drain.contains("rid=3 "), "{legacy_drain}");

        let carry = ctx.fabric.handoff_carry(&mut CARRY_MAIL_BYTES.clone());
        assert_eq!((carry.receipts.len(), carry.earlier_receipts.len()), (1, 2));
        #[derive(serde::Deserialize)]
        struct LegacyCarry {
            #[serde(default)]
            receipts: Vec<ReceiptCarry>,
        }
        let wire = aterm_toml::to_string(&carry).expect("outgoing carry");
        let legacy: LegacyCarry = aterm_toml::from_str(&wire).expect("older reader");
        assert_eq!(
            legacy.receipts.len(),
            1,
            "old receipts cannot lose their provenance"
        );
        assert_eq!(legacy.receipts[0].rid, 3);
        // Both lists spend one shared budget, and both are counted on shed.
        let mut budget = carry.receipts[0].cost();
        let mut limited = ctx.fabric.handoff_carry(&mut budget);
        assert_eq!(limited.receipts.len() + limited.earlier_receipts.len(), 1);
        assert_eq!(limited.receipts_uncarried, 2);
        assert_eq!(budget, 0);
        assert!(limited.shed());
        assert_eq!(limited.rows_cost(), 0);
        assert_eq!(limited.receipts_uncarried, 3);

        let (successor, adopted) = hand_over(&store);
        assert_eq!(receipts(&successor), want(&adopted[0].0));
    });
}

#[test]
fn unknown_carried_receipts_stay_fenced_before_the_first_log_report() {
    with_link(|| {
        let store = new_store();
        let (_, ctx) = registered(&store, 1);
        let carry = FabricCarry {
            last_receipt_id: 1,
            receipts: vec![ReceiptCarry {
                rid: 1,
                off: 7,
                verdict: "handled".to_string(),
                kind: "ask".to_string(),
                from: "s-peer@n-lab".to_string(),
            }],
            ..FabricCarry::default()
        };
        ctx.fabric.seed_carry(&carry, &ctx.timeline);
        assert!(!cmd_outbox(&store, "").contains("receipt "));
        assert!(cmd_outbox(&store, "earlier-receipts=1").contains(" log=earlier\n"));
        let carried = ctx.fabric.handoff_carry(&mut CARRY_MAIL_BYTES.clone());
        assert!(carried.receipts.is_empty());
        assert_eq!(carried.earlier_receipts.len(), 1);
    });
}

/// A PTY KEEPER RECOVERY IS NOT A FRESH MAILBOX (the fabric audit's fourth
/// round, findings 2-5). A session the keeper kept through a crash keeps
/// its sid and its agent, and comes back from its META's fabric: HELD
/// again before it answers a verb, its broadcast opt-in listed (so the new
/// bridge keeps the topic's cursor), every id above the dead window's (an
/// old inbox id names no row, and no post or receipt meets a sequence pin
/// the crash left), and each post it was told is queued retired dead
/// `not-carried`, said on its timeline. NEGATIVE CONTROL: a META without
/// the fabric (an older window's) carries no hold or topic, and its
/// counters are fenced far above any window's, not restarted.
///
/// FAILS WITHOUT THE FIX: the recovery's `Adopted` held `hold: None`,
/// `topics: []` and an empty carry — `send` went through, `topic ls` read
/// `OK 0` and the next post was id 1.
#[cfg(unix)]
#[test]
fn a_keeper_recovery_holds_resubscribes_and_counts_above_the_dead_window() {
    with_link(|| {
        let store = new_store();
        let (sid, ctx) = registered(&store, 1);
        deliver(&store, &sid, 10, "s-peer@n-lab", "ask", "one");
        deliver(&store, &sid, 11, "h-andrew", "task", "two");
        for (n, text) in [(1, "landed"), (2, "queued"), (3, "queued-too")] {
            assert_eq!(
                cmd_post(&ctx, &format!("to=@s-peer kind=note {text}"), None),
                format!("OK {n}\n")
            );
        }
        assert_eq!(
            cmd_outbox_verb(&store, &format!("sent {sid} 1 off=70")),
            "OK\n"
        );
        assert_eq!(cmd_inbox_seen(&ctx, "1 handled"), "OK seen=1\n");
        assert_eq!(
            ctx.fabric.topic_add("builds", "@5", &ctx.timeline),
            Ok(true)
        );
        let stop = Hold {
            reason: "stop%20driving".to_string(),
            origin: "local".to_string(),
        };
        assert!(apply_hold_for_test(&ctx, Some(stop.clone())));
        // …and the fleet's halt over it, which stands beside it.
        let halt = Hold {
            reason: "main%20broken".to_string(),
            origin: "fleet".to_string(),
        };
        assert!(apply_hold_for_test(&ctx, Some(halt.clone())));

        let screen = aterm_core::terminal::Terminal::new(24, 80).carry_meta_abandoning_partial();
        let carry = ctx.fabric.keeper_carry();
        assert_eq!(
            (carry.hold.as_deref(), carry.more_holds.as_slice()),
            (
                Some("fleet main%20broken"),
                &["local stop%20driving".to_string()][..]
            ),
            "the fleet's first, where an older reader looks"
        );
        let meta = crate::seamless::keeper_meta_of(screen.clone(), Some(&carry)).expect("a META");
        let recovered = crate::seamless::recovered_fabric(&meta);
        let successor = new_store();
        let fresh = test_handle(1);
        let new_sid = fresh.sid.as_str().to_string();
        crate::spawn::seed_adopted_fabric(
            &fresh.ctx.fabric,
            &fresh.ctx.timeline,
            &recovered.topics,
            &recovered.holds,
            &recovered.carry,
        );
        let new_ctx = fresh.ctx.clone();
        successor.write().unwrap().register(fresh);

        assert_eq!(
            new_ctx.fabric.holds().standing().collect::<Vec<_>>(),
            [halt, stop.clone()],
            "held again, both origins"
        );
        assert!(halt_refusal(&new_ctx, "send").is_some(), "send is refused");
        assert_eq!(
            cmd_topic(&new_ctx, "ls").0,
            "OK 1 seen=0\ntopic builds since=@5\n",
            "the opt-in is listed, so the bridge keeps its cursor"
        );
        let rows: Vec<(String, String)> = new_ctx
            .timeline
            .lock()
            .unwrap()
            .since(None)
            .map(|e| (e.kind.to_string(), e.payload.clone()))
            .collect();
        for id in [2, 3] {
            assert!(
                rows.contains(&(
                    "post-landed".to_string(),
                    format!("{id} off=- reason={NOT_CARRIED}")
                )),
                "queued post {id} is retired dead, said: {rows:?}"
            );
        }
        assert!(
            !rows
                .iter()
                .any(|(_, payload)| payload.starts_with("1 off=")),
            "the landed post is not: {rows:?}"
        );
        // Every id is above the dead window's: the next of each kind
        // starts past the lease.
        let lease = 2 * INBOX_ID_RESERVE;
        assert_eq!(
            cmd_inbox_seen(&new_ctx, "2 handled"),
            "ERR no such message\n",
            "an id the agent learned before the crash names no row"
        );
        assert_eq!(
            deliver(&successor, &new_sid, 12, "s-peer@n-lab", "ask", "three"),
            format!("OK {}\n", lease + 1)
        );
        assert_eq!(
            cmd_post(&new_ctx, "to=@s-peer kind=note next", None),
            format!("OK {}\n", lease + 1)
        );
        assert_eq!(
            cmd_inbox_seen(&new_ctx, &format!("{} handled", lease + 1)),
            format!("OK seen={}\n", lease + 1)
        );
        assert!(
            cmd_outbox(&successor, "").contains(&format!("rid={} off=12", lease + 1)),
            "the next receipt is past the lease"
        );

        // NEGATIVE CONTROL: the same screen, no fabric.
        let bare = crate::seamless::keeper_meta_of(screen, None).expect("a META");
        let recovered = crate::seamless::recovered_fabric(&bare);
        assert_eq!((recovered.holds, recovered.topics.len()), (Vec::new(), 0));
        let fence = 1 << 32;
        assert!(
            recovered.carry.last_msg_id >= fence
                && recovered.carry.last_post_id >= fence
                && recovered.carry.last_receipt_id >= fence
                && recovered.carry.uncarried.is_empty(),
            "fenced, not restarted: {:?}",
            recovered.carry
        );
        // A fabric no window writes is read as a halt, never as none.
        let unreadable = Hold {
            reason: CARRIED_UNREADABLE.to_string(),
            origin: "local".to_string(),
        };
        for garbled in [&br#"{"fabric":{"hold":7}}"#[..], br#"{"fabric":[]}"#] {
            let holds = crate::seamless::recovered_fabric(garbled).holds;
            assert_eq!(
                parse_holds(holds.iter().map(String::as_str))
                    .standing()
                    .collect::<Vec<_>>(),
                std::slice::from_ref(&unreadable)
            );
        }
        // So is a `more_holds` that does not read, beside a fleet hold
        // that does: both stand.
        for garbled in [
            &br#"{"fabric":{"hold":"fleet x","more_holds":7}}"#[..],
            br#"{"fabric":{"hold":"fleet x","more_holds":[7]}}"#,
        ] {
            let holds = crate::seamless::recovered_fabric(garbled).holds;
            let fleet = Hold {
                reason: "x".to_string(),
                origin: "fleet".to_string(),
            };
            assert_eq!(
                parse_holds(holds.iter().map(String::as_str))
                    .standing()
                    .collect::<Vec<_>>(),
                [fleet, unreadable.clone()]
            );
        }
        // A META from before the two slots carries `hold` alone: exactly
        // that hold, of that origin.
        let holds = crate::seamless::recovered_fabric(
            br#"{"fabric":{"hold":"local stop%20driving","ids":[1,1,1]}}"#,
        )
        .holds;
        assert_eq!(
            parse_holds(holds.iter().map(String::as_str))
                .standing()
                .collect::<Vec<_>>(),
            [stop]
        );
    });
}

/// `dropped=` GOES ON COUNTING ACROSS A PTY KEEPER RECOVERY (audit r3-07,
/// the keeper half), as it does across an update: the META carries the
/// session's count and the recovered session's `inbox` header reads it,
/// then counts on from it. NEGATIVE CONTROL: the same session's META from
/// a window before the count (no `dropped` member) reads `dropped=0`, which
/// is what every recovery read before — the reader, not the default, is
/// what carries it. FAILS WITHOUT THE FIX: the carry had no such member and
/// the recovered header read `dropped=0`. AND THE COUNT IS BOUNDED as the
/// writer bounds it: a META naming more than `i64::MAX` (no window writes
/// one; the reader is lenient) recovers at `i64::MAX`, from which the
/// ring's next eviction counts on, where `u64::MAX` overflowed it.
#[cfg(unix)]
#[test]
fn a_keeper_recovery_goes_on_counting_dropped() {
    with_link(|| {
        let store = new_store();
        let (_sid, ctx) = registered(&store, 1);
        ctx.fabric.with_inbox_for_test(|i| i.dropped = 7);
        let carry = ctx.fabric.keeper_carry();
        assert_eq!(carry.dropped, 7);
        let screen = aterm_core::terminal::Terminal::new(24, 80).carry_meta_abandoning_partial();
        let meta = crate::seamless::keeper_meta_of(screen.clone(), Some(&carry)).expect("a META");
        let older = crate::seamless::keeper_meta_of(
            screen,
            Some(&KeeperCarry {
                dropped: 0,
                ..carry.clone()
            }),
        )
        .expect("a META");
        assert!(
            !String::from_utf8_lossy(&older).contains("\"dropped\""),
            "PRECONDITION: a zero count writes no member, as an older window's META"
        );
        let recover = |meta: &[u8]| {
            let recovered = crate::seamless::recovered_fabric(meta);
            let fresh = test_handle(1);
            crate::spawn::seed_adopted_fabric(
                &fresh.ctx.fabric,
                &fresh.ctx.timeline,
                &recovered.topics,
                &recovered.holds,
                &recovered.carry,
            );
            let ctx = fresh.ctx.clone();
            let sid = fresh.sid.as_str().to_string();
            let successor = new_store();
            successor.write().unwrap().register(fresh);
            (successor, ctx, sid)
        };
        let (_store, recovered, _) = recover(&meta);
        let header = cmd_inbox(&recovered, "--peek");
        assert!(header.contains(" dropped=7 "), "{header}");
        // NEGATIVE CONTROL: the META without the member.
        let (_store, older, _) = recover(&older);
        let header = cmd_inbox(&older, "--peek");
        assert!(header.contains(" dropped=0 "), "{header}");
        // PAST THE BOUND: the count past any carry's `i64::MAX`.
        let past = crate::seamless::keeper_meta_of(
            aterm_core::terminal::Terminal::new(24, 80).carry_meta_abandoning_partial(),
            Some(&KeeperCarry {
                dropped: u64::MAX,
                ..carry
            }),
        )
        .expect("a META");
        assert!(
            String::from_utf8_lossy(&past).contains(&format!("\"dropped\":{}", u64::MAX)),
            "PRECONDITION: the META names u64::MAX"
        );
        let (store, past, sid) = recover(&past);
        let header = cmd_inbox(&past, "--peek");
        assert!(
            header.contains(&format!(" dropped={} ", i64::MAX)),
            "{header}"
        );
        // And the ring's next eviction counts on from it. One sender per
        // row keeps the per-sender quota out of it.
        for off in 1..=u64::try_from(RING_CAP).expect("cap fits") + 1 {
            let from = format!("a-svc{off}");
            assert!(deliver(&store, &sid, off, &from, "note", "x").starts_with("OK "));
        }
        let header = cmd_inbox(&past, "--peek");
        assert!(
            header.contains(&format!(" dropped={} ", i64::MAX.unsigned_abs() + 1)),
            "{header}"
        );
    });
}

/// THE COMMIT SEES THE OUTBOX MOVE, and a move past its reading WAITS: a
/// post queued, a receipt owed or a post retired after the park changes
/// [`SessionFabric::carry_serial`] (the Commit then stands down, a lossless
/// retry), and under the fence ([`SessionFabric::fence_hold`]) a post is
/// not answered until the attempt stands down — never `OK` for a row the
/// successor would not carry. NEGATIVE CONTROL: a read moves nothing.
#[cfg(unix)] // the update fence (`lift_hold_fence`) is unix-only
#[test]
fn the_commit_sees_the_outbox_move_and_a_post_waits_on_its_fence() {
    with_link(|| {
        let store = new_store();
        let (sid, ctx) = registered(&store, 1);
        deliver(&store, &sid, 10, "s-peer@n-lab", "ask", "one");
        let parked = ctx.fabric.carry_serial();
        let _ = cmd_inbox(&ctx, "");
        assert_eq!(ctx.fabric.carry_serial(), parked, "a read moves nothing");
        assert_eq!(cmd_post(&ctx, "to=@s-peer kind=note one", None), "OK 1\n");
        let after_post = ctx.fabric.carry_serial();
        assert_ne!(after_post, parked, "a queued post moves it");
        assert_eq!(cmd_inbox_seen(&ctx, "1 deferred"), "OK seen=1\n");
        let after_receipt = ctx.fabric.carry_serial();
        assert_ne!(after_receipt, after_post, "an owed receipt moves it");
        assert_eq!(
            cmd_outbox_verb(&store, &format!("sent {sid} 1 off=70")),
            "OK\n"
        );
        assert_ne!(
            ctx.fabric.carry_serial(),
            after_receipt,
            "a retirement moves it"
        );

        // The Commit's reading raises the fence: a post waits for it —
        // past any test value, so only the lift can answer it.
        ctx.fabric.set_fence_wait(aterm_spec::wait::MUST_HAPPEN);
        let fenced = ctx.fabric.fence_hold(true);
        assert_eq!(fenced, ctx.fabric.carry_serial());
        let before = ctx.fabric.fence_waits();
        let poster = {
            let ctx = ctx.clone();
            std::thread::spawn(move || cmd_post(&ctx, "to=@s-peer kind=note two", None))
        };
        assert_parked_on_the_fence(
            &ctx.fabric,
            before,
            || poster.is_finished(),
            "a post under the fence is not answered",
        );
        assert_eq!(ctx.fabric.carry_serial(), fenced, "and it moved nothing");
        ctx.fabric.lift_hold_fence();
        assert_eq!(
            poster.join().unwrap(),
            "OK 2\n",
            "it lands once the attempt stands down"
        );
    });
}

/// FINDING 8's GUARD, for a successor whose producer predates the id carry:
/// an `await inbox since=` above every id this session has handed out
/// names no row of this process's, and is refused at once — never parked
/// over unread mail until `OK timeout`.
///
/// FAILS WITHOUT THE FIX: the wait parks its whole timeout and answers `OK
/// timeout` with rows 1 and 2 unread.
#[test]
fn an_inbox_anchor_this_process_never_minted_is_refused() {
    with_link(|| {
        let store = new_store();
        let (sid, ctx) = registered(&store, 1);
        deliver(&store, &sid, 10, "h-andrew", "task", "one");
        deliver(&store, &sid, 11, "h-andrew", "task", "two");
        // A ten-minute timeout against a minute's hang detector: a wait
        // that parks its timeout cannot pass on a loaded machine's luck.
        let started = std::time::Instant::now();
        assert_eq!(
            cmd_await_inbox(&ctx, &["since=37"], 600_000),
            "ERR bad since high=2\n"
        );
        assert!(
            started.elapsed() < aterm_spec::wait::MUST_HAPPEN,
            "refused at once, not parked: {:?}",
            started.elapsed()
        );
        // NEGATIVE CONTROL: an anchor it minted still waits and latches.
        assert_eq!(cmd_await_inbox(&ctx, &["since=1"], 50), "OK inbox 2\n");
        assert_eq!(cmd_await_inbox(&ctx, &["since=2"], 50), "OK timeout\n");
    });
}

/// ROUND SEVEN'S REVIEW, ITEM 1: THE MAIL BUDGET CHARGES WHAT TOML WRITES,
/// AND IT HOLDS THE RECEIPTS TOO. A post of ANSI colour codes escapes to up
/// to six bytes per byte in the manifest, so it is charged that — a budget
/// its RAW length fits leaves it uncarried, said — and owed receipts are
/// charged against the same budget, counted when they do not fit.
///
/// FAILS WITHOUT THE FIX: the post was charged its raw length and carried,
/// and every receipt rode with no budget at all.
#[test]
fn the_mail_budget_charges_what_toml_writes_and_holds_the_receipts() {
    with_link(|| {
        let store = new_store();
        let (sid, ctx) = registered(&store, 1);
        let generation = next_bridge_generation();
        bridge_attached(generation);
        assert_eq!(
            cmd_link(
                &store,
                &format!("log {}.0", "aa".repeat(16)),
                Some(generation)
            ),
            "OK\n"
        );
        let coloured = "\u{1b}[31mred\u{1b}[0m \"q\" \\ ".repeat(20);
        assert_eq!(
            cmd_post(
                &ctx,
                "to=@s-peer kind=note",
                Some(coloured.clone().into_bytes())
            ),
            "OK 1\n"
        );
        let raw = ctx.fabric.with_inbox_for_test(|inbox| {
            let p = &inbox.posts[0];
            p.body.len() + p.to.len() + p.kind.len()
        });
        let mut left = raw + CARRY_ROW_OVERHEAD;
        let carry = ctx.fabric.handoff_carry(&mut left);
        assert_eq!(carry.uncarried, [1], "the escaped cost does not fit");
        assert!(carry.posts.is_empty());
        // The charge is an upper bound on what the TOML writer spends.
        let mut roomy = CARRY_MAIL_BYTES;
        let carried = ctx.fabric.handoff_carry(&mut roomy);
        assert_eq!(carried.posts[0].body, coloured);
        let written = aterm_toml::to_string(&carried).unwrap();
        assert!(
            written.len() <= carried.rows_cost(),
            "{} written, {} charged",
            written.len(),
            carried.rows_cost()
        );
        assert!(toml_cost(&coloured) > coloured.len() * 3 / 2);

        // Receipts share the budget.
        deliver(&store, &sid, 10, "s-peer@n-lab", "ask", "one");
        deliver(&store, &sid, 11, "s-peer@n-lab", "ask", "two");
        assert_eq!(cmd_inbox_seen(&ctx, "1 handled"), "OK seen=1\n");
        assert_eq!(cmd_inbox_seen(&ctx, "2 handled"), "OK seen=2\n");
        let mut none = 0;
        let carry = ctx.fabric.handoff_carry(&mut none);
        assert!(carry.receipts.is_empty());
        assert_eq!(carry.receipts_uncarried, 2, "counted, never silent");
        let mut one = ReceiptCarry {
            rid: 1,
            off: 10,
            verdict: "handled".to_string(),
            kind: "ask".to_string(),
            from: "s-peer@n-lab".to_string(),
        }
        .cost();
        let carry = ctx.fabric.handoff_carry(&mut one);
        assert_eq!(carry.receipts.len(), 1);
        assert_eq!(carry.receipts_uncarried, 1);
        assert_eq!(one, 0, "the budget is spent");
        // NEGATIVE CONTROL: a roomy budget carries everything.
        let mut roomy = CARRY_MAIL_BYTES;
        let carry = ctx.fabric.handoff_carry(&mut roomy);
        assert_eq!((carry.posts.len(), carry.receipts.len()), (1, 2));
        assert_eq!(carry.receipts_uncarried, 0);
        assert!(carry.uncarried.is_empty());
    });
}

/// ROUND SEVEN'S REVIEW, ITEM 1: SHEDDING THE CARRY keeps the counters and
/// retires every post dead in the successor, `not-carried`, said; the
/// receipts are counted. What the manifest writer does before it would
/// refuse an update over its cap.
#[test]
fn a_shed_carry_keeps_its_counters_and_says_what_it_dropped() {
    with_link(|| {
        let store = new_store();
        let (sid, ctx) = registered(&store, 1);
        assert_eq!(cmd_post(&ctx, "to=@s-peer kind=note one", None), "OK 1\n");
        assert_eq!(cmd_post(&ctx, "to=@s-peer kind=note two", None), "OK 2\n");
        deliver(&store, &sid, 10, "s-peer@n-lab", "task", "t");
        assert_eq!(cmd_inbox_seen(&ctx, "1 handled"), "OK seen=1\n");
        let mut roomy = CARRY_MAIL_BYTES;
        let mut carry = ctx.fabric.handoff_carry(&mut roomy);
        assert!(carry.rows_cost() > 0);
        assert!(carry.shed());
        assert!(!carry.shed(), "nothing left to shed");
        assert_eq!(carry.rows_cost(), 0);
        assert_eq!(carry.uncarried, [1, 2]);
        assert_eq!(carry.receipts_uncarried, 1);
        assert_eq!(
            (carry.last_msg_id, carry.last_post_id, carry.last_receipt_id),
            (1 + INBOX_ID_RESERVE, 2, 1),
            "the counters stay"
        );
        let fresh = test_handle(1);
        fresh.ctx.fabric.seed_carry(&carry, &fresh.ctx.timeline);
        let rows: Vec<String> = fresh
            .ctx
            .timeline
            .lock()
            .unwrap()
            .since(None)
            .map(|e| e.payload.clone())
            .collect();
        assert_eq!(
            rows,
            [
                format!("1 off=- reason={NOT_CARRIED}"),
                format!("2 off=- reason={NOT_CARRIED}")
            ]
        );
        assert_eq!(
            cmd_post(&fresh.ctx, "to=@s-peer kind=note three", None),
            "OK 3\n"
        );
    });
}

/// ROUND SEVEN'S REVIEW OF GROUP J, ITEM 2: INBOUND MAIL IN THE OVERLAP
/// NEVER VETOES THE UPDATE. A row delivered after the park's draw moves no
/// serial the Commit compares, and one delivered under the Commit's fence
/// is answered at once: the manifest carried the last id plus
/// [`INBOX_ID_RESERVE`], so both are numbered below every id the successor
/// hands out. Only a process that outruns the reserve stands its Commit
/// down ([`SessionFabric::past_handoff_ceiling`]). NEGATIVE CONTROL: the
/// last row inside the reserve does not.
///
/// FAILS WITHOUT THE FIX: the row's id was in the carry serial, so the
/// first delivery after the draw revoked the attempt, and one under the
/// fence waited.
#[cfg(unix)] // the update fence (`lift_hold_fence`) is unix-only
#[test]
fn mail_in_the_overlap_is_numbered_below_the_successor_and_vetoes_nothing() {
    with_link(|| {
        let store = new_store();
        let (sid, ctx) = registered(&store, 1);
        assert_eq!(
            deliver(&store, &sid, 10, "h-andrew", "task", "one"),
            "OK 1\n"
        );
        let parked = ctx.fabric.carry_serial();
        let carried = ctx.fabric.handoff_carry(&mut CARRY_MAIL_BYTES.clone());
        assert_eq!(carried.last_msg_id, 1 + INBOX_ID_RESERVE);
        assert_eq!(
            deliver(&store, &sid, 11, "h-andrew", "task", "two"),
            "OK 2\n"
        );
        assert_eq!(ctx.fabric.carry_serial(), parked, "a new row moves nothing");
        let fenced = ctx.fabric.fence_hold(true);
        assert_eq!(fenced, parked);
        assert_eq!(
            deliver(&store, &sid, 12, "s-peer@n-lab", "note", "three"),
            "OK 3\n",
            "under the fence, inside the reserve: answered at once"
        );
        assert!(!ctx.fabric.past_handoff_ceiling());
        ctx.fabric.lift_hold_fence();

        // The last id inside the reserve is still no veto; one past it is.
        ctx.fabric
            .with_inbox_for_test(|i| i.last_msg_id = carried.last_msg_id - 1);
        assert_eq!(
            deliver(&store, &sid, 13, "h-andrew", "task", "four"),
            format!("OK {}\n", carried.last_msg_id)
        );
        assert!(!ctx.fabric.past_handoff_ceiling(), "at the ceiling: fine");
        assert_eq!(
            deliver(&store, &sid, 14, "s-peer@n-lab", "note", "five"),
            format!("OK {}\n", carried.last_msg_id + 1)
        );
        assert!(
            ctx.fabric.past_handoff_ceiling(),
            "past it, the Commit stands down"
        );
    });
}

/// The notice cadence this test process waits on: the production one
/// read through the timing value.
#[cfg(unix)]
fn notice_every() -> std::time::Duration {
    aterm_gui_timing::current().of(MSG_ROOM_NOTICE_EVERY)
}

/// Watch deliveries waiting on the Commit's fence until each of `sids`
/// has logged a WARN whose own `waited` is past `bound` — those sessions'
/// [`SessionFabric::fence_wait`], the longest any fenced path on them waits
/// before a `hold`, a post and a retirement answer [`HOLD_BUSY`] — failing
/// the moment one answers. A watch that ended inside the bound would pass a
/// wait that times out into `ERR busy` (round seven's review of group J,
/// open problem 202). The minute is a hang detector, never a budget.
#[cfg(unix)] // the update fence (`lift_hold_fence`) is unix-only
fn still_waiting_past_every_fenced_bound(
    waiters: Vec<std::thread::JoinHandle<String>>,
    sids: &[&str],
    bound: std::time::Duration,
) -> Vec<std::thread::JoinHandle<String>> {
    // The WARN prints `waited` to a tenth of a second, so a printed value a
    // tenth past the bound is past it whatever the rounding.
    let past = bound.as_secs_f64() + 0.1;
    let waited_past = |sid: &str| {
        crate::test_log::containing(&format!("for session {sid} has waited "))
            .iter()
            .filter(|n| n.level == aterm_log::Level::Warn)
            .filter_map(|n| {
                let (_, rest) = n.message.split_once(" has waited ")?;
                rest.split_once(" s ")?.0.parse::<f64>().ok()
            })
            .any(|waited| waited >= past)
    };
    let watched = aterm_spec::wait::within(aterm_spec::wait::MUST_HAPPEN, notice_every(), || {
        if let Some(i) = waiters
            .iter()
            .position(std::thread::JoinHandle::is_finished)
        {
            return Some(Err(i));
        }
        sids.iter().all(|sid| waited_past(sid)).then_some(Ok(()))
    });
    match watched {
        Some(Ok(())) => waiters,
        Some(Err(i)) => {
            let reply = waiters
                .into_iter()
                .nth(i)
                .map(std::thread::JoinHandle::join);
            panic!("fenced delivery {i} was answered under the fence: {reply:?}");
        }
        None => panic!("no WARN past the {bound:?} bound for every one of {sids:?}"),
    }
}

/// ROUND SEVEN'S REVIEW OF GROUP J, ITEM 1, AND OPEN PROBLEMS 201-203: A
/// DELIVERY THAT MUST NOT BE ANSWERED UNDER THE COMMIT'S FENCE WAITS FOR
/// THE ATTEMPT, for as long as the fence stands, and is never answered an
/// `ERR`: the bridge books any non-`OK` reply as a verdict (the
/// record refused to its sender, a broadcast dropped) and commits its
/// cursor past it. Two such deliveries, on two sessions: an addressed row
/// that would pass the carried id, and a broadcast well inside it. Both
/// still wait past [`SessionFabric::fence_wait`] — the bound every other
/// fenced path answers `ERR busy` at, and the only door to it — until each
/// session's own WARN (a notice in `aterm.log`, not on the GUI's stderr)
/// says it has waited past it. The bound is the sessions' own
/// ([`SessionFabric::set_fence_wait`]), so the watch is a few notice rounds
/// long. Once the attempt stands down each lands under the next id.
/// NEGATIVE CONTROL: a redelivery of the same offset answers the id it
/// first got.
///
/// FAILS WITHOUT THE FIX: the first pass answered the row `ERR busy
/// update-committing` after the fence wait, which the bridge refused to
/// the sender for good; the broadcast was answered `OK` at once, and lost
/// with the `_exit`; and the wait's notice went to stderr, which nobody
/// reads. A watch that ends inside the bound passes against the first pass
/// as well (open problem 202).
///
/// NO LINK SECTION ([`with_link_reset`]), unlike its siblings: nothing here
/// reads or writes the bridge link. The fence, the carry and both waits are
/// the sessions' own state, the deliveries speak for no bridge (no lane
/// generation: [`link_evidence`] returns before it writes), and `deliver`
/// outside the section is the schedule-proof shape 27 `inbox_hold` tests
/// already use ([`touched_contains`]); inside it, the wait would hold the
/// section's mutex and park every link test the runner started meanwhile.
#[cfg(unix)] // the update fence (`lift_hold_fence`) is unix-only
#[test]
fn a_fenced_delivery_waits_past_every_bound_and_is_never_refused() {
    crate::test_log::install();
    let store = new_store();
    let (sid, ctx) = registered(&store, 1);
    let (bsid, bctx) = registered(&store, 2);
    // A broadcast reaches only a session that asked for its topic.
    assert_eq!(
        bctx.fabric.topic_add("news", "head", &bctx.timeline),
        Ok(true)
    );
    for s in [&sid, &bsid] {
        assert_eq!(deliver(&store, s, 10, "h-andrew", "task", "one"), "OK 1\n");
    }
    let carried = ctx.fabric.handoff_carry(&mut CARRY_MAIL_BYTES.clone());
    let _ = bctx.fabric.handoff_carry(&mut CARRY_MAIL_BYTES.clone());
    ctx.fabric
        .with_inbox_for_test(|i| i.last_msg_id = carried.last_msg_id);
    // Every fenced path on these sessions answers busy past a bound of a
    // few notice rounds, not the real ten seconds.
    let bound = notice_every() * 5;
    for fabric in [&ctx.fabric, &bctx.fabric] {
        fabric.set_fence_wait(bound);
    }
    let _ = ctx.fabric.fence_hold(true);
    let _ = bctx.fabric.fence_hold(true);
    let row = {
        let store = store.clone();
        let sid = sid.clone();
        std::thread::spawn(move || deliver(&store, &sid, 11, "s-peer@n-lab", "note", "two"))
    };
    let shout = {
        let store = store.clone();
        let bsid = bsid.clone();
        std::thread::spawn(move || broadcast(&store, &bsid, 11, "news", "shout"))
    };
    let mut waiters =
        still_waiting_past_every_fenced_bound(vec![row, shout], &[&sid, &bsid], bound).into_iter();
    let (row, shout) = (waiters.next().unwrap(), waiters.next().unwrap());
    assert!(!ctx.fabric.past_handoff_ceiling(), "the row minted nothing");
    assert_eq!(
        bctx.fabric.with_inbox_for_test(|i| i.last_msg_id),
        1,
        "nor did the broadcast"
    );
    ctx.fabric.lift_hold_fence();
    bctx.fabric.lift_hold_fence();
    let reply = row.join().unwrap();
    assert_eq!(reply, format!("OK {}\n", carried.last_msg_id + 1));
    assert_eq!(shout.join().unwrap(), "OK 2\n");
    assert_eq!(
        deliver(&store, &sid, 11, "s-peer@n-lab", "note", "two"),
        reply,
        "a redelivery of the same offset answers the same id"
    );
    assert_eq!(broadcast(&store, &bsid, 11, "news", "shout"), "OK 2\n");
}

/// ROUND SEVEN'S REVIEW, ITEM 5: THE FENCE HOLDS A RETIREMENT BACK ONLY FOR
/// A SUCCESSOR THAT READS THE CARRY. An older one restarts the post ids
/// under the same sid, so a retirement left unanswered keeps a sequence
/// pin its own post would be deduplicated into; for it the retirement is
/// answered, as every earlier build answered it. NEGATIVE CONTROL: for a
/// successor that reads the carry, it waits.
#[cfg(unix)] // the update fence (`lift_hold_fence`) is unix-only
#[test]
fn the_fence_holds_a_retirement_back_only_for_a_successor_that_reads_the_carry() {
    with_link(|| {
        let store = new_store();
        let (sid, ctx) = registered(&store, 1);
        assert_eq!(cmd_post(&ctx, "to=@s-peer kind=note one", None), "OK 1\n");
        assert_eq!(cmd_post(&ctx, "to=@s-peer kind=note two", None), "OK 2\n");
        deliver(&store, &sid, 10, "s-peer@n-lab", "ask", "q");
        assert_eq!(cmd_inbox_seen(&ctx, "1 handled"), "OK seen=1\n");

        let _ = ctx.fabric.fence_hold(false);
        assert_eq!(
            cmd_outbox_verb(&store, &format!("sent {sid} 1 off=70")),
            "OK\n",
            "answered at once for an older successor"
        );
        assert_eq!(
            cmd_deliver(&store, &format!("{sid} receipt=1 off=71")),
            "OK\n"
        );
        ctx.fabric.lift_hold_fence();

        ctx.fabric.set_fence_wait(aterm_spec::wait::MUST_HAPPEN);
        let _ = ctx.fabric.fence_hold(true);
        let before = ctx.fabric.fence_waits();
        let bridge = {
            let store = store.clone();
            let sid = sid.clone();
            std::thread::spawn(move || cmd_outbox_verb(&store, &format!("sent {sid} 2 off=72")))
        };
        assert_parked_on_the_fence(
            &ctx.fabric,
            before,
            || bridge.is_finished(),
            "held back for a successor that carries it",
        );
        ctx.fabric.lift_hold_fence();
        assert_eq!(bridge.join().unwrap(), "OK\n");
    });
}
