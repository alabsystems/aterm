// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for session mailbox state ([`FabricCarry`]) carried across a seamless update.

use super::*;
use crate::SessionCtx;
use crate::session_store::{SessionHandoff, Store, new_store, test_handle};

use super::with_link_reset as with_link;

fn registered(store: &Store, local_id: u64) -> (String, std::sync::Arc<SessionCtx>) {
    let h = test_handle(local_id);
    let sid = h.sid.as_str().to_string();
    let ctx = h.ctx.clone();
    store.write().unwrap_or_else(|p| p.into_inner()).register(h);
    (sid, ctx)
}

fn deliver(store: &Store, sid: &str, off: u64, from: &str, kind: &str, text: &str) -> String {
    cmd_deliver(
        store,
        &format!("{sid} off={off} from={from} kind={kind} trust=agent text={text}"),
    )
}

fn broadcast(store: &Store, sid: &str, off: u64, topic: &str, text: &str) -> String {
    cmd_deliver(
        store,
        &format!(
            "{sid} off={off} from=s-peer@n-lab kind=note trust=agent topic={topic} text={text}"
        ),
    )
}

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

        assert!(cmd_topic(&ctx, "add news").contains("added=1"));
        assert_eq!(broadcast(&store, &sid, 10, "news", "b"), "OK 1\n");
        assert!(cmd_topic(&ctx, "drop news").contains("dropped=1"));
        assert!(cmd_topic(&ctx, "add news").contains("added=1"));
        assert_eq!(broadcast(&store, &sid, 11, "news", "b"), "OK 2\n");
        assert_eq!(carried(&ctx), (2, 1), "only the row taken since the add");

        assert!(cmd_topic(&pctx, "add news").contains("added=1"));
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
        assert_eq!(cmd_post(&ctx, "to=@s-peer kind=note gone", None), "OK 1\n");
        assert_eq!(
            cmd_post(&ctx, "to=@s-peer kind=note waiting", None),
            "OK 2\n"
        );
        assert_eq!(
            cmd_outbox_verb(&store, &format!("sent {sid} 1 off=70")),
            "OK\n"
        );
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
        assert_eq!(
            cmd_inbox_seen(new_ctx, "1 handled"),
            "ERR no such message\n"
        );
    });
}

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
/// round).
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
            cmd_topic(&new_ctx, "ls"),
            "OK 1 seen=0\ntopic builds since=@5\n",
            "the opt-in is listed, so the bridge keeps its cursor"
        );
        let rows: Vec<(String, String)> = new_ctx
            .timeline
            .lock()
            .unwrap()
            .since(None)
            .map(|e| (e.kind.to_string(), e.payload.to_string()))
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
/// then counts on from it.
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
        let (_store, older, _) = recover(&older);
        let header = cmd_inbox(&older, "--peek");
        assert!(header.contains(" dropped=0 "), "{header}");
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
/// successor would not carry.
#[cfg(unix)]
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

#[test]
fn an_inbox_anchor_this_process_never_minted_is_refused() {
    with_link(|| {
        let store = new_store();
        let (sid, ctx) = registered(&store, 1);
        deliver(&store, &sid, 10, "h-andrew", "task", "one");
        deliver(&store, &sid, 11, "h-andrew", "task", "two");
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
        assert_eq!(cmd_await_inbox(&ctx, &["since=1"], 50), "OK inbox 2\n");
        assert_eq!(cmd_await_inbox(&ctx, &["since=2"], 50), "OK timeout\n");
    });
}

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
        let mut roomy = CARRY_MAIL_BYTES;
        let carry = ctx.fabric.handoff_carry(&mut roomy);
        assert_eq!((carry.posts.len(), carry.receipts.len()), (1, 2));
        assert_eq!(carry.receipts_uncarried, 0);
        assert!(carry.uncarried.is_empty());
    });
}

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
            .map(|e| e.payload.to_string())
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

/// INBOUND MAIL IN THE OVERLAP
/// NEVER VETOES THE UPDATE.
#[cfg(unix)]
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
/// the moment one answers.
#[cfg(unix)]
fn still_waiting_past_every_fenced_bound(
    waiters: Vec<std::thread::JoinHandle<String>>,
    sids: &[&str],
    bound: std::time::Duration,
) -> Vec<std::thread::JoinHandle<String>> {
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

/// A
/// DELIVERY THAT MUST NOT BE ANSWERED UNDER THE COMMIT'S FENCE WAITS FOR
/// THE ATTEMPT, for as long as the fence stands, and is never answered an
/// `ERR`: the bridge books any non-`OK` reply as a verdict (the
/// record refused to its sender, a broadcast dropped) and commits its
/// cursor past it.
#[cfg(unix)]
#[test]
fn a_fenced_delivery_waits_past_every_bound_and_is_never_refused() {
    crate::test_log::install();
    let store = new_store();
    let (sid, ctx) = registered(&store, 1);
    let (bsid, bctx) = registered(&store, 2);
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

/// THE FENCE HOLDS A RETIREMENT BACK ONLY FOR
/// A SUCCESSOR THAT READS THE CARRY.
#[cfg(unix)]
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
