// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Every change to what a session's keeper carry reads rings its condvar, and so tells
//! [`on_carry_moved`]'s listener ([`InboxChanged`]): counted on this thread, exactly, with a later
//! change as each count's control.

use super::inbox_hold::{deliver, registered_as};
use super::*;
use crate::test_support::{Store, new_store};

fn rings() -> u64 {
    CARRY_RINGS.with(std::cell::Cell::get)
}

/// A peer agent on another Mac, as a well-formed tab ID.
const PEER: &str = "t0000000001@n-lab";

/// The bridge delivers `PEER`'s broadcast on `topic` at `off` to session `sid`.
fn broadcast(store: &Store, sid: &str, off: u64, topic: &str) -> String {
    cmd_deliver(
        store,
        &format!("{sid} off={off} from={PEER} kind=note trust=agent topic={topic} text=b"),
    )
}

/// A broadcast the sender's quota refuses moves `dropped=`, which the carry holds: it rings.
#[test]
fn a_quota_refusal_that_counts_a_drop_rings() {
    let store = new_store();
    let (sid, ctx) = registered_as(&store, 1);
    assert_eq!(
        ctx.fabric.topic_add("news", "head", &ctx.timeline),
        Ok(true)
    );
    let quota = u64::try_from(SENDER_QUOTA).expect("a quota");
    for off in 0..quota {
        assert!(broadcast(&store, &sid, 5000 + off, "news").starts_with("OK "));
    }
    let carried = ctx.fabric.keeper_carry().dropped;
    let before = rings();
    assert_eq!(broadcast(&store, &sid, 5000 + quota, "news"), "ERR quota\n");
    assert_eq!(ctx.fabric.keeper_carry().dropped, carried + 1, "premise");
    assert_eq!(rings(), before + 1, "the drop rings");
    let before = rings();
    assert_eq!(
        ctx.fabric.keeper_carry().dropped,
        carried + 1,
        "a read of the carry"
    );
    assert_eq!(
        broadcast(&store, &sid, 5000 + quota + 1, "news"),
        "ERR quota\n"
    );
    assert_eq!(
        rings(),
        before + 1,
        "the read rang nothing; the next drop rings once"
    );
}

/// The bridge retiring an owed receipt moves the carry's `owed`: it rings; retiring one no longer
/// owed moves nothing and rings nothing.
#[test]
fn retiring_an_owed_receipt_rings_and_retiring_it_again_does_not() {
    with_link_reset(|| {
        let store = new_store();
        let (sid, ctx) = registered_as(&store, 1);
        deliver(&store, &sid, 10, PEER, "ask", "one");
        deliver(&store, &sid, 11, PEER, "ask", "two");
        assert_eq!(cmd_inbox_seen(&ctx, "2"), "OK seen=2\n");
        assert_eq!(cmd_inbox_seen(&ctx, "1 handled"), "OK seen=2\n");
        assert_eq!(cmd_inbox_seen(&ctx, "2 handled"), "OK seen=2\n");
        assert_eq!(ctx.fabric.keeper_carry().owed, 2, "premise");
        let before = rings();
        assert_eq!(
            cmd_deliver(&store, &format!("{sid} receipt=1 off=90")),
            "OK\n"
        );
        assert_eq!(ctx.fabric.keeper_carry().owed, 1);
        assert_eq!(rings(), before + 1, "the retirement rings");
        let before = rings();
        assert_eq!(
            cmd_deliver(&store, &format!("{sid} receipt=1 off=90")),
            "OK\n",
            "a retried retirement"
        );
        assert_eq!(
            cmd_deliver(&store, &format!("{sid} receipt=2 off=91")),
            "OK\n"
        );
        assert_eq!(ctx.fabric.keeper_carry().owed, 0);
        assert_eq!(
            rings(),
            before + 1,
            "the retried one moved nothing and rang nothing; the next one rings once"
        );
    });
}

/// A topic added or dropped moves the carry's `topics`, whichever way it is made: each tells
/// the listener once, and an add of one already there or a drop of one not there tells nothing.
#[test]
fn a_topic_change_tells_the_listener_and_a_no_op_does_not() {
    let store = new_store();
    let (_, ctx) = registered_as(&store, 1);
    let before = rings();
    assert_eq!(
        ctx.fabric.topic_add("news", "head", &ctx.timeline),
        Ok(true)
    );
    assert_eq!(rings(), before + 1, "an add");
    let before = rings();
    assert_eq!(
        ctx.fabric.topic_add("news", "head", &ctx.timeline),
        Ok(false),
        "already there"
    );
    assert!(!ctx.fabric.topic_drop("builds", &ctx.timeline), "not there");
    assert!(ctx.fabric.topic_drop("news", &ctx.timeline));
    assert_eq!(
        rings(),
        before + 1,
        "the no-ops told nothing; the drop tells once"
    );
    let before = rings();
    ctx.fabric
        .set_topics(parse_topics(&["builds head".to_string()]));
    assert_eq!(ctx.fabric.keeper_carry().topics, ["builds head"], "premise");
    assert_eq!(rings(), before + 1, "a whole set replaced");
}

/// The listener hears each ring once.
#[test]
fn the_listener_hears_every_ring() {
    thread_local! {
        static HEARD: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    }
    fn heard() {
        HEARD.with(|heard| heard.set(heard.get() + 1));
    }
    // The one listener this test binary sets.
    on_carry_moved(heard);
    let fabric = SessionFabric::default();
    let before = HEARD.with(std::cell::Cell::get);
    fabric.close();
    fabric.wake_waiters_for_test();
    assert_eq!(HEARD.with(std::cell::Cell::get), before + 2);
}
